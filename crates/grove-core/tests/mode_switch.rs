//! Switching agent sessions between chat mode and terminal mode, against both fake agents in
//! fixtures/fake-agents. Each fake adds its turns to the conversation's record, as the real
//! agents do, and a resumed chat-mode process reads them back.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Barrier;
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, AgentMessage, AgentSpec, AgentState, Core, CoreConfig, CoreError, CoreEvent, InputKind,
    Mode, ModelChoice, SessionId, StateSource, TurnOutcome,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::sync::mpsc::unbounded_channel;

mod common;
use common::{conversation_only, grove_hook, untimed, write_script};

const TIMEOUT: Duration = Duration::from_secs(10);

// The core owns its runtime, and dropping it inside an async context panics.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn fake(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fake-agents")
        .join(name)
}

/// A core tracking one fresh repo, with the agent's TUI and its chat-mode process replaced by
/// their fakes. Every launch of one plays the same scenario.
struct Setup {
    _dir: TempDir,
    agent: Agent,
    core: Core,
    events: broadcast::Receiver<CoreEvent>,
    worktree: PathBuf,
    tui_log: PathBuf,
    chat_log: PathBuf,
}

fn setup(agent: Agent, chat: Vec<Value>, tui: Vec<Value>) -> Setup {
    setup_tuned(agent, chat, tui, |_| {})
}

/// Like `setup`, with `tune` changing the config first.
fn setup_tuned(
    agent: Agent,
    chat: Vec<Value>,
    tui: Vec<Value>,
    tune: impl FnOnce(&mut CoreConfig),
) -> Setup {
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

    let path = |name: &str| dir.path().join(name);
    let scenario = |name: &str, steps: Vec<Value>| {
        fs::write(path(name), json!({ "steps": steps }).to_string()).unwrap();
        path(name).display().to_string()
    };
    let (chat, tui) = (scenario("chat.json", chat), scenario("tui.json", tui));
    let record = path("record");
    fs::create_dir(&record).unwrap();
    let (tui_log, chat_log) = (path("tui.log"), path("chat.log"));
    let wrapper = |name: &str, lines: String, fake: PathBuf| {
        let wrapper = path(name);
        write_script(
            &wrapper,
            &format!("{lines}\nexec '{}' \"$@\"", fake.display()),
        );
        wrapper
    };
    let mut cfg = CoreConfig::new(path("home"));
    cfg.hook_relay = grove_hook().to_path_buf();
    match agent {
        Agent::Claude => {
            let record = record.display();
            cfg.claude_command = wrapper(
                "claude",
                format!(
                    "export FAKE_CLAUDE_SCENARIO='{tui}' FAKE_CLAUDE_LOG='{}' FAKE_CLAUDE_RECORD='{record}'",
                    tui_log.display()
                ),
                fake("claude"),
            );
            cfg.claude_sidecar_command = wrapper(
                "claude-sidecar",
                format!(
                    "export FAKE_CLAUDE_SIDECAR_SCENARIO='{chat}' FAKE_CLAUDE_SIDECAR_LOG='{}' FAKE_CLAUDE_RECORD='{record}'",
                    chat_log.display()
                ),
                fake("claude-sidecar"),
            );
        }
        Agent::Codex => {
            // Codex hooks run through a login shell. A home of its own keeps the developer's
            // profile out.
            let user = path("user");
            fs::create_dir_all(user.join(".codex")).unwrap();
            let lines = format!(
                "export HOME='{0}' CODEX_HOME='{0}/.codex' SHELL=/bin/sh PYTHONDONTWRITEBYTECODE=1 \
                 FAKE_CODEX_SCENARIO='{tui}' FAKE_CODEX_APP_SERVER_SCENARIO='{chat}' \
                 FAKE_CODEX_RECORD='{1}' FAKE_CODEX_LOG='{2}'\n\
                 if [ \"$1\" = app-server ]; then export FAKE_CODEX_LOG='{3}'; fi",
                user.display(),
                record.display(),
                tui_log.display(),
                chat_log.display(),
            );
            cfg.codex_command = wrapper("codex", lines, fake("codex"));
        }
    }
    tune(&mut cfg);
    let (core, events) = Core::start(cfg).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    Setup {
        _dir: dir,
        agent,
        core,
        events,
        worktree,
        tui_log,
        chat_log,
    }
}

// A failed test shows what the fakes did.
impl Drop for Setup {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!(
                "TUI log:\n{}\nchat log:\n{}",
                fs::read_to_string(&self.tui_log).unwrap_or_default(),
                fs::read_to_string(&self.chat_log).unwrap_or_default()
            );
        }
    }
}

fn read_log(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Waits until `log` has `n` lines that `wanted` picks, and returns them.
fn wait_lines(log: &Path, n: usize, what: &str, wanted: impl Fn(&Value) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let lines: Vec<Value> = read_log(log).into_iter().filter(&wanted).collect();
        if lines.len() >= n {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "{} has {} of {n} {what}",
            log.display(),
            lines.len()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

/// The argv of every launch in `log`, without the fake's own path. The Codex hook check's app
/// servers are left out: they never start a thread.
fn launches(log: &Path) -> Vec<Vec<String>> {
    read_log(log)
        .iter()
        .filter_map(|line| line["argv"].as_array())
        .map(|argv| {
            argv[1..]
                .iter()
                .map(|a| a.as_str().unwrap().to_owned())
                .collect()
        })
        .collect()
}

fn the_value_after(argv: &[String], flag: &str) -> String {
    let at = argv.iter().position(|a| a == flag).unwrap();
    argv[at + 1].clone()
}

impl Setup {
    fn spawn_chat(&self) -> SessionId {
        self.spawn(Mode::Chat)
    }

    fn spawn(&self, mode: Mode) -> SessionId {
        let spec = AgentSpec {
            agent: Some(self.agent),
            mode,
            prompt: None,
            cols: 120,
            rows: 40,
        };
        self.core.spawn_agent(&self.worktree, spec).unwrap()
    }

    fn set_mode(&self, session: SessionId, mode: Mode) -> Result<(), CoreError> {
        block_on(self.core.set_mode(session, mode))
    }

    fn next_state(&mut self) -> (AgentState, StateSource) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(CoreEvent::AgentState { state, source, .. }) => return (state, source),
                Ok(_) => {}
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "no agent state came");
                    thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// One turn in chat mode: sends `text` and waits for the turn to end.
    fn chat_turn(&mut self, session: SessionId, text: &str) {
        self.core.send_message(session, text).unwrap();
        let protocol = StateSource::Protocol;
        assert_eq!(self.next_state(), (AgentState::Working, protocol));
        assert_eq!(self.next_state(), (AgentState::Done, protocol));
    }

    /// One turn in terminal mode, the `n`th launch of the TUI: types `text` once the TUI is
    /// up, then waits for the turn to end.
    fn terminal_turn(&mut self, session: SessionId, n: usize, text: &str) {
        // The fake's stdin is raw once its first hook has run, so Enter arrives as \r.
        wait_lines(&self.tui_log, n, "SessionStart hooks", |l| {
            l["hook"] == "SessionStart"
        });
        self.core
            .write(session, format!("{text}\r").as_bytes())
            .unwrap();
        assert_eq!(self.next_state(), (AgentState::Working, StateSource::Hook));
        assert_eq!(self.next_state(), (AgentState::Done, StateSource::Hook));
    }

    /// How many chat-mode conversations have started. The Codex hook check runs app servers
    /// too, but they never start a thread.
    fn chat_starts(&self) -> usize {
        match self.agent {
            Agent::Claude => launches(&self.chat_log).len(),
            Agent::Codex => read_log(&self.chat_log)
                .iter()
                .filter(|l| l["stdin"].as_str().is_some_and(|m| m.contains("\"thread/")))
                .count(),
        }
    }

    /// The conversation, with no times: these tests check what it holds and in what order.
    fn history(&self, session: SessionId) -> Vec<AgentMessage> {
        let (sink, _frames) = unbounded_channel();
        untimed(conversation_only(
            self.core.subscribe(session, sink).unwrap(),
        ))
    }
}

/// What one turn adds to a conversation, in chat mode and in the agent's record.
fn turn_messages(text: &str) -> Vec<AgentMessage> {
    vec![
        AgentMessage::User { text: text.into() },
        AgentMessage::TurnStarted { time_unix_ms: None },
        AgentMessage::TextDelta {
            text: format!("re: {text}"),
        },
        AgentMessage::TurnEnded {
            outcome: TurnOutcome::Completed,
            time_unix_ms: None,
        },
    ]
}

fn send(message: Value) -> Value {
    json!({ "send": message })
}

fn record(message: Value) -> Value {
    json!({ "record": message })
}

/// A Codex turn, as `thread/resume` returns it, for the user's last message.
fn codex_turn(id: &str) -> Value {
    json!({"id": id, "status": "completed", "error": null, "items": [
        {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": "{{text}}", "text_elements": []}]},
        {"type": "agentMessage", "id": "a", "text": "re: {{text}}"},
    ]})
}

/// Every chat-mode launch answers one message with "re: <message>", adds the turn to the
/// record, then waits until the core is done with it.
fn chat_scenario(agent: Agent) -> Vec<Value> {
    match agent {
        Agent::Claude => vec![
            json!({"wait_message": "user_message"}),
            record(json!({"type": "user", "text": "{{text}}"})),
            record(json!({"type": "turn_start"})),
            send(json!({"type": "turn_start"})),
            send(json!({"type": "text_delta", "text": "re: {{text}}"})),
            record(json!({"type": "text_delta", "text": "re: {{text}}"})),
            // Recorded before the core hears the turn ended, so a switch then finds it.
            record(json!({"type": "turn_end"})),
            send(json!({"type": "turn_end"})),
            json!({"wait_message": "never"}),
        ],
        Agent::Codex => {
            let notify =
                |method: &str, params: Value| send(json!({"method": method, "params": params}));
            let turn = json!({"id": "{{turn_id}}", "status": "inProgress"});
            vec![
                json!({"wait_message": "turn/start"}),
                notify(
                    "turn/started",
                    json!({"threadId": "{{thread_id}}", "turn": turn}),
                ),
                notify(
                    "item/agentMessage/delta",
                    json!({"threadId": "{{thread_id}}", "turnId": "{{turn_id}}", "itemId": "a", "delta": "re: {{text}}"}),
                ),
                record(codex_turn("{{turn_id}}")),
                notify(
                    "turn/completed",
                    json!({"threadId": "{{thread_id}}", "turn": {"id": "{{turn_id}}", "status": "completed"}}),
                ),
            ]
        }
    }
}

/// Every TUI launch answers one typed message the same way, with its hooks, then waits until
/// grove ends it.
fn tui_scenario(agent: Agent) -> Vec<Value> {
    let mut steps = vec![
        json!({"hook": "SessionStart-resume"}),
        json!({"wait_input": "\r"}),
        json!({"hook": "UserPromptSubmit"}),
    ];
    match agent {
        Agent::Claude => steps.extend([
            record(json!({"type": "user", "text": "{{text}}"})),
            record(json!({"type": "turn_start"})),
            record(json!({"type": "text_delta", "text": "re: {{text}}"})),
            record(json!({"type": "turn_end"})),
        ]),
        Agent::Codex => steps.push(record(codex_turn("tui"))),
    }
    steps.extend([json!({"hook": "Stop"}), json!({"wait_input": "never"})]);
    steps
}

fn switching_setup(agent: Agent) -> Setup {
    setup(agent, chat_scenario(agent), tui_scenario(agent))
}

fn chat_to_terminal_resumes_in_the_tui_with_the_integration_flags_and_back_in_chat_mode(
    agent: Agent,
) {
    let mut s = switching_setup(agent);
    let session = s.spawn_chat();
    s.chat_turn(session, "one");
    let resume_id = s.core.resume_id(session).unwrap();

    s.set_mode(session, Mode::Terminal).unwrap();
    s.terminal_turn(session, 1, "two");
    s.set_mode(session, Mode::Chat).unwrap();
    let history = s.history(session);
    s.chat_turn(session, "three");

    let tui = &launches(&s.tui_log)[0];
    let chat = launches(&s.chat_log);
    match agent {
        Agent::Claude => {
            let first = &chat[0];
            assert_eq!(
                *tui,
                [
                    "--settings".to_owned(),
                    the_value_after(first, "--settings"),
                    format!("--mcp-config={}", the_value_after(first, "--mcp-config")),
                    "--resume".to_owned(),
                    resume_id.clone(),
                ]
            );
            let mut resumed = first.clone();
            resumed.push("--resume".into());
            assert_eq!(chat[1], resumed);
        }
        Agent::Codex => {
            assert_eq!(tui[..3], ["resume", &resume_id, "--no-daemon"]);
            // The TUI gets the same hooks and MCP config as the app server.
            let configs: Vec<Value> = read_log(&s.tui_log)
                .into_iter()
                .chain(read_log(&s.chat_log))
                .filter_map(|line| line.get("config").cloned())
                .collect();
            assert!(configs.windows(2).all(|pair| pair[0] == pair[1]));
            let resumed = wait_lines(&s.chat_log, 1, "thread/resume", |l| {
                l["stdin"]
                    .as_str()
                    .is_some_and(|m| m.contains("thread/resume"))
            });
            let request: Value =
                serde_json::from_str(resumed[0]["stdin"].as_str().unwrap()).unwrap();
            assert_eq!(request["params"]["threadId"], resume_id.as_str());
        }
    }
    assert_eq!(s.core.resume_id(session).unwrap(), resume_id);
    let mut expected = turn_messages("one");
    expected.extend(turn_messages("two"));
    assert_eq!(history, expected);
}

#[test]
fn claude_chat_to_terminal_resumes_in_the_tui_with_the_integration_flags_and_back_in_chat_mode() {
    chat_to_terminal_resumes_in_the_tui_with_the_integration_flags_and_back_in_chat_mode(
        Agent::Claude,
    );
}

#[test]
fn codex_chat_to_terminal_resumes_in_the_tui_with_the_integration_flags_and_back_in_chat_mode() {
    chat_to_terminal_resumes_in_the_tui_with_the_integration_flags_and_back_in_chat_mode(
        Agent::Codex,
    );
}

/// The MCP servers grove passed each launch, as the fakes read them: a first launch in chat mode, a
/// resume in the TUI, a resume in chat mode, and a first launch in the TUI. For Claude, the
/// servers in each `mcp.json`; for Codex, the `mcp_servers` of each launch, the hook check's app
/// servers included.
fn mcp_servers_of_every_launch(agent: Agent, mcp_server: Option<PathBuf>) -> Vec<Value> {
    let mut s = setup_tuned(agent, chat_scenario(agent), tui_scenario(agent), |cfg| {
        cfg.mcp_server = mcp_server;
    });
    let session = s.spawn_chat();
    s.chat_turn(session, "one");
    s.set_mode(session, Mode::Terminal).unwrap();
    s.terminal_turn(session, 1, "two");
    s.set_mode(session, Mode::Chat).unwrap();
    s.chat_turn(session, "three");
    s.spawn(Mode::Terminal);
    wait_lines(&s.tui_log, 2, "SessionStart hooks", |l| {
        l["hook"] == "SessionStart"
    });

    let lines: Vec<Value> = read_log(&s.tui_log)
        .into_iter()
        .chain(read_log(&s.chat_log))
        .collect();
    match agent {
        Agent::Claude => {
            let servers: Vec<Value> = lines
                .iter()
                .filter(|l| l["file"].as_str().is_some_and(|f| f.ends_with("mcp.json")))
                .map(|l| {
                    let config: Value =
                        serde_json::from_str(l["content"].as_str().unwrap()).unwrap();
                    config["mcpServers"].clone()
                })
                .collect();
            assert_eq!(servers.len(), 4, "{lines:?}");
            servers
        }
        Agent::Codex => {
            // The fake logs the `-c` overrides of each launch as the config Codex would read.
            let servers: Vec<Value> = lines
                .iter()
                .filter_map(|l| l.get("config"))
                .map(|config| config.get("mcp_servers").cloned().unwrap_or(json!({})))
                .collect();
            // Two TUI launches and at least the two chat-mode app servers.
            assert!(servers.len() >= 4, "{lines:?}");
            servers
        }
    }
}

#[test]
fn claude_with_no_mcp_server_is_passed_no_grove_server_on_any_launch() {
    for servers in mcp_servers_of_every_launch(Agent::Claude, None) {
        assert_eq!(servers, json!({}));
    }
}

#[test]
fn codex_with_no_mcp_server_is_passed_no_grove_server_on_any_launch() {
    for servers in mcp_servers_of_every_launch(Agent::Codex, None) {
        assert_eq!(servers, json!({}));
    }
}

#[test]
fn claude_with_an_mcp_server_gets_it_on_every_launch() {
    let mcp_server = PathBuf::from("/opt/grove/grove-mcp");
    for servers in mcp_servers_of_every_launch(Agent::Claude, Some(mcp_server.clone())) {
        assert_eq!(servers["grove"]["command"], mcp_server.to_str().unwrap());
        assert_eq!(servers.as_object().unwrap().len(), 1);
    }
}

#[test]
fn codex_with_an_mcp_server_gets_it_on_every_launch() {
    let mcp_server = PathBuf::from("/opt/grove/grove-mcp");
    for servers in mcp_servers_of_every_launch(Agent::Codex, Some(mcp_server.clone())) {
        assert_eq!(servers["grove"]["command"], mcp_server.to_str().unwrap());
        assert_eq!(servers.as_object().unwrap().len(), 1);
    }
}

fn after_three_round_trips_subscribe_returns_every_message_in_order(agent: Agent) {
    let mut s = switching_setup(agent);
    let session = s.spawn_chat();
    let mut expected = Vec::new();
    for round in 1..=3 {
        let (chat, terminal) = (format!("chat {round}"), format!("terminal {round}"));
        s.chat_turn(session, &chat);
        s.set_mode(session, Mode::Terminal).unwrap();
        s.terminal_turn(session, round, &terminal);
        s.set_mode(session, Mode::Chat).unwrap();
        expected.extend(turn_messages(&chat));
        expected.extend(turn_messages(&terminal));
    }

    assert_eq!(s.history(session), expected);
}

#[test]
fn claude_after_three_round_trips_subscribe_returns_every_message_in_order() {
    after_three_round_trips_subscribe_returns_every_message_in_order(Agent::Claude);
}

#[test]
fn codex_after_three_round_trips_subscribe_returns_every_message_in_order() {
    after_three_round_trips_subscribe_returns_every_message_in_order(Agent::Codex);
}

#[test]
fn a_history_the_core_cannot_read_does_not_leave_the_session_switching() {
    let mut s = switching_setup(Agent::Claude);
    let session = s.spawn_chat();
    s.chat_turn(session, "one");
    s.set_mode(session, Mode::Terminal).unwrap();
    s.terminal_turn(session, 1, "two");
    // A text_delta with no text makes the sidecar's history line one the core drops.
    let resume_id = s.core.resume_id(session).unwrap();
    let record = s
        .tui_log
        .with_file_name("record")
        .join(format!("{resume_id}.jsonl"));
    let mut lines = fs::read_to_string(&record).unwrap();
    lines.push_str("{\"type\": \"text_delta\"}\n");
    fs::write(&record, lines).unwrap();

    let switched = block_on(async {
        tokio::time::timeout(
            Duration::from_secs(30),
            s.core.set_mode(session, Mode::Chat),
        )
        .await
    });

    assert!(matches!(switched, Ok(Ok(()))), "{switched:?}");
    s.chat_turn(session, "three");
}

#[test]
fn a_caller_that_stops_waiting_on_set_mode_leaves_the_session_free_to_switch() {
    let mut s = switching_setup(Agent::Claude);
    let session = s.spawn_chat();
    s.chat_turn(session, "one");

    // Polled once, so it waits for the old process to exit, then dropped.
    let pending = block_on(async {
        let mut switch = std::pin::pin!(s.core.set_mode(session, Mode::Terminal));
        std::future::poll_fn(|cx| Poll::Ready(switch.as_mut().poll(cx).is_pending())).await
    });

    assert!(pending, "set_mode did not wait for the exit");
    assert_eq!(s.next_state(), (AgentState::Exited, StateSource::Protocol));
    s.set_mode(session, Mode::Terminal).unwrap();
    s.terminal_turn(session, 1, "two");
}

/// Asserts `set_mode` to terminal mode is refused in `state`, and the session is as it was.
fn refused_in(s: &mut Setup, session: SessionId, state: AgentState, mode: Mode) {
    let other = match mode {
        Mode::Chat => Mode::Terminal,
        Mode::Terminal => Mode::Chat,
    };
    let launched = (launches(&s.tui_log).len(), s.chat_starts());

    let refused = s.set_mode(session, other);

    assert!(
        matches!(refused, Err(CoreError::AgentBusy { session: refusing, state: busy })
            if refusing == session && busy == state),
        "{refused:?}"
    );
    assert_eq!((launches(&s.tui_log).len(), s.chat_starts()), launched);
    match mode {
        Mode::Chat => assert!(s.core.subscribe(session, unbounded_channel().0).is_ok()),
        Mode::Terminal => assert!(s.core.write(session, b"x").is_ok()),
    }
}

#[test]
fn set_mode_is_refused_while_a_chat_session_is_working_or_needs_input() {
    let approval =
        json!({"type": "approval_request", "id": "toolu_01", "tool": "Bash", "input": {}});
    let chat = vec![
        json!({"wait_message": "user_message"}),
        send(json!({"type": "turn_start"})),
        json!({"wait_message": "user_message"}),
        send(approval),
        json!({"wait_message": "never"}),
    ];
    let mut s = setup(Agent::Claude, chat, Vec::new());
    let session = s.spawn_chat();

    s.core.send_message(session, "one").unwrap();
    assert_eq!(s.next_state().0, AgentState::Working);
    refused_in(&mut s, session, AgentState::Working, Mode::Chat);
    s.core.send_message(session, "two").unwrap();
    let approval = AgentState::NeedsInput {
        kind: InputKind::Approval,
    };
    assert_eq!(s.next_state().0, approval);
    refused_in(&mut s, session, approval, Mode::Chat);
}

#[test]
fn set_mode_is_refused_while_a_terminal_session_is_working_or_needs_input() {
    let tui = vec![
        json!({"hook": "SessionStart-startup"}),
        json!({"hook": "UserPromptSubmit"}),
        json!({"wait_input": "\r"}),
        json!({"hook": "PermissionRequest-Bash"}),
        json!({"wait_input": "never"}),
    ];
    let mut s = setup(Agent::Codex, Vec::new(), tui);
    let spec = AgentSpec {
        agent: Some(Agent::Codex),
        mode: Mode::Terminal,
        prompt: None,
        cols: 120,
        rows: 40,
    };
    let session = s.core.spawn_agent(&s.worktree, spec).unwrap();

    assert_eq!(s.next_state(), (AgentState::Working, StateSource::Hook));
    refused_in(&mut s, session, AgentState::Working, Mode::Terminal);
    s.core.write(session, b"\r").unwrap();
    let approval = AgentState::NeedsInput {
        kind: InputKind::Approval,
    };
    assert_eq!(s.next_state(), (approval, StateSource::Hook));
    refused_in(&mut s, session, approval, Mode::Terminal);
}

fn set_mode_is_refused_while_a_sent_message_has_no_turn_yet(agent: Agent) {
    let s = setup(agent, vec![json!({"wait_message": "never"})], Vec::new());
    let session = s.spawn_chat();
    s.core.send_message(session, "wait for me").unwrap();

    let refused = s.set_mode(session, Mode::Terminal);

    assert!(
        matches!(refused, Err(CoreError::MessagePending(to)) if to == session),
        "{refused:?}"
    );
    assert_eq!(launches(&s.tui_log).len(), 0);
    assert_eq!(
        s.history(session),
        [AgentMessage::User {
            text: "wait for me".into()
        }]
    );
}

#[test]
fn claude_set_mode_is_refused_while_a_sent_message_has_no_turn_yet() {
    set_mode_is_refused_while_a_sent_message_has_no_turn_yet(Agent::Claude);
}

#[test]
fn codex_set_mode_is_refused_while_a_sent_message_has_no_turn_yet() {
    set_mode_is_refused_while_a_sent_message_has_no_turn_yet(Agent::Codex);
}

#[test]
fn set_mode_works_again_once_the_agent_starts_on_the_message() {
    // The fake starts the turn only once the core sends it a model, so the test picks when.
    let chat = vec![
        json!({"wait_message": "user_message"}),
        json!({"wait_message": "set_model"}),
        send(json!({"type": "turn_start"})),
        send(json!({"type": "turn_end"})),
        json!({"wait_message": "never"}),
    ];
    let mut s = setup(Agent::Claude, chat, tui_scenario(Agent::Claude));
    let session = s.spawn_chat();
    s.core.send_message(session, "one").unwrap();
    assert!(matches!(
        s.set_mode(session, Mode::Terminal),
        Err(CoreError::MessagePending(_))
    ));

    s.core.set_model(session, "opus", None).unwrap();

    assert_eq!(s.next_state().0, AgentState::Working);
    assert_eq!(s.next_state().0, AgentState::Done);
    s.set_mode(session, Mode::Terminal).unwrap();
}

// Run many times, so the send lands before, during and after the switch's own check.
fn a_send_racing_set_mode_reaches_the_old_process_or_fails(agent: Agent) {
    let s = setup(agent, vec![json!({"wait_message": "never"})], Vec::new());
    let core = &s.core;
    for _ in 0..10 {
        let session = s.spawn_chat();
        let start = Barrier::new(2);

        let (sent, switched) = thread::scope(|scope| {
            let sender = scope.spawn(|| {
                start.wait();
                core.send_message(session, "race")
            });
            start.wait();
            let switched = block_on(core.set_mode(session, Mode::Terminal));
            (sender.join().unwrap(), switched)
        });

        // The fake never starts a turn, so a message the old process got must stop the switch.
        let fine = matches!(
            (&sent, &switched),
            (Ok(()), Err(CoreError::MessagePending(_)))
                | (
                    Err(CoreError::SwitchingMode(_)
                        | CoreError::NotInChatMode(_)
                        | CoreError::SessionNotRunning(_),),
                    Ok(()),
                )
        );
        assert!(fine, "send: {sent:?}, set_mode: {switched:?}");
    }
}

#[test]
fn claude_a_send_racing_set_mode_reaches_the_old_process_or_fails() {
    a_send_racing_set_mode_reaches_the_old_process_or_fails(Agent::Claude);
}

#[test]
fn codex_a_send_racing_set_mode_reaches_the_old_process_or_fails() {
    a_send_racing_set_mode_reaches_the_old_process_or_fails(Agent::Codex);
}

/// The stdin lines the latest chat-mode process got, as JSON.
fn latest_chat_stdin(log: &Path) -> Vec<Value> {
    let lines = read_log(log);
    let start = lines.iter().rposition(|l| l.get("argv").is_some()).unwrap();
    lines[start..]
        .iter()
        .filter_map(|l| serde_json::from_str(l["stdin"].as_str()?).ok())
        .collect()
}

fn a_chosen_model_runs_in_the_tui_and_back_in_chat_mode(agent: Agent) {
    let mut s = switching_setup(agent);
    let session = s.spawn_chat();
    let (model, effort) = match agent {
        Agent::Claude => ("haiku", "low"),
        Agent::Codex => ("gpt-5.5-mini", "low"),
    };
    s.core.set_model(session, model, Some(effort)).unwrap();
    s.chat_turn(session, "one");

    s.set_mode(session, Mode::Terminal).unwrap();
    s.terminal_turn(session, 1, "two");
    s.set_mode(session, Mode::Chat).unwrap();
    s.chat_turn(session, "three");

    let tui = &launches(&s.tui_log)[0];
    let stdin = latest_chat_stdin(&s.chat_log);
    match agent {
        Agent::Claude => {
            assert_eq!(tui[tui.len() - 4..], ["--model", model, "--effort", effort]);
            let set_model = json!({"type": "set_model", "model": model, "effort": effort});
            let message = stdin.iter().position(|l| l["type"] == "user_message");
            assert_eq!(stdin.iter().position(|l| *l == set_model), Some(0));
            assert!(message.is_some(), "{stdin:?}");
        }
        Agent::Codex => {
            let at = tui.iter().position(|a| a == "-m").unwrap();
            assert_eq!(tui[at + 1], model);
            assert!(tui.contains(&format!("model_reasoning_effort=\"{effort}\"")));
            let turn = stdin.iter().find(|l| l["method"] == "turn/start").unwrap();
            assert_eq!(turn["params"]["model"], model);
            assert_eq!(turn["params"]["effort"], effort);
        }
    }
    assert_eq!(
        s.core.model_choice(session).unwrap(),
        Some(ModelChoice {
            model: model.into(),
            effort: Some(effort.into()),
        })
    );
}

#[test]
fn claude_a_chosen_model_runs_in_the_tui_and_back_in_chat_mode() {
    a_chosen_model_runs_in_the_tui_and_back_in_chat_mode(Agent::Claude);
}

#[test]
fn codex_a_chosen_model_runs_in_the_tui_and_back_in_chat_mode() {
    a_chosen_model_runs_in_the_tui_and_back_in_chat_mode(Agent::Codex);
}

fn a_side_question_is_refused_in_terminal_mode_with_the_reason(agent: Agent) {
    let s = setup(agent, Vec::new(), vec![json!({"wait_input": "never"})]);
    let session = s.spawn(Mode::Terminal);

    let refused = s
        .core
        .ask_side_question(session, "why?", unbounded_channel().0)
        .unwrap_err();

    assert!(matches!(refused, CoreError::NotInChatMode(id) if id == session));
    assert_eq!(
        refused.to_string(),
        format!("session {session} is not an agent session in chat mode")
    );
}

#[test]
fn claude_a_side_question_is_refused_in_terminal_mode_with_the_reason() {
    a_side_question_is_refused_in_terminal_mode_with_the_reason(Agent::Claude);
}

#[test]
fn codex_a_side_question_is_refused_in_terminal_mode_with_the_reason() {
    a_side_question_is_refused_in_terminal_mode_with_the_reason(Agent::Codex);
}
