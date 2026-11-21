//! Codex agent sessions in chat mode, driven by the fake `codex app-server` in
//! fixtures/fake-agents.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use common::{conversation_only, now_unix_ms, times, untimed};
use grove_core::{
    AccountInfo, Agent, AgentMessage, AgentSpec, AgentState, Core, CoreConfig, CoreEvent,
    InputKind, Mode, ModelInfo, Question, QuestionOption, Reply, RequestId, SessionId,
    SlashCommand, StateSource, TurnOutcome,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::sync::mpsc::unbounded_channel;

const TIMEOUT: Duration = Duration::from_secs(10);

// The core owns its runtime, and dropping it inside an async context panics.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

/// A core tracking one fresh repo, with Codex replaced by its fake playing `steps` as
/// `codex app-server`.
struct Setup {
    _dir: TempDir,
    core: Core,
    events: broadcast::Receiver<CoreEvent>,
    worktree: PathBuf,
    log: PathBuf,
    /// What the fake's `skills/list` returns. Missing: its own list.
    skills: PathBuf,
}

fn ann() -> Value {
    json!({"type": "chatgpt", "email": "ann@example.com", "planType": "plus"})
}

fn setup(steps: Value) -> Setup {
    setup_with_account(steps, ann())
}

fn setup_with_account(steps: Value, account: Value) -> Setup {
    // macOS caps a socket path at 104 bytes, and its default temp folder is long.
    let dir = tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().join("repo");
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());

    // The fake reads its scenario, log and account from its environment, so a wrapper sets them.
    let scenario = dir.path().join("scenario.json");
    fs::write(&scenario, json!({ "steps": steps }).to_string()).unwrap();
    let log = dir.path().join("fake.log");
    let account_file = dir.path().join("account.json");
    let skills = dir.path().join("skills.json");
    fs::write(&account_file, account.to_string()).unwrap();
    let wrapper = dir.path().join("codex");
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents/codex");
    let script = format!(
        "#!/bin/sh\nexport FAKE_CODEX_SCENARIO='{0}' FAKE_CODEX_APP_SERVER_SCENARIO='{0}' FAKE_CODEX_LOG='{1}' FAKE_CODEX_ACCOUNT=\"$(cat '{2}')\" FAKE_CODEX_SKILLS='{4}'\nexec '{3}' \"$@\"\n",
        scenario.display(),
        log.display(),
        account_file.display(),
        fake.display(),
        skills.display(),
    );
    fs::write(&wrapper, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut cfg = CoreConfig::new(dir.path().join("home"));
    cfg.codex_command = wrapper;
    let (core, events) = Core::start(cfg).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    Setup {
        _dir: dir,
        core,
        events,
        worktree,
        log,
        skills,
    }
}

// A failed test shows what the fake did.
impl Drop for Setup {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!(
                "fake log:\n{}",
                fs::read_to_string(&self.log).unwrap_or_default()
            );
        }
    }
}

fn spec(mode: Mode, prompt: Option<&str>) -> AgentSpec {
    AgentSpec {
        agent: Agent::Codex,
        mode,
        prompt: prompt.map(str::to_owned),
        cols: 120,
        rows: 40,
        panel: None,
    }
}

impl Setup {
    fn spawn(&self, prompt: Option<&str>) -> SessionId {
        self.core
            .spawn_agent(&self.worktree, spec(Mode::Chat, prompt))
            .unwrap()
    }

    fn log(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Every message the core has sent the fake so far, in order.
    fn sent(&self) -> Vec<Value> {
        self.log()
            .iter()
            .filter_map(|line| line["stdin"].as_str())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Waits until the core has sent a message that `wanted` picks, and returns it.
    fn wait_sent(&self, what: &str, wanted: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(message) = self.sent().into_iter().find(&wanted) {
                return message;
            }
            assert!(Instant::now() < deadline, "the core never sent {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_request(&self, method: &str) -> Value {
        self.wait_sent(method, |m| m["method"] == method)
    }

    /// The core's reply to the fake's request `id`.
    fn wait_reply(&self, id: u64) -> Value {
        self.wait_sent(&format!("a reply to {id}"), |m| {
            m["id"] == id && m.get("method").is_none()
        })
    }

    /// Waits until the core has read the thread id from the fake's reply.
    fn wait_thread(&self, session: SessionId) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(id) = self.core.resume_id(session) {
                return id;
            }
            assert!(Instant::now() < deadline, "no thread started");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn next_event(&mut self) -> CoreEvent {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(event) => return event,
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "no event came");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// The next agent state the core sends, skipping other events.
    fn next_state(&mut self) -> (SessionId, AgentState, StateSource) {
        loop {
            if let CoreEvent::AgentState {
                session,
                state,
                source,
            } = self.next_event()
            {
                return (session, state, source);
            }
        }
    }

    /// Waits until the session's process exits. The fake can die by a signal when it exits
    /// while it writes, so its exit state is not checked.
    fn wait_exit(&mut self, session: SessionId) {
        while !matches!(self.next_event(), CoreEvent::SessionExited { session: s, .. } if s == session)
        {
        }
    }

    fn state(&mut self, session: SessionId, state: AgentState) {
        assert_eq!(self.next_state(), (session, state, StateSource::Protocol));
    }
}

/// A subscribed sink's messages after the history, received on a thread of their own.
struct Frames {
    arrivals: mpsc::Receiver<Vec<AgentMessage>>,
    messages: Vec<AgentMessage>,
    // Where the next `until` starts looking.
    cursor: usize,
}

impl Frames {
    fn subscribe(core: &Core, session: SessionId) -> Frames {
        let (sink, mut rx) = unbounded_channel();
        let history = core.subscribe(session, sink).unwrap();
        let (tx, arrivals) = mpsc::channel();
        thread::spawn(move || {
            while let Some(frame) = rx.blocking_recv() {
                if tx.send(frame).is_err() {
                    return;
                }
            }
        });
        Frames {
            arrivals,
            messages: history,
            cursor: 0,
        }
    }

    /// Takes frames until a message after the last one this returned is one `wanted` picks,
    /// and returns it.
    fn until(&mut self, what: &str, wanted: impl Fn(&AgentMessage) -> bool) -> AgentMessage {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(i) = self.messages[self.cursor..].iter().position(&wanted) {
                self.cursor += i + 1;
                return self.messages[self.cursor - 1].clone();
            }
            self.cursor = self.messages.len();
            match self
                .arrivals
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(frame) => self.messages.extend(frame),
                Err(RecvTimeoutError::Disconnected) => panic!("the sink closed before {what}"),
                Err(RecvTimeoutError::Timeout) => panic!("{what} never came"),
            }
        }
    }

    fn until_turn_end(&mut self) -> TurnOutcome {
        match self.until("the turn end", |m| {
            matches!(m, AgentMessage::TurnEnded { .. })
        }) {
            AgentMessage::TurnEnded { outcome, .. } => outcome,
            _ => unreachable!(),
        }
    }
}

fn send(message: Value) -> Value {
    json!({ "send": message })
}

fn wait(method: &str) -> Value {
    json!({ "wait_message": method })
}

fn notify(method: &str, params: Value) -> Value {
    send(json!({"method": method, "params": params}))
}

/// A request from the fake to the core.
fn request(id: u64, method: &str, mut params: Value) -> Value {
    params["threadId"] = "{{thread_id}}".into();
    params["turnId"] = "{{turn_id}}".into();
    send(json!({"id": id, "method": method, "params": params}))
}

fn turn_started() -> Value {
    notify(
        "turn/started",
        json!({"threadId": "{{thread_id}}", "turn": {"id": "{{turn_id}}", "items": [],
               "status": "inProgress", "error": null}}),
    )
}

fn text_delta(text: &str) -> Value {
    notify(
        "item/agentMessage/delta",
        json!({"threadId": "{{thread_id}}", "turnId": "{{turn_id}}", "itemId": "msg_1",
               "delta": text}),
    )
}

fn turn_completed(status: &str, error: Value) -> Value {
    notify(
        "turn/completed",
        json!({"threadId": "{{thread_id}}", "turn": {"id": "{{turn_id}}", "items": [],
               "status": status, "error": error}}),
    )
}

/// One turn: waits for a message, then streams `deltas` and completes the turn.
fn turn(deltas: &[&str]) -> Vec<Value> {
    let mut steps = vec![wait("turn/start"), turn_started()];
    steps.extend(deltas.iter().map(|d| text_delta(d)));
    steps.push(turn_completed("completed", Value::Null));
    steps
}

fn item(method: &str, item: Value) -> Value {
    notify(
        method,
        json!({"threadId": "{{thread_id}}", "turnId": "{{turn_id}}", "item": item,
               "startedAtMs": 0, "completedAtMs": 0}),
    )
}

fn delta(text: &str) -> AgentMessage {
    AgentMessage::TextDelta { text: text.into() }
}

fn started() -> AgentMessage {
    AgentMessage::TurnStarted { time_unix_ms: None }
}

fn completed() -> AgentMessage {
    AgentMessage::TurnEnded {
        outcome: TurnOutcome::Completed,
        time_unix_ms: None,
    }
}

fn text_input(text: &str) -> Value {
    json!([{"type": "text", "text": text, "text_elements": []}])
}

#[test]
fn a_message_starts_a_turn_on_the_thread_its_deltas_arrive_in_order_and_the_turn_end_makes_it_done()
{
    let mut s = setup(json!(turn(&["Hel", "lo", " there"])));
    let session = s.spawn(None);
    let mut frames = Frames::subscribe(&s.core, session);

    s.core.send_message(session, "hi").unwrap();

    s.state(session, AgentState::Working);
    assert_eq!(frames.until_turn_end(), TurnOutcome::Completed);
    assert_eq!(
        untimed(conversation_only(frames.messages)),
        [
            AgentMessage::User { text: "hi".into() },
            started(),
            delta("Hel"),
            delta("lo"),
            delta(" there"),
            completed(),
        ]
    );
    s.state(session, AgentState::Done);
    let thread = s.wait_thread(session);
    let start = s.wait_request("turn/start");
    assert_eq!(
        start["params"],
        json!({"threadId": thread, "input": text_input("hi")})
    );
}

#[test]
fn the_handshake_comes_first_and_a_first_prompt_waits_for_the_thread() {
    let mut s = setup(json!(turn(&["ok"])));
    let session = s.spawn(Some("go"));

    s.state(session, AgentState::Working);
    s.state(session, AgentState::Done);

    let methods: Vec<Value> = s.sent().iter().map(|m| m["method"].clone()).collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "account/read",
            "thread/start",
            "model/list",
            "skills/list",
            "turn/start"
        ]
    );
    assert_eq!(s.sent()[0]["params"]["clientInfo"]["name"], "grove");
    assert_eq!(
        s.wait_request("turn/start")["params"]["input"],
        text_input("go")
    );
}

#[test]
fn the_account_comes_from_the_app_server() {
    let mut s = setup(json!([]));
    s.spawn(None);
    let mut logged_out = setup_with_account(json!([]), Value::Null);
    logged_out.spawn(None);

    let account = |s: &mut Setup| loop {
        if let CoreEvent::Account { agent, account } = s.next_event() {
            assert_eq!(agent, Agent::Codex);
            return account;
        }
    };

    assert_eq!(
        account(&mut s),
        Some(AccountInfo {
            email: Some("ann@example.com".into()),
            organization: None,
            plan: Some("plus".into()),
            api_key_source: None,
        })
    );
    assert_eq!(account(&mut logged_out), None);
}

#[test]
fn each_approval_kind_makes_the_session_need_input_and_its_answer_is_the_matching_reply() {
    let command = json!({"itemId": "call_1", "kind": "command", "startedAtMs": 1,
                         "environmentId": null, "command": "npm test", "cwd": "/w"});
    let file_change = json!({"itemId": "patch_1", "startedAtMs": 1, "reason": "edit"});
    let network = json!({"itemId": "perm_1", "startedAtMs": 1, "environmentId": null,
                         "cwd": "/w", "reason": "fetch",
                         "permissions": {"network": {"enabled": true}, "fileSystem": null}});
    let write = json!({"itemId": "perm_2", "startedAtMs": 1, "environmentId": null,
                       "cwd": "/w", "reason": null,
                       "permissions": {"network": null,
                                       "fileSystem": {"read": null, "write": ["/w/out"]}}});
    let denied = json!({"itemId": "call_2", "kind": "command", "startedAtMs": 1,
                        "environmentId": null, "command": "rm -rf /"});
    let asks = [
        (
            "item/commandExecution/requestApproval",
            &command,
            "commandExecution",
        ),
        (
            "item/fileChange/requestApproval",
            &file_change,
            "fileChange",
        ),
        ("item/permissions/requestApproval", &network, "permissions"),
        ("item/permissions/requestApproval", &write, "permissions"),
        (
            "item/commandExecution/requestApproval",
            &denied,
            "commandExecution",
        ),
    ];
    let mut steps = vec![wait("turn/start"), turn_started()];
    for (id, (method, params, _)) in asks.iter().enumerate() {
        steps.push(request(id as u64, method, (*params).clone()));
        steps.push(wait("response"));
    }
    steps.push(turn_completed("completed", Value::Null));
    let mut s = setup(json!(steps));
    let session = s.spawn(Some("go"));
    let mut frames = Frames::subscribe(&s.core, session);
    s.state(session, AgentState::Working);
    let replies = [
        (Reply::AllowOnce, json!({"decision": "accept"})),
        (
            Reply::AllowForSession,
            json!({"decision": "acceptForSession"}),
        ),
        (
            Reply::AllowOnce,
            json!({"permissions": {"network": {"enabled": true}}, "scope": "turn"}),
        ),
        (
            Reply::AllowForSession,
            json!({"permissions": {"fileSystem": {"read": null, "write": ["/w/out"]}},
                   "scope": "session"}),
        ),
        (
            Reply::Deny {
                message: Some("no".into()),
            },
            json!({"decision": "decline"}),
        ),
    ];

    for (id, ((_, params, tool), (reply, result))) in asks.iter().zip(replies).enumerate() {
        let item = params["itemId"].as_str().unwrap();
        let asked = frames.until(item, |m| {
            matches!(m, AgentMessage::ApprovalRequested { .. })
        });
        s.state(
            session,
            AgentState::NeedsInput {
                kind: InputKind::Approval,
            },
        );
        let mut input = (*params).clone();
        input["threadId"] = s.wait_thread(session).into();
        let AgentMessage::ApprovalRequested {
            request,
            tool: asked_tool,
            input: asked_input,
        } = asked
        else {
            unreachable!()
        };
        assert_eq!(request, RequestId(item.into()));
        assert_eq!(asked_tool, *tool);
        assert_eq!(asked_input["itemId"], input["itemId"]);
        assert_eq!(asked_input["threadId"], input["threadId"]);

        s.core.answer(session, request, reply).unwrap();

        s.state(session, AgentState::Working);
        assert_eq!(s.wait_reply(id as u64), json!({"id": id, "result": result}));
    }
    assert_eq!(frames.until_turn_end(), TurnOutcome::Completed);
    s.state(session, AgentState::Done);
}

#[test]
fn a_question_makes_the_session_need_input_and_the_answers_reach_the_agent_by_question_id() {
    let questions = json!({"itemId": "call_q", "isBlocking": true, "autoResolutionMs": null,
        "questions": [
            {"id": "lib", "header": "Library", "question": "Which one?", "isOther": true,
             "isSecret": false, "options": [{"label": "tokio", "description": "async"}]},
            {"id": "name", "header": "Name", "question": "Its name?", "isOther": true,
             "isSecret": false, "options": null}]});
    let mut skipped = questions.clone();
    skipped["itemId"] = "call_r".into();
    let steps = vec![
        wait("turn/start"),
        turn_started(),
        request(0, "item/tool/requestUserInput", questions),
        wait("response"),
        request(1, "item/tool/requestUserInput", skipped),
        wait("response"),
        turn_completed("completed", Value::Null),
    ];
    let mut s = setup(json!(steps));
    let session = s.spawn(Some("go"));
    let mut frames = Frames::subscribe(&s.core, session);
    s.state(session, AgentState::Working);
    let question = AgentState::NeedsInput {
        kind: InputKind::Question,
    };

    let asked = frames.until("the question", |m| {
        matches!(m, AgentMessage::QuestionAsked { .. })
    });
    s.state(session, question);
    s.core
        .answer(
            session,
            RequestId("call_q".into()),
            Reply::Answers(vec![vec!["tokio".into()], vec!["grove".into()]]),
        )
        .unwrap();
    s.state(session, AgentState::Working);
    let reply = s.wait_reply(0);
    frames.until(
        "the second question",
        |m| matches!(m, AgentMessage::QuestionAsked { request, .. } if request.0 == "call_r"),
    );
    s.state(session, question);
    s.core
        .answer(
            session,
            RequestId("call_r".into()),
            Reply::Deny { message: None },
        )
        .unwrap();

    assert_eq!(
        asked,
        AgentMessage::QuestionAsked {
            request: RequestId("call_q".into()),
            questions: vec![
                Question {
                    question: "Which one?".into(),
                    header: "Library".into(),
                    options: vec![QuestionOption {
                        label: "tokio".into(),
                        description: "async".into(),
                    }],
                    multi_select: false,
                },
                Question {
                    question: "Its name?".into(),
                    header: "Name".into(),
                    options: vec![],
                    multi_select: false,
                },
            ],
        }
    );
    assert_eq!(
        reply,
        json!({"id": 0, "result": {"answers": {"lib": {"answers": ["tokio"]},
                                               "name": {"answers": ["grove"]}}}})
    );
    s.state(session, AgentState::Working);
    assert_eq!(s.wait_reply(1), json!({"id": 1, "result": {"answers": {}}}));
    s.state(session, AgentState::Done);
}

#[test]
fn interrupt_sends_turn_interrupt_for_the_running_turn_and_leaves_the_session_idle() {
    let steps = vec![
        wait("turn/start"),
        turn_started(),
        text_delta("work"),
        wait("turn/interrupt"),
        turn_completed("interrupted", Value::Null),
    ];
    let mut s = setup(json!(steps));
    let session = s.spawn(None);
    let mut frames = Frames::subscribe(&s.core, session);
    let thread = s.wait_thread(session);
    // No turn runs yet, so there is nothing to interrupt.
    s.core.interrupt(session).unwrap();

    s.core.send_message(session, "go").unwrap();
    s.state(session, AgentState::Working);
    frames.until("the delta", |m| m == &delta("work"));
    s.core.interrupt(session).unwrap();

    // The fake plays on only for the running turn's id.
    assert_eq!(frames.until_turn_end(), TurnOutcome::Interrupted);
    s.state(session, AgentState::Idle);
    let interrupts: Vec<Value> = s
        .sent()
        .into_iter()
        .filter(|m| m["method"] == "turn/interrupt")
        .collect();
    assert_eq!(interrupts.len(), 1);
    assert_eq!(interrupts[0]["params"]["threadId"], thread);
}

fn user(text: &str) -> AgentMessage {
    AgentMessage::User { text: text.into() }
}

fn not_delivered(text: &str) -> AgentMessage {
    AgentMessage::NotDelivered { text: text.into() }
}

fn conversation(core: &Core, session: SessionId) -> Vec<AgentMessage> {
    conversation_only(core.subscribe(session, unbounded_channel().0).unwrap())
}

#[test]
fn a_first_prompt_shows_as_not_delivered_when_the_app_server_exits_at_once() {
    let mut s = setup(json!([{"exit": 0}]));

    let session = s.spawn(Some("go"));

    s.wait_exit(session);
    assert_eq!(
        untimed(conversation(&s.core, session)),
        [user("go"), not_delivered("go")]
    );
}

#[test]
fn a_message_the_app_server_exits_before_starting_a_turn_on_shows_as_not_delivered() {
    let mut steps = turn(&["ok"]);
    steps.extend([wait("turn/start"), json!({"exit": 0})]);
    let mut s = setup(json!(steps));
    let session = s.spawn(Some("one"));
    s.state(session, AgentState::Working);
    s.state(session, AgentState::Done);

    s.core.send_message(session, "two").unwrap();

    s.wait_exit(session);
    assert_eq!(
        untimed(conversation(&s.core, session)),
        [
            user("one"),
            started(),
            delta("ok"),
            completed(),
            user("two"),
            not_delivered("two")
        ]
    );
}

#[test]
fn a_message_waiting_for_the_running_turn_shows_as_not_delivered_when_the_app_server_exits() {
    let steps = vec![
        wait("turn/start"),
        turn_started(),
        wait("turn/interrupt"),
        json!({"exit": 1}),
    ];
    let mut s = setup(json!(steps));
    let session = s.spawn(Some("one"));
    s.state(session, AgentState::Working);

    s.core.send_message(session, "two").unwrap();
    // The fake exits when the interrupt comes, with "two" still waiting in the core.
    s.core.interrupt(session).unwrap();

    s.wait_exit(session);
    assert_eq!(
        untimed(conversation(&s.core, session)),
        [user("one"), started(), user("two"), not_delivered("two")]
    );
}

#[test]
fn a_failed_turn_makes_the_session_failed_with_codexs_name_for_the_error() {
    let error = json!({"message": "You've hit your usage limit.",
                       "codexErrorInfo": "usageLimitExceeded",
                       "additionalDetails": null, "misalignment": null});
    let steps = vec![
        wait("turn/start"),
        turn_started(),
        turn_completed("failed", error),
    ];
    let mut s = setup(json!(steps));
    let session = s.spawn(Some("go"));
    let mut frames = Frames::subscribe(&s.core, session);

    s.state(session, AgentState::Working);

    assert_eq!(
        frames.until_turn_end(),
        TurnOutcome::Failed {
            kind: "usageLimitExceeded".into(),
            message: "You've hit your usage limit.".into(),
        }
    );
    s.state(session, AgentState::Failed);
}

#[test]
fn commands_file_changes_and_mcp_calls_show_as_tool_calls_and_results() {
    let command = json!({"type": "commandExecution", "id": "call_1", "command": "npm test",
                         "cwd": "/w", "status": "inProgress", "aggregatedOutput": null,
                         "exitCode": null});
    let mut command_done = command.clone();
    command_done["status"] = "failed".into();
    command_done["aggregatedOutput"] = "1 failed\n".into();
    command_done["exitCode"] = 1.into();
    let changes = json!([{"path": "/w/a.rs", "kind": {"type": "update", "move_path": null},
                          "diff": "@@"}]);
    let patch = json!({"type": "fileChange", "id": "patch_1", "changes": changes,
                       "status": "inProgress"});
    let mut patch_done = patch.clone();
    patch_done["status"] = "completed".into();
    let mcp = json!({"type": "mcpToolCall", "id": "mcp_1", "server": "grove",
                     "tool": "open_panel", "status": "inProgress", "arguments": {"kind": "diff"},
                     "result": null, "error": null});
    let mut mcp_done = mcp.clone();
    mcp_done["status"] = "completed".into();
    mcp_done["result"] = json!({"content": [{"type": "text", "text": "opened"}],
                                "structuredContent": null, "_meta": null});
    let reply = json!({"type": "agentMessage", "id": "msg_1", "text": "", "phase": null});
    let steps = vec![
        wait("turn/start"),
        turn_started(),
        item("item/started", reply),
        item("item/started", command),
        item("item/completed", command_done),
        item("item/started", patch),
        item("item/completed", patch_done),
        item("item/started", mcp),
        item("item/completed", mcp_done),
        turn_completed("completed", Value::Null),
    ];
    let s = setup(json!(steps));
    let session = s.spawn(None);
    let mut frames = Frames::subscribe(&s.core, session);

    s.core.send_message(session, "go").unwrap();

    frames.until_turn_end();
    let tools: Vec<AgentMessage> = frames
        .messages
        .into_iter()
        .filter(|m| {
            matches!(
                m,
                AgentMessage::ToolCall { .. } | AgentMessage::ToolResult { .. }
            )
        })
        .collect();
    assert_eq!(
        untimed(tools),
        [
            AgentMessage::ToolCall {
                id: "call_1".into(),
                tool: "commandExecution".into(),
                input: json!({"command": "npm test", "cwd": "/w"}),
                time_unix_ms: None,
            },
            AgentMessage::ToolResult {
                id: "call_1".into(),
                output: "1 failed\n".into(),
                is_error: true,
            },
            AgentMessage::ToolCall {
                id: "patch_1".into(),
                tool: "fileChange".into(),
                input: json!({"changes": changes}),
                time_unix_ms: None,
            },
            AgentMessage::ToolResult {
                id: "patch_1".into(),
                output: "completed".into(),
                is_error: false,
            },
            AgentMessage::ToolCall {
                id: "mcp_1".into(),
                tool: "grove/open_panel".into(),
                input: json!({"kind": "diff"}),
                time_unix_ms: None,
            },
            AgentMessage::ToolResult {
                id: "mcp_1".into(),
                output: "opened".into(),
                is_error: false,
            },
        ]
    );
}

#[test]
fn a_request_grove_does_not_handle_gets_an_error_so_the_turn_goes_on() {
    let steps = vec![
        wait("turn/start"),
        turn_started(),
        request(
            7,
            "item/tool/call",
            json!({"callId": "c", "tool": "x", "arguments": {}}),
        ),
        wait("response"),
        turn_completed("completed", Value::Null),
    ];
    let mut s = setup(json!(steps));
    let session = s.spawn(Some("go"));

    s.state(session, AgentState::Working);
    s.state(session, AgentState::Done);

    let reply = s.wait_reply(7);
    assert_eq!(reply["error"]["code"], -32601);
    assert!(reply.get("result").is_none());
}

#[test]
fn the_app_server_gets_the_same_overrides_and_session_id_as_a_terminal_launch() {
    let s = setup(json!([]));
    let terminal = s
        .core
        .spawn_agent(&s.worktree, spec(Mode::Terminal, None))
        .unwrap();
    let chat = s.spawn(None);
    s.wait_thread(chat);

    // The hook check runs the app server too, with no GROVE_SESSION_ID.
    let deadline = Instant::now() + TIMEOUT;
    let launches = loop {
        let log = s.log();
        let mut by_pid: HashMap<u64, (String, Value, Value)> = HashMap::new();
        for line in &log {
            let pid = line["pid"].as_u64().unwrap();
            if let Some(id) = line["env"]["GROVE_SESSION_ID"].as_str() {
                by_pid.entry(pid).or_default().0 = id.to_owned();
                by_pid.entry(pid).or_default().1 = line["argv"][1].clone();
            }
            if line.get("config").is_some() {
                by_pid.entry(pid).or_default().2 = line["config"].clone();
            }
        }
        let launches: HashMap<String, (Value, Value)> = by_pid
            .into_values()
            .filter(|(id, _, config)| !id.is_empty() && !config.is_null())
            .map(|(id, first, config)| (id, (first, config)))
            .collect();
        if launches.len() == 2 {
            break launches;
        }
        assert!(Instant::now() < deadline, "the two launches never logged");
        thread::sleep(Duration::from_millis(10));
    };

    let (first, mut chat_config) = launches[&chat.to_string()].clone();
    let (_, mut terminal_config) = launches[&terminal.to_string()].clone();
    assert_eq!(first, "app-server");
    let set = "/shell_environment_policy/set/GROVE_SESSION_ID";
    assert_eq!(chat_config.pointer(set).unwrap(), &json!(chat.to_string()));
    assert_eq!(
        terminal_config.pointer(set).unwrap(),
        &json!(terminal.to_string())
    );
    *chat_config.pointer_mut(set).unwrap() = Value::Null;
    *terminal_config.pointer_mut(set).unwrap() = Value::Null;
    assert_eq!(chat_config, terminal_config);
    assert!(chat_config["hooks"]["Stop"].is_array());
}

#[test]
fn the_model_list_comes_from_the_app_server() {
    let s = setup(json!([]));
    let session = s.spawn(None);

    let models = block_on(s.core.models(session).unwrap()).unwrap();

    // The fake's list, in its order.
    assert_eq!(
        models,
        [
            ModelInfo {
                id: "gpt-5.5".into(),
                name: "GPT-5.5".into(),
                description: "Frontier model".into(),
                efforts: vec!["low".into(), "medium".into(), "high".into()],
                default_effort: Some("medium".into()),
                is_default: true,
            },
            ModelInfo {
                id: "gpt-5.5-mini".into(),
                name: "GPT-5.5 mini".into(),
                description: "Smaller and faster".into(),
                efforts: vec!["low".into(), "medium".into()],
                default_effort: Some("low".into()),
                is_default: false,
            },
        ]
    );
    assert_eq!(s.wait_request("model/list")["params"], json!({}));
}

#[test]
fn set_model_applies_from_the_next_turn_start_leaving_the_running_turn_alone() {
    // The fake fills {{model}} and {{effort}} with what the turn runs with.
    let approval = json!({"itemId": "call_1", "command": "npm test"});
    let mut steps = vec![wait("turn/start"), turn_started()];
    steps.push(request(
        0,
        "item/commandExecution/requestApproval",
        approval,
    ));
    steps.push(wait("response"));
    steps.push(text_delta("{{model}} {{effort}}"));
    steps.push(turn_completed("completed", Value::Null));
    steps.extend(turn(&["{{model}} {{effort}}"]));
    let s = setup(json!(steps));
    let session = s.spawn(Some("one"));
    let mut frames = Frames::subscribe(&s.core, session);
    frames.until("the approval", |m| {
        matches!(m, AgentMessage::ApprovalRequested { .. })
    });

    s.core
        .set_model(session, "gpt-5.5-mini", Some("low"))
        .unwrap();
    s.core
        .answer(session, RequestId("call_1".into()), Reply::AllowOnce)
        .unwrap();
    frames.until_turn_end();
    s.core.send_message(session, "two").unwrap();
    frames.until_turn_end();

    let said: Vec<&AgentMessage> = frames
        .messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::TextDelta { .. }))
        .collect();
    assert_eq!(said, [&delta("gpt-5.5 medium"), &delta("gpt-5.5-mini low")]);
    let starts: Vec<Value> = s
        .sent()
        .into_iter()
        .filter(|m| m["method"] == "turn/start")
        .map(|m| m["params"].clone())
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(
        (&starts[0]["model"], &starts[0]["effort"]),
        (&Value::Null, &Value::Null)
    );
    assert_eq!(starts[1]["model"], "gpt-5.5-mini");
    assert_eq!(starts[1]["effort"], "low");
}

#[test]
fn set_model_before_the_first_message_applies_to_the_first_turn_and_keeps_to_later_ones() {
    let mut steps = turn(&["{{model}} {{effort}}"]);
    steps.extend(turn(&["{{model}} {{effort}}"]));
    let s = setup(json!(steps));
    let session = s.spawn(None);
    let mut frames = Frames::subscribe(&s.core, session);

    s.core.set_model(session, "gpt-5.5-mini", None).unwrap();
    s.core.send_message(session, "one").unwrap();
    frames.until_turn_end();
    s.core.send_message(session, "two").unwrap();
    frames.until_turn_end();

    let said: Vec<&AgentMessage> = frames
        .messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::TextDelta { .. }))
        .collect();
    // With no effort, Codex keeps the thread's.
    assert_eq!(
        said,
        [&delta("gpt-5.5-mini medium"), &delta("gpt-5.5-mini medium")]
    );
}

#[test]
fn a_tool_call_and_its_turn_from_the_app_server_carry_the_times_the_core_got_them() {
    let command = json!({"type": "commandExecution", "id": "call_1", "command": "npm test",
                         "cwd": "/w", "status": "inProgress", "aggregatedOutput": null,
                         "exitCode": null});
    let steps = vec![
        wait("turn/start"),
        turn_started(),
        item("item/started", command),
        turn_completed("completed", Value::Null),
    ];
    let s = setup(json!(steps));
    let session = s.spawn(None);
    let mut frames = Frames::subscribe(&s.core, session);
    let before = now_unix_ms();

    s.core.send_message(session, "go").unwrap();

    frames.until_turn_end();
    let after = now_unix_ms();
    let stamped = times(&frames.messages);
    assert_eq!(stamped.len(), 3, "a turn start, a tool call and a turn end");
    for time in stamped {
        let time = time.expect("each has a time");
        assert!(
            (before..=after).contains(&time),
            "{time} is not in {before}..={after}"
        );
    }
}

#[test]
fn codexs_enabled_skills_reach_the_panel_as_commands_and_the_list_after_a_skills_change_replaces_them()
 {
    let changed = json!({"send": {"method": "skills/changed", "params": {}}});
    let mut steps = turn(&["ok"]);
    steps.push(changed);
    let s = setup(json!(steps));
    let session = s.spawn(None);
    let mut frames = Frames::subscribe(&s.core, session);
    let is_list = |m: &AgentMessage| matches!(m, AgentMessage::Commands { .. });

    let first = frames.until("the skills", is_list);
    // Read by the fake at its next skills/list, which the change below asks for.
    let plan = json!([{"name": "plan", "description": "Plan the change", "enabled": true,
        "path": "/skills/plan/SKILL.md", "scope": "repo"}]);
    fs::write(&s.skills, plan.to_string()).unwrap();
    s.core.send_message(session, "go").unwrap();
    let second = frames.until("the skills after the change", is_list);

    let skill = |name: &str, description: &str| SlashCommand {
        name: name.into(),
        description: description.into(),
        argument_hint: String::new(),
        invocation: format!("${name}"),
    };
    assert_eq!(
        first,
        AgentMessage::Commands {
            commands: vec![skill("review", "Review the diff")]
        }
    );
    assert_eq!(
        second,
        AgentMessage::Commands {
            commands: vec![skill("plan", "Plan the change")]
        }
    );
    // A panel that attaches now gets the new list, after the conversation.
    let late = s.core.subscribe(session, unbounded_channel().0).unwrap();
    assert_eq!(late.last(), Some(&second));
}
