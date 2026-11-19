//! Codex in chat mode: the core speaks `codex app-server`'s JSON-RPC on stdio, one message per
//! line, and maps it to the same messages the Claude sidecar sends. Checked against the
//! protocol `codex app-server generate-ts` prints for Codex CLI 0.158.0.

use std::collections::{HashMap, VecDeque};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{FromAgent, ModelInfo, Question, QuestionOption, Reply, RequestId, TurnError};
use crate::AccountInfo;

/// What the core knows of its connection to one app server. The chat's lock guards it.
pub(super) struct AppServer {
    next_id: u64,
    // The method of each request the core sent that has no reply yet, by id.
    sent: HashMap<u64, &'static str>,
    // The thread to go on with, for `thread/resume`. None starts a new one.
    resume: Option<String>,
    thread: Thread,
    // Messages wait here until the thread starts and the turn before ends, so each gets its own
    // turn and none steers another.
    queued: VecDeque<String>,
    // The id of the `turn/start` request whose turn has not ended yet.
    turn_start: Option<u64>,
    // That turn has not sent `turn/started` yet, so its message has not reached the agent.
    turn_starting: bool,
    // The running turn, which `turn/interrupt` names.
    turn: Option<String>,
    // What `set_model` chose, for every `turn/start` from then on. None leaves Codex's own.
    model: Option<String>,
    effort: Option<String>,
}

enum Thread {
    Starting,
    Started {
        id: String,
    },
    /// Starting it failed, which dropped the waiting messages. The next message tries again.
    Failed,
}

/// How to reply to one approval request or question.
#[derive(Debug)]
pub(super) struct ReplyTo {
    // The JSON-RPC id, as the server gave it.
    id: Value,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    /// Commands and file changes.
    Decision,
    /// The permissions the agent asked for.
    Permissions(Value),
    /// The question ids, in order.
    Answers(Vec<String>),
}

impl AppServer {
    pub(super) fn new(resume: Option<String>) -> Self {
        Self {
            next_id: 0,
            sent: HashMap::new(),
            resume,
            thread: Thread::Starting,
            queued: VecDeque::new(),
            turn_start: None,
            turn_starting: false,
            turn: None,
            model: None,
            effort: None,
        }
    }

    /// The first line, which the core writes before any other.
    pub(super) fn opening(&mut self) -> Vec<u8> {
        let client = json!({"name": "grove", "version": env!("CARGO_PKG_VERSION")});
        self.request("initialize", json!({"clientInfo": client}))
    }

    /// The line to write for the user's message, or None while it waits for the thread or for
    /// the turn before to end.
    pub(super) fn user_message(&mut self, text: &str) -> Option<Vec<u8>> {
        self.queued.push_back(text.to_owned());
        if let Thread::Failed = self.thread {
            self.thread = Thread::Starting;
            return Some(self.start_thread());
        }
        self.next_turn()
    }

    /// Starts a turn for the next waiting message, when the thread runs and no turn does.
    fn next_turn(&mut self) -> Option<Vec<u8>> {
        let Thread::Started { id } = &self.thread else {
            return None;
        };
        if self.turn_start.is_some() {
            return None;
        }
        let mut params = json!({"threadId": id, "input": text_input(&self.queued.pop_front()?)});
        if let Some(model) = &self.model {
            params["model"] = model.as_str().into();
        }
        if let Some(effort) = &self.effort {
            params["effort"] = effort.as_str().into();
        }
        self.turn_start = Some(self.next_id);
        self.turn_starting = true;
        Some(self.request("turn/start", params))
    }

    /// How many of the user's messages have no turn started on them yet: those waiting here, and
    /// the one whose turn has not sent `turn/started`. A message whose turn failed to start is
    /// not counted, as its failed turn shows in the conversation.
    pub(super) fn unstarted(&self) -> usize {
        self.queued.len() + usize::from(self.turn_starting)
    }

    /// The model, and the effort when given, for the turns that start from now on.
    pub(super) fn set_model(&mut self, model: &str, effort: Option<&str>) {
        self.model = Some(model.to_owned());
        if let Some(effort) = effort {
            self.effort = Some(effort.to_owned());
        }
    }

    /// The line that stops the running turn, or None when no turn runs.
    pub(super) fn interrupt(&mut self) -> Option<Vec<u8>> {
        let Thread::Started { id } = &self.thread else {
            return None;
        };
        let params = json!({"threadId": id, "turnId": self.turn.as_ref()?});
        Some(self.request("turn/interrupt", params))
    }

    /// What one line from the server means, and the lines to write back.
    pub(super) fn read(&mut self, line: &[u8]) -> (Vec<FromAgent>, Vec<Vec<u8>>) {
        let mut writes = Vec::new();
        let Ok(message) = serde_json::from_slice::<Message>(line) else {
            return (Vec::new(), writes);
        };
        let incoming = match message {
            Message {
                id: Some(id),
                method: Some(method),
                params,
                ..
            } => match server_request(&method, &id, params) {
                Ok(request) => vec![request],
                Err((code, message)) => {
                    writes.push(line_of(
                        json!({"id": id, "error": {"code": code, "message": message}}),
                    ));
                    Vec::new()
                }
            },
            Message {
                id: Some(id),
                method: None,
                result,
                error,
                ..
            } => self.response(&id, result, error, &mut writes),
            Message {
                id: None,
                method: Some(method),
                params,
                ..
            } => self.notification(&method, params, &mut writes),
            _ => Vec::new(),
        };
        (incoming, writes)
    }

    fn request(&mut self, method: &'static str, params: Value) -> Vec<u8> {
        let id = self.next_id;
        self.next_id += 1;
        self.sent.insert(id, method);
        line_of(json!({"id": id, "method": method, "params": params}))
    }

    fn start_thread(&mut self) -> Vec<u8> {
        match self.resume.clone() {
            Some(id) => self.request("thread/resume", json!({"threadId": id})),
            None => self.request("thread/start", json!({})),
        }
    }

    fn response(
        &mut self,
        id: &Value,
        result: Option<Value>,
        error: Option<RpcError>,
        writes: &mut Vec<Vec<u8>>,
    ) -> Vec<FromAgent> {
        let Some((request, method)) = id
            .as_u64()
            .and_then(|id| Some((id, self.sent.remove(&id)?)))
        else {
            return Vec::new();
        };
        // The reply to the request of the turn that has not ended yet.
        let current_turn = self.turn_start == Some(request);
        let result = match (result, error) {
            (_, Some(error)) => {
                if current_turn {
                    self.turn_start = None;
                    self.turn_starting = false;
                    writes.extend(self.next_turn());
                }
                return self.failed(method, error.message);
            }
            (Some(result), None) => result,
            (None, None) => Value::Null,
        };
        match method {
            "initialize" => {
                writes.push(line_of(json!({"method": "initialized"})));
                writes.push(self.request("account/read", json!({})));
                writes.push(self.start_thread());
                // One page holds them all: the picker's list is short.
                writes.push(self.request("model/list", json!({})));
                Vec::new()
            }
            "model/list" => vec![FromAgent::Models {
                models: models(&result["data"]),
            }],
            "account/read" => vec![FromAgent::Account {
                account: account(&result["account"]),
            }],
            "thread/start" | "thread/resume" => {
                let Some(thread) = result["thread"]["id"].as_str() else {
                    return self.failed(method, "the reply has no thread id".into());
                };
                self.thread = Thread::Started {
                    id: thread.to_owned(),
                };
                writes.extend(self.next_turn());
                let thread = FromAgent::Thread {
                    id: thread.to_owned(),
                };
                // Only a resume's reply holds the turns so far, the same ones `thread/read`
                // returns. Read here, no turn can start before them.
                if method == "thread/resume" {
                    let messages = history(&result["thread"]["turns"]);
                    return vec![FromAgent::History { messages }, thread];
                }
                vec![thread]
            }
            "turn/start" => {
                if current_turn && self.turn.is_none() {
                    self.turn = result["turn"]["id"].as_str().map(str::to_owned);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A request of the core's that failed. A turn that never started ends failed.
    fn failed(&mut self, method: &'static str, message: String) -> Vec<FromAgent> {
        match method {
            // The account stays unknown, and an interrupt that came too late changes nothing.
            "account/read" | "turn/interrupt" => return Vec::new(),
            // The picker offers no models, and Codex uses its own.
            "model/list" => return vec![FromAgent::Models { models: Vec::new() }],
            "initialize" | "thread/start" | "thread/resume" => {
                self.thread = Thread::Failed;
                self.queued.clear();
            }
            _ => {}
        }
        let failed = FromAgent::TurnEnd {
            interrupted: false,
            error: Some(TurnError {
                kind: method.to_owned(),
                message,
            }),
            time_unix_ms: None,
        };
        // A resume that failed read no history, and the conversation shows why.
        if method == "thread/resume" {
            return vec![
                FromAgent::History {
                    messages: Vec::new(),
                },
                failed,
            ];
        }
        vec![failed]
    }

    fn notification(
        &mut self,
        method: &str,
        params: Value,
        writes: &mut Vec<Vec<u8>>,
    ) -> Vec<FromAgent> {
        match method {
            "turn/started" => {
                self.turn_starting = false;
                self.turn = params["turn"]["id"].as_str().map(str::to_owned);
                vec![FromAgent::TurnStart { time_unix_ms: None }]
            }
            "item/agentMessage/delta" => params["delta"]
                .as_str()
                .map(|text| FromAgent::TextDelta { text: text.into() })
                .into_iter()
                .collect(),
            "item/started" => tool_call(&params["item"]).into_iter().collect(),
            "item/completed" => tool_result(&params["item"]).into_iter().collect(),
            "turn/completed" => {
                self.turn = None;
                self.turn_start = None;
                self.turn_starting = false;
                writes.extend(self.next_turn());
                let turn = &params["turn"];
                let error = (turn["status"] == "failed").then(|| TurnError {
                    kind: error_kind(&turn["error"]["codexErrorInfo"]),
                    message: turn["error"]["message"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                });
                vec![FromAgent::TurnEnd {
                    interrupted: turn["status"] == "interrupted",
                    error,
                    time_unix_ms: None,
                }]
            }
            _ => Vec::new(),
        }
    }
}

impl ReplyTo {
    /// The line that answers the request with `reply`. `check_reply` has made sure it fits.
    pub(super) fn line(&self, reply: &Reply) -> Vec<u8> {
        let result = match (&self.kind, reply) {
            (Kind::Decision, Reply::AllowOnce) => json!({"decision": "accept"}),
            (Kind::Decision, Reply::AllowForSession) => json!({"decision": "acceptForSession"}),
            // Codex takes no reason with a decline.
            (Kind::Decision, _) => json!({"decision": "decline"}),
            (Kind::Permissions(asked), Reply::AllowOnce) => granted(asked, "turn"),
            (Kind::Permissions(asked), Reply::AllowForSession) => granted(asked, "session"),
            (Kind::Permissions(_), _) => json!({"permissions": {}, "scope": "turn"}),
            (Kind::Answers(ids), Reply::Answers(answers)) => {
                let answers: Map<String, Value> = ids
                    .iter()
                    .zip(answers)
                    .map(|(id, answer)| (id.clone(), json!({"answers": answer})))
                    .collect();
                json!({"answers": answers})
            }
            // Codex has no way to decline a question, so it gets no answers.
            (Kind::Answers(_), _) => json!({"answers": {}}),
        };
        line_of(json!({"id": self.id, "result": result}))
    }
}

/// Any JSON-RPC message: a request has an id and a method, a reply an id, and a notification a
/// method.
#[derive(Deserialize)]
struct Message {
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalParams {
    item_id: String,
    // Set when one item asks more than once.
    approval_id: Option<String>,
    #[serde(default)]
    permissions: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuestionParams {
    item_id: String,
    questions: Vec<CodexQuestion>,
}

#[derive(Deserialize)]
struct CodexQuestion {
    id: String,
    header: String,
    question: String,
    options: Option<Vec<QuestionOption>>,
}

/// An approval request or a question from the server, or the error to reply with when the core
/// takes no such request.
fn server_request(
    method: &str,
    id: &Value,
    params: Value,
) -> std::result::Result<FromAgent, (i64, String)> {
    let bad_params = |e: serde_json::Error| (-32602, format!("{method}: {e}"));
    let tool = match method {
        "item/commandExecution/requestApproval" => "commandExecution",
        "item/fileChange/requestApproval" => "fileChange",
        "item/permissions/requestApproval" => "permissions",
        "item/tool/requestUserInput" => {
            let asked: QuestionParams = serde_json::from_value(params).map_err(bad_params)?;
            let ids = asked.questions.iter().map(|q| q.id.clone()).collect();
            let questions = asked
                .questions
                .into_iter()
                .map(|q| Question {
                    question: q.question,
                    header: q.header,
                    options: q.options.unwrap_or_default(),
                    multi_select: false,
                })
                .collect();
            return Ok(FromAgent::QuestionRequest {
                id: RequestId(asked.item_id),
                questions,
                reply_to: Some(ReplyTo {
                    id: id.clone(),
                    kind: Kind::Answers(ids),
                }),
            });
        }
        _ => return Err((-32601, format!("grove does not handle {method}"))),
    };
    let asked: ApprovalParams = serde_json::from_value(params.clone()).map_err(bad_params)?;
    let kind = if tool == "permissions" {
        Kind::Permissions(asked.permissions)
    } else {
        Kind::Decision
    };
    Ok(FromAgent::ApprovalRequest {
        id: RequestId(asked.approval_id.unwrap_or(asked.item_id)),
        tool: tool.into(),
        input: params,
        reply_to: Some(ReplyTo {
            id: id.clone(),
            kind,
        }),
    })
}

/// The messages of a resumed thread's turns, as chat mode shows them live: the user's message,
/// the turn's start, its items in order, and its end. A turn still running has no end. A turn's
/// start and end take the times its record has. Its items have none, so tool calls have no time.
fn history(turns: &Value) -> Vec<FromAgent> {
    let mut messages = Vec::new();
    for turn in turns.as_array().into_iter().flatten() {
        let started_at = unix_ms(&turn["startedAt"]);
        let mut started = false;
        for item in turn["items"].as_array().into_iter().flatten() {
            if item["type"] == "userMessage" && !started {
                messages.push(FromAgent::User {
                    text: user_text(&item["content"]),
                });
                continue;
            }
            if !started {
                messages.push(FromAgent::TurnStart {
                    time_unix_ms: started_at,
                });
                started = true;
            }
            match item["type"].as_str() {
                Some("agentMessage") => messages.extend(
                    item["text"]
                        .as_str()
                        .map(|text| FromAgent::TextDelta { text: text.into() }),
                ),
                _ => messages.extend(tool_call(item).into_iter().chain(tool_result(item))),
            }
        }
        if !started {
            messages.push(FromAgent::TurnStart {
                time_unix_ms: started_at,
            });
        }
        let error = || TurnError {
            kind: error_kind(&turn["error"]["codexErrorInfo"]),
            message: turn["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        };
        let (interrupted, error) = match turn["status"].as_str() {
            Some("completed") => (false, None),
            Some("interrupted") => (true, None),
            Some("failed") => (false, Some(error())),
            _ => continue,
        };
        messages.push(FromAgent::TurnEnd {
            interrupted,
            error,
            time_unix_ms: unix_ms(&turn["completedAt"]),
        });
    }
    messages
}

/// A time in seconds since the Unix epoch, as Codex records it, in milliseconds.
fn unix_ms(seconds: &Value) -> Option<u64> {
    seconds.as_u64()?.checked_mul(1000)
}

/// The text of a user message's input, one line per text input. Other inputs, such as images,
/// are left out.
fn user_text(content: &Value) -> String {
    let inputs = content.as_array().map(Vec::as_slice).unwrap_or_default();
    inputs
        .iter()
        .filter(|input| input["type"] == "text")
        .filter_map(|input| input["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The tool call an item that just started shows as, if it is one.
fn tool_call(item: &Value) -> Option<FromAgent> {
    let (tool, input) = match item["type"].as_str()? {
        "commandExecution" => (
            "commandExecution".to_owned(),
            json!({"command": item["command"], "cwd": item["cwd"]}),
        ),
        "fileChange" => ("fileChange".to_owned(), json!({"changes": item["changes"]})),
        "mcpToolCall" => (
            format!("{}/{}", item["server"].as_str()?, item["tool"].as_str()?),
            item["arguments"].clone(),
        ),
        _ => return None,
    };
    Some(FromAgent::ToolCall {
        id: item["id"].as_str()?.to_owned(),
        tool,
        input,
        time_unix_ms: None,
    })
}

/// What a tool call returned, from its item once it completed.
fn tool_result(item: &Value) -> Option<FromAgent> {
    let status = item["status"].as_str()?;
    let (output, is_error) = match item["type"].as_str()? {
        "commandExecution" => (
            item["aggregatedOutput"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            status != "completed",
        ),
        "fileChange" => (status.to_owned(), status != "completed"),
        "mcpToolCall" => match item["error"]["message"].as_str() {
            Some(message) => (message.to_owned(), true),
            None => (mcp_text(&item["result"]["content"]), status != "completed"),
        },
        _ => return None,
    };
    Some(FromAgent::ToolResult {
        id: item["id"].as_str()?.to_owned(),
        output,
        is_error,
    })
}

/// An MCP result's text blocks, one per line. Other blocks show as their JSON.
fn mcp_text(content: &Value) -> String {
    let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();
    blocks
        .iter()
        .map(|block| match block["text"].as_str() {
            Some(text) if block["type"] == "text" => text.to_owned(),
            _ => block.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Codex's own name for a turn's error, such as "usageLimitExceeded".
fn error_kind(info: &Value) -> String {
    match info {
        Value::String(kind) => kind.clone(),
        Value::Object(kind) => kind.keys().next().cloned().unwrap_or_default(),
        _ => "other".into(),
    }
}

/// Who is logged in, from an `account/read` reply's account. None when no one is.
fn account(account: &Value) -> Option<AccountInfo> {
    let text = |key: &str| account[key].as_str().map(str::to_owned);
    let kind = account["type"].as_str()?;
    Some(match kind {
        "chatgpt" => AccountInfo {
            email: text("email"),
            organization: None,
            plan: text("planType"),
            api_key_source: None,
        },
        // Codex does not say where the key came from, so this is its name for the login.
        _ => AccountInfo {
            email: None,
            organization: None,
            plan: None,
            api_key_source: Some(kind.to_owned()),
        },
    })
}

/// The models of a `model/list` reply's data. One without a model or a name is left out.
fn models(data: &Value) -> Vec<ModelInfo> {
    let list = data.as_array().map(Vec::as_slice).unwrap_or_default();
    let text = |value: &Value| value.as_str().map(str::to_owned);
    list.iter()
        .filter_map(|model| {
            let efforts = model["supportedReasoningEfforts"].as_array();
            Some(ModelInfo {
                // What `turn/start` takes as its model.
                id: text(&model["model"])?,
                name: text(&model["displayName"])?,
                description: text(&model["description"]).unwrap_or_default(),
                efforts: efforts
                    .into_iter()
                    .flatten()
                    .filter_map(|e| text(&e["reasoningEffort"]))
                    .collect(),
                default_effort: text(&model["defaultReasoningEffort"]),
                is_default: model["isDefault"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

/// The permissions a grant gives: those asked for, leaving out the kinds not asked for.
fn granted(asked: &Value, scope: &str) -> Value {
    let permissions: Map<String, Value> = asked
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(_, value)| !value.is_null())
        .map(|(kind, value)| (kind.clone(), value.clone()))
        .collect();
    json!({"permissions": permissions, "scope": scope})
}

fn text_input(text: &str) -> Value {
    json!([{"type": "text", "text": text, "text_elements": []}])
}

fn line_of(message: Value) -> Vec<u8> {
    format!("{message}\n").into_bytes()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_resumed_threads_turns_take_their_recorded_times_and_its_tool_calls_have_none() {
        let turns = json!([{
            "id": "t1", "status": "completed", "error": null,
            "startedAt": 1_763_560_920, "completedAt": 1_763_560_983,
            "items": [
                {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": "go"}]},
                {"type": "commandExecution", "id": "call_1", "command": "ls", "cwd": "/w",
                 "status": "completed", "aggregatedOutput": "a.rs\n", "exitCode": 0},
            ],
        }, {
            "id": "t2", "status": "completed", "error": null, "items": [],
        }]);

        let restored: Vec<crate::AgentMessage> = history(&turns)
            .into_iter()
            .filter_map(super::super::recorded)
            .collect();

        let times: Vec<Option<u64>> = restored
            .iter()
            .filter_map(|message| match message {
                crate::AgentMessage::TurnStarted { time_unix_ms }
                | crate::AgentMessage::ToolCall { time_unix_ms, .. }
                | crate::AgentMessage::TurnEnded { time_unix_ms, .. } => Some(*time_unix_ms),
                _ => None,
            })
            .collect();
        // Turn one's start, its tool call and its end, then turn two's, whose record has no times.
        assert_eq!(
            times,
            [
                Some(1_763_560_920_000),
                None,
                Some(1_763_560_983_000),
                None,
                None
            ]
        );
    }

    use super::*;

    fn sent(line: &[u8]) -> Value {
        serde_json::from_slice(line).unwrap()
    }

    /// Runs the handshake up to the thread request, and returns that request.
    fn handshake(server: &mut AppServer) -> Value {
        let init = sent(&server.opening());
        let reply = json!({"id": init["id"], "result": {}}).to_string();
        let (_, writes) = server.read(reply.as_bytes());
        sent(&writes[2])
    }

    #[test]
    fn a_new_session_starts_a_thread_and_a_resumed_one_resumes_its_thread() {
        let id = "019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b";

        let start = handshake(&mut AppServer::new(None));
        let resume = handshake(&mut AppServer::new(Some(id.into())));

        assert_eq!(start["method"], "thread/start");
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"], json!({"threadId": id}));
    }

    #[test]
    fn a_failed_model_list_lists_no_models_and_ends_no_turn() {
        let mut server = AppServer::new(None);
        let init = sent(&server.opening());
        let reply = json!({"id": init["id"], "result": {}}).to_string();
        let (_, writes) = server.read(reply.as_bytes());
        let list = sent(&writes[3]);
        assert_eq!(list["method"], "model/list");

        let error = json!({"id": list["id"], "error": {"code": -32600, "message": "unknown"}});
        let (incoming, _) = server.read(error.to_string().as_bytes());

        assert!(matches!(&incoming[..], [FromAgent::Models { models }] if models.is_empty()));
    }

    #[test]
    fn a_failed_thread_start_ends_the_waiting_message_and_the_next_message_tries_again() {
        let mut server = AppServer::new(None);
        let thread = handshake(&mut server);
        assert_eq!(server.user_message("hi"), None);

        let error = json!({"id": thread["id"], "error": {"code": -1, "message": "bad config"}});
        let (incoming, _) = server.read(error.to_string().as_bytes());
        let retry = sent(&server.user_message("again").unwrap());

        assert!(matches!(
            &incoming[..],
            [FromAgent::TurnEnd { error: Some(TurnError { kind, message }), .. }]
                if kind == "thread/start" && message == "bad config"
        ));
        assert_eq!(retry["method"], "thread/start");
    }

    /// A started thread, and the reply that started it.
    fn started(server: &mut AppServer, first: &str) -> Vec<Vec<u8>> {
        let thread = handshake(server);
        assert_eq!(server.user_message(first), None);
        let reply = json!({"id": thread["id"], "result": {"thread": {"id": "th"}}});
        server.read(reply.to_string().as_bytes()).1
    }

    fn completed(server: &mut AppServer) -> Vec<Vec<u8>> {
        let done = json!({"method": "turn/completed", "params": {"turn": {"status": "completed"}}});
        server.read(done.to_string().as_bytes()).1
    }

    fn text_of(line: &[u8]) -> Value {
        sent(line)["params"]["input"][0]["text"].clone()
    }

    #[test]
    fn each_message_gets_its_own_turn_after_the_one_before_ends() {
        let mut server = AppServer::new(None);

        let at_start = started(&mut server, "one");
        let second = server.user_message("two");
        let third = server.user_message("three");
        let after_one = completed(&mut server);
        let after_two = completed(&mut server);

        assert_eq!(at_start.len(), 1);
        assert_eq!(text_of(&at_start[0]), "one");
        assert_eq!((second, third), (None, None));
        assert_eq!(after_one.len(), 1);
        assert_eq!(text_of(&after_one[0]), "two");
        assert_eq!(text_of(&after_two[0]), "three");
    }

    #[test]
    fn a_failed_turn_start_lets_the_next_message_go() {
        let mut server = AppServer::new(None);
        let first = sent(&started(&mut server, "one")[0]);
        assert_eq!(server.user_message("two"), None);

        let error = json!({"id": first["id"], "error": {"code": -1, "message": "busy"}});
        let (_, writes) = server.read(error.to_string().as_bytes());

        assert_eq!(writes.len(), 1);
        assert_eq!(text_of(&writes[0]), "two");
    }

    #[test]
    fn a_late_turn_start_reply_does_not_bring_back_a_finished_turn() {
        let mut server = AppServer::new(None);
        let first = sent(&started(&mut server, "one")[0]);
        completed(&mut server);
        let second = sent(&server.user_message("two").unwrap());

        let late = json!({"id": first["id"], "result": {"turn": {"id": "t1"}}});
        server.read(late.to_string().as_bytes());
        let before_reply = server.interrupt();
        let reply = json!({"id": second["id"], "result": {"turn": {"id": "t2"}}});
        server.read(reply.to_string().as_bytes());
        let interrupt = sent(&server.interrupt().unwrap());

        assert_eq!(before_reply, None);
        assert_eq!(interrupt["params"]["turnId"], "t2");
    }
}
