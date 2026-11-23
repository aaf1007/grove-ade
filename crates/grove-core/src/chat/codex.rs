//! Codex in chat mode: the core speaks `codex app-server`'s JSON-RPC on stdio, one message per
//! line, and maps it to the same messages the Claude sidecar sends. Checked against the
//! protocol `codex app-server generate-ts` prints for Codex CLI 0.158.0, and `thread/fork` and
//! `thread/unsubscribe` against 0.170.0.

use std::collections::{HashMap, HashSet, VecDeque};

use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::{
    FromAgent, ModelInfo, Question, QuestionOption, Reply, RequestId, SideQuestionId, SlashCommand,
    TurnError,
};
use crate::AccountInfo;
use crate::usage::{ContextReading, ContextWindow, UsageRead, UsageWindow};

/// What the core knows of its connection to one app server. The chat's lock guards it.
pub(super) struct AppServer {
    next_id: u64,
    // Each request the core sent that has no reply yet, by id.
    sent: HashMap<u64, Sent>,
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
    // The account is a ChatGPT login, the only kind whose plan has limits to read.
    has_plan_limits: bool,
    // The running turn's latest token count, which goes out when the turn ends. Codex sends
    // one per model call.
    token_usage: Option<ContextReading>,
    // The open side question's fork.
    fork: Option<Fork>,
    // Every fork thread this connection made. A message that names one never reaches the
    // session's own conversation, also after its side question ended.
    forks: HashSet<String>,
}

/// What a request the core sent was for.
enum Sent {
    /// The session's own thread, by method.
    Session(&'static str),
    /// A side question's fork, by method.
    Fork {
        side_question: SideQuestionId,
        method: &'static str,
    },
}

/// The fork of the session's thread that answers a side question.
struct Fork {
    side_question: SideQuestionId,
    question: String,
    // None until `thread/fork` replies.
    thread: Option<String>,
    // The fork's running turn, which `turn/interrupt` names.
    turn: Option<String>,
    // The answer has text, so the next agent message starts a new paragraph.
    has_text: bool,
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
            has_plan_limits: false,
            token_usage: None,
            fork: None,
            forks: HashSet::new(),
        }
    }

    /// The lines that fork the session's thread for side question `id` and end the fork of the
    /// one before. The fork cannot write, asks for no approval and is never saved. Its turn
    /// starts when the fork's reply comes. Fails with the reason while the session has no thread.
    pub(super) fn ask_side_question(
        &mut self,
        id: SideQuestionId,
        question: &str,
    ) -> std::result::Result<Vec<Vec<u8>>, String> {
        let mut lines = match &self.fork {
            Some(fork) => self.end_side_question(fork.side_question),
            None => Vec::new(),
        };
        let Thread::Started { id: thread } = &self.thread else {
            return Err("Codex has not started the conversation yet.".into());
        };
        // Without its turns, which the reply would otherwise repeat in full.
        let params = json!({"threadId": thread, "ephemeral": true, "approvalPolicy": "never",
            "sandbox": "read-only", "excludeTurns": true});
        lines.push(self.fork_request(id, "thread/fork", params));
        self.fork = Some(Fork {
            side_question: id,
            question: question.to_owned(),
            thread: None,
            turn: None,
            has_text: false,
        });
        Ok(lines)
    }

    /// The lines that end side question `id`'s fork: its turn stops and the server lets the
    /// thread go. None when it has ended. A fork still being made ends when its reply comes.
    pub(super) fn end_side_question(&mut self, id: SideQuestionId) -> Vec<Vec<u8>> {
        let Some(fork) = self.fork.take_if(|fork| fork.side_question == id) else {
            return Vec::new();
        };
        let Some(thread) = fork.thread else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        if let Some(turn) = fork.turn {
            let params = json!({"threadId": thread, "turnId": turn});
            lines.push(self.fork_request(id, "turn/interrupt", params));
        }
        lines.push(self.unsubscribe(id, &thread));
        lines
    }

    fn unsubscribe(&mut self, id: SideQuestionId, thread: &str) -> Vec<u8> {
        self.fork_request(id, "thread/unsubscribe", json!({"threadId": thread}))
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
            } => match (
                self.names_fork(&params),
                server_request(&method, &id, params),
            ) {
                // A side question changes nothing and asks nothing, so its fork is declined.
                (
                    true,
                    Ok(FromAgent::ApprovalRequest {
                        reply_to: Some(reply_to),
                        ..
                    })
                    | Ok(FromAgent::QuestionRequest {
                        reply_to: Some(reply_to),
                        ..
                    }),
                ) => {
                    writes.push(reply_to.line(&Reply::Deny { message: None }));
                    Vec::new()
                }
                (_, Ok(request)) => vec![request],
                (_, Err((code, message))) => {
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
            } if self.names_fork(&params) => self.fork_notification(&method, &params, &mut writes),
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

    /// Whether a message from the server is about a side question's fork, by its `threadId`.
    fn names_fork(&self, params: &Value) -> bool {
        params["threadId"]
            .as_str()
            .is_some_and(|thread| self.forks.contains(thread))
    }

    fn request(&mut self, method: &'static str, params: Value) -> Vec<u8> {
        self.send(Sent::Session(method), method, params)
    }

    fn fork_request(
        &mut self,
        side_question: SideQuestionId,
        method: &'static str,
        params: Value,
    ) -> Vec<u8> {
        let sent = Sent::Fork {
            side_question,
            method,
        };
        self.send(sent, method, params)
    }

    fn send(&mut self, sent: Sent, method: &str, params: Value) -> Vec<u8> {
        let id = self.next_id;
        self.next_id += 1;
        self.sent.insert(id, sent);
        line_of(json!({"id": id, "method": method, "params": params}))
    }

    /// The reply to a request for a side question's fork. It never changes the session's own
    /// thread or turns.
    fn fork_response(
        &mut self,
        side_question: SideQuestionId,
        method: &'static str,
        result: Option<Value>,
        error: Option<RpcError>,
        writes: &mut Vec<Vec<u8>>,
    ) -> Vec<FromAgent> {
        let result = result.unwrap_or_default();
        let forked = result["thread"]["id"].as_str();
        let failure = match (method, error) {
            // An interrupt or an unsubscribe that came too late changes nothing.
            ("thread/fork" | "turn/start", Some(error)) => Some(error.message),
            ("thread/fork", None) if forked.is_none() => Some("the reply has no thread id".into()),
            (_, Some(_)) => return Vec::new(),
            (_, None) => None,
        };
        let open = self
            .fork
            .as_mut()
            .filter(|fork| fork.side_question == side_question);
        if let Some(message) = failure {
            // The side question got no answer, and its fork, if it was made, is let go.
            let thread = open.and_then(|fork| fork.thread.clone());
            if let Some(thread) = thread {
                writes.push(self.unsubscribe(side_question, &thread));
            }
            self.fork
                .take_if(|fork| fork.side_question == side_question);
            let failed = FromAgent::TurnEnd {
                interrupted: false,
                error: Some(TurnError {
                    kind: method.to_owned(),
                    message,
                }),
                time_unix_ms: None,
            };
            return vec![side(side_question, failed)];
        }
        match (method, forked, open) {
            ("thread/fork", Some(thread), Some(fork)) => {
                fork.thread = Some(thread.to_owned());
                let params = json!({"threadId": thread, "input": text_input(&fork.question)});
                self.forks.insert(thread.to_owned());
                writes.push(self.fork_request(side_question, "turn/start", params));
            }
            // Its side question ended while the fork was made.
            ("thread/fork", Some(thread), None) => {
                self.forks.insert(thread.to_owned());
                writes.push(self.unsubscribe(side_question, thread));
            }
            ("turn/start", _, Some(fork)) if fork.turn.is_none() => {
                fork.turn = result["turn"]["id"].as_str().map(str::to_owned);
            }
            _ => {}
        }
        Vec::new()
    }

    /// A notification that names a fork's thread. One for a side question that has ended is
    /// dropped. The answer shows the fork's text, and none of its tool calls.
    fn fork_notification(
        &mut self,
        method: &str,
        params: &Value,
        writes: &mut Vec<Vec<u8>>,
    ) -> Vec<FromAgent> {
        let thread = params["threadId"].as_str();
        let Some(fork) = self
            .fork
            .as_mut()
            .filter(|fork| fork.thread.as_deref() == thread)
        else {
            return Vec::new();
        };
        let id = fork.side_question;
        let message = match method {
            "turn/started" => {
                fork.turn = params["turn"]["id"].as_str().map(str::to_owned);
                FromAgent::TurnStart { time_unix_ms: None }
            }
            "item/started" if params["item"]["type"] == "agentMessage" && fork.has_text => {
                FromAgent::TextDelta {
                    text: "\n\n".into(),
                }
            }
            "item/agentMessage/delta" => {
                let Some(text) = params["delta"].as_str() else {
                    return Vec::new();
                };
                fork.has_text = true;
                FromAgent::TextDelta { text: text.into() }
            }
            "turn/completed" => {
                // The answer is whole, so the server lets the fork go.
                let thread = fork.thread.clone().unwrap_or_default();
                self.fork = None;
                writes.push(self.unsubscribe(id, &thread));
                turn_end(&params["turn"])
            }
            _ => return Vec::new(),
        };
        vec![side(id, message)]
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
        let Some((request, sent)) = id
            .as_u64()
            .and_then(|id| Some((id, self.sent.remove(&id)?)))
        else {
            return Vec::new();
        };
        let method = match sent {
            Sent::Session(method) => method,
            Sent::Fork {
                side_question,
                method,
            } => return self.fork_response(side_question, method, result, error, writes),
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
                writes.push(self.request("skills/list", json!({})));
                Vec::new()
            }
            "skills/list" => vec![FromAgent::Skills {
                commands: skills(&result["data"]),
            }],
            "model/list" => vec![FromAgent::Models {
                models: models(&result["data"]),
            }],
            "account/read" => {
                let logged_in = &result["account"];
                let mut incoming = vec![FromAgent::Account {
                    account: account(logged_in),
                }];
                match logged_in["type"].as_str() {
                    Some("chatgpt") => {
                        self.has_plan_limits = true;
                        writes.push(self.request("account/rateLimits/read", json!({})));
                    }
                    // An API key or a cloud provider's login has no plan, so nothing is read.
                    Some(_) => incoming.push(FromAgent::Usage(UsageRead::NoPlanLimits)),
                    None => {}
                }
                incoming
            }
            "account/rateLimits/read" => {
                vec![FromAgent::Usage(usage(&result["rateLimits"]))]
            }
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
            // The account stays unknown, the commands stay as they were, and an interrupt that
            // came too late changes nothing.
            "account/read" | "skills/list" | "turn/interrupt" => return Vec::new(),
            // The picker offers no models, and Codex uses its own.
            "model/list" => return vec![FromAgent::Models { models: Vec::new() }],
            // The last good reading stays, marked. A read never ends a turn.
            "account/rateLimits/read" => {
                return vec![FromAgent::Usage(UsageRead::Failed {
                    reason: Some(message),
                })];
            }
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
            // Only says the list changed, so the core asks for it again.
            "skills/changed" => {
                writes.push(self.request("skills/list", json!({})));
                Vec::new()
            }
            "item/completed" => tool_result(&params["item"]).into_iter().collect(),
            "thread/tokenUsage/updated" => {
                if let Some(reading) = context_reading(&params["tokenUsage"]) {
                    self.token_usage = Some(reading);
                }
                Vec::new()
            }
            "turn/completed" => {
                self.turn = None;
                self.turn_start = None;
                self.turn_starting = false;
                writes.extend(self.next_turn());
                // A turn uses up some of the plan, so the limits are read again after each.
                if self.has_plan_limits {
                    writes.push(self.request("account/rateLimits/read", json!({})));
                }
                let reading = self.token_usage.take().map(FromAgent::Context);
                [turn_end(&params["turn"])]
                    .into_iter()
                    .chain(reading)
                    .collect()
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

/// A message of side question `id`'s fork.
fn side(id: SideQuestionId, message: FromAgent) -> FromAgent {
    FromAgent::Side {
        id,
        message: Box::new(message),
    }
}

/// How a `turn/completed` notification's turn ended.
fn turn_end(turn: &Value) -> FromAgent {
    let error = (turn["status"] == "failed").then(|| TurnError {
        kind: error_kind(&turn["error"]["codexErrorInfo"]),
        message: turn["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    });
    FromAgent::TurnEnd {
        interrupted: turn["status"] == "interrupted",
        error,
        time_unix_ms: None,
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

/// A session's tokens and context use, from a `thread/tokenUsage/updated` notification's
/// `tokenUsage`: the thread's total, and the last model call's tokens against the model's
/// context window. None when it has no total.
fn context_reading(token_usage: &Value) -> Option<ContextReading> {
    let context_window = || {
        Some(ContextWindow {
            used_tokens: token_usage["last"]["totalTokens"].as_u64()?,
            size_tokens: token_usage["modelContextWindow"].as_u64()?,
        })
    };
    Some(ContextReading {
        session_total_tokens: token_usage["total"]["totalTokens"].as_u64()?,
        context_window: context_window(),
    })
}

/// The plan's limit windows, from an `account/rateLimits/read` reply's `rateLimits`: the
/// primary window, then the secondary. A reply with neither counts as a failed read.
fn usage(rate_limits: &Value) -> UsageRead {
    let windows: Vec<UsageWindow> = [("primary", "Primary"), ("secondary", "Secondary")]
        .into_iter()
        .filter_map(|(key, unnamed)| {
            let window = &rate_limits[key];
            let percent_used = window["usedPercent"].as_u64()?.min(100);
            Some(UsageWindow {
                name: window_name(window["windowDurationMins"].as_u64(), unnamed),
                percent_used: u8::try_from(percent_used).ok()?,
                resets_at_unix_ms: unix_ms(&window["resetsAt"]),
            })
        })
        .collect();
    if windows.is_empty() {
        UsageRead::Failed {
            reason: Some("the reply has no limit window".into()),
        }
    } else {
        UsageRead::Windows { windows }
    }
}

/// A limit window's name from how long it lasts, such as "5-hour". `unnamed` when Codex does
/// not say.
fn window_name(minutes: Option<u64>, unnamed: &str) -> String {
    const HOUR: u64 = 60;
    const DAY: u64 = 24 * HOUR;
    const WEEK: u64 = 7 * DAY;
    match minutes {
        None | Some(0) => unnamed.to_owned(),
        Some(WEEK) => "Weekly".to_owned(),
        Some(minutes) if minutes % DAY == 0 => format!("{}-day", minutes / DAY),
        Some(minutes) if minutes % HOUR == 0 => format!("{}-hour", minutes / HOUR),
        Some(minutes) => format!("{minutes}-minute"),
    }
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

/// The enabled skills of a `skills/list` reply's data, as commands. Codex runs a skill by
/// `$name` in a message.
fn skills(data: &Value) -> Vec<SlashCommand> {
    let entries = data.as_array().map(Vec::as_slice).unwrap_or_default();
    entries
        .iter()
        .flat_map(|entry| entry["skills"].as_array().into_iter().flatten())
        .filter(|skill| skill["enabled"].as_bool().unwrap_or(false))
        .filter_map(|skill| {
            let name = skill["name"].as_str()?;
            let description = [
                &skill["interface"]["shortDescription"],
                &skill["shortDescription"],
                &skill["description"],
            ]
            .into_iter()
            .find_map(Value::as_str)
            .unwrap_or_default();
            Some(SlashCommand {
                name: name.to_owned(),
                description: description.to_owned(),
                argument_hint: String::new(),
                invocation: format!("${name}"),
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

    const SIDE: SideQuestionId = SideQuestionId(0);

    /// The message of the side question that `incoming` holds, and nothing else.
    fn side_turn_error(incoming: &[FromAgent]) -> (&str, &str) {
        let [FromAgent::Side { id: SIDE, message }] = incoming else {
            panic!("no side message");
        };
        let FromAgent::TurnEnd {
            error: Some(TurnError { kind, message }),
            ..
        } = &**message
        else {
            panic!("no failed turn end");
        };
        (kind, message)
    }

    #[test]
    fn a_side_question_cannot_fork_a_thread_that_has_not_started() {
        let mut server = AppServer::new(None);
        handshake(&mut server);

        let asked = server.ask_side_question(SIDE, "why?");

        assert_eq!(
            asked,
            Err("Codex has not started the conversation yet.".into())
        );
    }

    #[test]
    fn a_fork_made_after_its_side_question_ended_is_let_go_and_starts_no_turn() {
        let mut server = AppServer::new(None);
        started(&mut server, "one");
        let fork = sent(&server.ask_side_question(SIDE, "why?").unwrap()[0]);
        let ended = server.end_side_question(SIDE);

        let reply = json!({"id": fork["id"], "result": {"thread": {"id": "fork"}}});
        let (incoming, writes) = server.read(reply.to_string().as_bytes());
        let late = json!({"method": "turn/completed",
            "params": {"threadId": "fork", "turn": {"status": "completed"}}});
        let (after, _) = server.read(late.to_string().as_bytes());

        assert!(ended.is_empty());
        assert!(incoming.is_empty() && after.is_empty());
        let [unsubscribe] = &writes[..] else {
            panic!("want one line, got {}", writes.len());
        };
        let unsubscribe = sent(unsubscribe);
        assert_eq!(unsubscribe["method"], "thread/unsubscribe");
        assert_eq!(unsubscribe["params"], json!({"threadId": "fork"}));
        // The session's own turn still runs.
        assert_eq!(server.user_message("two"), None);
    }

    #[test]
    fn a_fork_that_fails_ends_the_side_question_failed_and_leaves_the_sessions_turn_alone() {
        let mut server = AppServer::new(None);
        started(&mut server, "one");
        let fork = sent(&server.ask_side_question(SIDE, "why?").unwrap()[0]);

        let error = json!({"id": fork["id"], "error": {"code": -1, "message": "no rollout"}});
        let (incoming, writes) = server.read(error.to_string().as_bytes());

        assert_eq!(side_turn_error(&incoming), ("thread/fork", "no rollout"));
        assert!(writes.is_empty());
        assert_eq!(server.user_message("two"), None);
    }

    #[test]
    fn a_fork_whose_turn_fails_to_start_ends_the_side_question_failed_and_is_let_go() {
        let mut server = AppServer::new(None);
        started(&mut server, "one");
        let fork = sent(&server.ask_side_question(SIDE, "why?").unwrap()[0]);
        let reply = json!({"id": fork["id"], "result": {"thread": {"id": "fork"}}});
        let turn = sent(&server.read(reply.to_string().as_bytes()).1[0]);
        assert_eq!(turn["params"]["threadId"], "fork");

        let error = json!({"id": turn["id"], "error": {"code": -1, "message": "busy"}});
        let (incoming, writes) = server.read(error.to_string().as_bytes());

        assert_eq!(side_turn_error(&incoming), ("turn/start", "busy"));
        assert_eq!(sent(&writes[0])["method"], "thread/unsubscribe");
        assert!(server.end_side_question(SIDE).is_empty());
    }

    #[test]
    fn a_forks_second_agent_message_starts_a_new_paragraph_and_its_tool_calls_are_not_shown() {
        let mut server = AppServer::new(None);
        started(&mut server, "one");
        let fork = sent(&server.ask_side_question(SIDE, "why?").unwrap()[0]);
        let reply = json!({"id": fork["id"], "result": {"thread": {"id": "fork"}}});
        server.read(reply.to_string().as_bytes());
        let mut texts = Vec::new();
        let mut note = |method: &str, params: Value| {
            let mut params = params;
            params["threadId"] = "fork".into();
            let line = json!({"method": method, "params": params}).to_string();
            for message in server.read(line.as_bytes()).0 {
                let FromAgent::Side { id: SIDE, message } = message else {
                    panic!("a fork's message reached the session");
                };
                if let FromAgent::TextDelta { text } = *message {
                    texts.push(text);
                }
            }
        };

        note(
            "item/started",
            json!({"item": {"type": "agentMessage", "id": "m1"}}),
        );
        note("item/agentMessage/delta", json!({"delta": "Looking."}));
        note(
            "item/started",
            json!({"item": {"type": "commandExecution", "id": "c1", "command": "ls", "cwd": "/w"}}),
        );
        note(
            "item/started",
            json!({"item": {"type": "agentMessage", "id": "m2"}}),
        );
        note("item/agentMessage/delta", json!({"delta": "Found it."}));

        assert_eq!(texts, ["Looking.", "\n\n", "Found it."]);
    }

    /// The `skills/list` request among `writes`.
    fn skills_request(writes: &[Vec<u8>]) -> Value {
        writes
            .iter()
            .map(|line| sent(line))
            .find(|line| line["method"] == "skills/list")
            .expect("no skills/list request")
    }

    fn skills_after_handshake(server: &mut AppServer) -> Value {
        let init = sent(&server.opening());
        let reply = json!({"id": init["id"], "result": {}}).to_string();
        skills_request(&server.read(reply.as_bytes()).1)
    }

    #[test]
    fn the_enabled_skills_codex_lists_become_commands_run_by_dollar_name() {
        let mut server = AppServer::new(None);
        let list = skills_after_handshake(&mut server);
        let skill = |name: &str, enabled: bool, extra: Value| {
            let mut skill = json!({"name": name, "description": format!("{name} in full"),
                "enabled": enabled, "path": format!("/s/{name}/SKILL.md"), "scope": "user"});
            skill
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            skill
        };
        let reply = json!({"id": list["id"], "result": {"data": [{"cwd": "/w", "errors": [], "skills": [
            skill("review", true, json!({"interface": {"shortDescription": "Review a diff"}})),
            skill("off", false, json!({})),
            skill("plan", true, json!({"shortDescription": "Plan it"})),
            skill("tidy", true, json!({})),
        ]}]}});

        let (incoming, _) = server.read(reply.to_string().as_bytes());

        let [FromAgent::Skills { commands }] = &incoming[..] else {
            panic!("no skills");
        };
        let shown: Vec<(&str, &str, &str, &str)> = commands
            .iter()
            .map(|c| {
                let SlashCommand {
                    name,
                    description,
                    argument_hint,
                    invocation,
                } = c;
                (
                    name.as_str(),
                    description.as_str(),
                    argument_hint.as_str(),
                    invocation.as_str(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("review", "Review a diff", "", "$review"),
                ("plan", "Plan it", "", "$plan"),
                ("tidy", "tidy in full", "", "$tidy"),
            ]
        );
    }

    #[test]
    fn a_skills_change_asks_for_the_list_again_and_a_failed_list_ends_no_turn() {
        let mut server = AppServer::new(None);
        skills_after_handshake(&mut server);

        let changed = json!({"method": "skills/changed", "params": {}});
        let (incoming, writes) = server.read(changed.to_string().as_bytes());
        let again = skills_request(&writes);
        let error = json!({"id": again["id"], "error": {"code": -32600, "message": "unknown"}});
        let (failed, _) = server.read(error.to_string().as_bytes());

        assert!(incoming.is_empty());
        assert!(failed.is_empty());
    }
}
