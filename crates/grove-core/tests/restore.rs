//! Restore after a restart, against the fake agents in fixtures/fake-agents. A second core started
//! on the same home folder brings back agent sessions in their mode with the integration of their
//! first launch, shells as fresh shells, run sessions stopped, review comments, and each agent
//! session's workspace.
//! Agent session records are here too: each lasts until the user deletes it, so an ended one
//! comes back ended.

use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Barrier, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use grove_core::{
    Agent, AgentMessage, AgentPanel, AgentSessionInfo, AgentSpec, AgentState, CommentId, Core,
    CoreConfig, CoreError, CoreEvent, DiffBase, Mode, ModelChoice, NewReviewComment, NewWorktree,
    PanelKind, ProfileSettings, RemoveOpts, RepoSettings, ReviewComment, RunSession, RunSpec,
    SessionId, SessionInfo, SessionKind, TurnOutcome, Warning, Workspace,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::sync::mpsc::unbounded_channel;

mod common;
use common::grove_hook;

const TIMEOUT: Duration = Duration::from_secs(10);

// A child that another test's thread has forked keeps every open file of this process until it
// execs, grove.lock included. A core stopped in that window stays locked, and the next start
// fails with AlreadyRunning. Tests here restart cores, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

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

// Runs git with the user's config turned off, so their hooks or signing settings never apply.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Grove Test")
        .env("GIT_AUTHOR_EMAIL", "test@grove.invalid")
        .env("GIT_COMMITTER_NAME", "Grove Test")
        .env("GIT_COMMITTER_EMAIL", "test@grove.invalid")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed in {}", dir.display());
}

// "line 1" to "line n", one per line.
fn numbered(n: u32) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

/// What each fake plays, on every launch.
struct Scenarios {
    claude_tui: Value,
    claude_chat: Value,
    codex_tui: Value,
}

impl Default for Scenarios {
    // Each runs until grove stops it. Codex reports its conversation with its first hook.
    fn default() -> Self {
        Scenarios {
            claude_tui: json!([{"wait_input": "never typed"}]),
            claude_chat: json!([{"wait_message": "never"}]),
            codex_tui: json!([{"hook": "SessionStart-startup"}, {"wait_input": "never typed"}]),
        }
    }
}

/// A core tracking one repo, with every agent replaced by its fake, each with its own log.
struct Setup {
    dir: TempDir,
    cfg: CoreConfig,
    core: Option<Core>,
    events: broadcast::Receiver<CoreEvent>,
    worktree: PathBuf,
    claude_tui: PathBuf,
    claude_chat: PathBuf,
    codex_tui: PathBuf,
    // `codex app-server`, for chat mode and for the hook check of terminal mode.
    codex_chat: PathBuf,
}

fn setup(scenarios: Scenarios) -> Setup {
    setup_with(scenarios, |_| {})
}

// A setup whose core config `configure` changes first.
fn setup_with(scenarios: Scenarios, configure: impl FnOnce(&mut CoreConfig)) -> Setup {
    // macOS caps a socket path at 104 bytes, and its default temp folder is long.
    let dir = tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap();
    let path = |name: &str| dir.path().join(name);
    let repo = path("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), numbered(20)).unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);

    let scenario = |name: &str, steps: Value| {
        fs::write(path(name), json!({ "steps": steps }).to_string()).unwrap();
        path(name).display().to_string()
    };
    let claude_tui_scenario = scenario("claude-tui.json", scenarios.claude_tui);
    let claude_chat_scenario = scenario("claude-chat.json", scenarios.claude_chat);
    let codex_tui_scenario = scenario("codex-tui.json", scenarios.codex_tui);
    let record = path("record");
    fs::create_dir(&record).unwrap();
    let (claude_tui, claude_chat) = (path("claude-tui.log"), path("claude-chat.log"));
    let (codex_tui, codex_chat) = (path("codex-tui.log"), path("codex-chat.log"));
    let wrapper = |name: &str, lines: String, fake: PathBuf| {
        let wrapper = path(name);
        let script = format!("#!/bin/sh\n{lines}\nexec '{}' \"$@\"\n", fake.display());
        fs::write(&wrapper, script).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        wrapper
    };
    let mut cfg = CoreConfig::new(path("home"));
    cfg.hook_relay = grove_hook().to_path_buf();
    cfg.claude_command = wrapper(
        "claude",
        format!(
            "export FAKE_CLAUDE_SCENARIO='{claude_tui_scenario}' FAKE_CLAUDE_LOG='{}' FAKE_CLAUDE_RECORD='{}'",
            claude_tui.display(),
            record.display()
        ),
        fake("claude"),
    );
    cfg.claude_sidecar_command = wrapper(
        "claude-sidecar",
        format!(
            "export FAKE_CLAUDE_SIDECAR_SCENARIO='{claude_chat_scenario}' FAKE_CLAUDE_SIDECAR_LOG='{}' FAKE_CLAUDE_RECORD='{}'",
            claude_chat.display(),
            record.display()
        ),
        fake("claude-sidecar"),
    );
    // Codex hooks run through a login shell. A home of its own keeps the developer's profile out.
    let user = path("user");
    fs::create_dir_all(user.join(".codex")).unwrap();
    cfg.codex_command = wrapper(
        "codex",
        format!(
            "export HOME='{0}' CODEX_HOME='{0}/.codex' SHELL=/bin/sh PYTHONDONTWRITEBYTECODE=1 \
             FAKE_CODEX_SCENARIO='{codex_tui_scenario}' FAKE_CODEX_RECORD='{1}' FAKE_CODEX_LOG='{2}'\n\
             if [ \"$1\" = app-server ]; then export FAKE_CODEX_LOG='{3}'; fi",
            user.display(),
            record.display(),
            codex_tui.display(),
            codex_chat.display(),
        ),
        fake("codex"),
    );
    configure(&mut cfg);

    let (core, events) = Core::start(cfg.clone()).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    Setup {
        dir,
        cfg,
        core: Some(core),
        events,
        worktree,
        claude_tui,
        claude_chat,
        codex_tui,
        codex_chat,
    }
}

// A failed test shows what the fakes did.
impl Drop for Setup {
    fn drop(&mut self) {
        if thread::panicking() {
            for log in [
                &self.claude_tui,
                &self.claude_chat,
                &self.codex_tui,
                &self.codex_chat,
            ] {
                eprintln!(
                    "{}:\n{}",
                    log.display(),
                    fs::read_to_string(log).unwrap_or_default()
                );
            }
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

// Polls `check` until it gives Some, and panics after TIMEOUT.
fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// The argv of each of the first `n` launches in `log`, without the fake's own path.
fn wait_launches(log: &Path, n: usize) -> Vec<Vec<String>> {
    wait_for(&format!("{n} launches in {}", log.display()), || {
        let launches: Vec<Vec<String>> = read_log(log)
            .iter()
            .filter_map(|line| line["argv"].as_array())
            .map(|argv| {
                argv[1..]
                    .iter()
                    .map(|a| a.as_str().unwrap().to_owned())
                    .collect()
            })
            .collect();
        (launches.len() >= n).then_some(launches)
    })
}

fn agent_info(session: SessionId, agent: Agent, mode: Mode) -> SessionInfo {
    subagent_info(session, agent, mode, None)
}

fn subagent_info(
    session: SessionId,
    agent: Agent,
    mode: Mode,
    parent: Option<SessionId>,
) -> SessionInfo {
    SessionInfo {
        session,
        kind: SessionKind::Agent {
            agent,
            mode,
            parent,
        },
        running: true,
    }
}

fn panel(agent: Agent, session: SessionId, shown: bool) -> AgentPanel {
    AgentPanel {
        agent,
        session,
        shown,
    }
}

impl Setup {
    fn core(&self) -> &Core {
        self.core.as_ref().unwrap()
    }

    // Quits grove and starts it again on the same home folder.
    fn restart(&mut self) {
        drop(self.core.take());
        let (core, events) = Core::start(self.cfg.clone()).unwrap();
        self.core = Some(core);
        self.events = events;
    }

    fn spawn(&self, agent: Agent, mode: Mode) -> SessionId {
        self.spawn_in(&self.worktree, agent, mode)
    }

    fn spawn_in(&self, worktree: &Path, agent: Agent, mode: Mode) -> SessionId {
        let spec = AgentSpec {
            agent: Some(agent),
            mode,
            prompt: None,
            cols: 100,
            rows: 30,
        };
        self.core().spawn_agent(worktree, spec).unwrap()
    }

    // A new worktree of the repo, on a new branch.
    fn add_worktree(&self, branch: &str) -> PathBuf {
        let new = NewWorktree {
            branch: branch.into(),
            ..NewWorktree::default()
        };
        block_on(self.core().create_worktree(&self.worktree, new))
            .unwrap()
            .worktree
            .path
    }

    // The worktree that `worktree` nests under, as the list says.
    fn nested_under(&self, worktree: &Path) -> Option<PathBuf> {
        block_on(self.core().list_worktrees())
            .into_iter()
            .find(|w| w.path == worktree)
            .unwrap()
            .nested_under
    }

    // Has `parent`, a Claude session in chat mode, ask grove for a Claude subagent the way
    // grove-mcp does, with its own token. Returns the subagent.
    fn spawn_subagent(&self, parent: SessionId) -> SessionId {
        self.spawn_subagent_in(parent, None)
    }

    // `spawn_subagent` in `worktree`, or else in the parent's own.
    fn spawn_subagent_in(&self, parent: SessionId, worktree: Option<&Path>) -> SessionId {
        let reply = self.ask_for_subagent_in(parent, worktree);
        let child: u64 = reply["result"]["session"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        serde_json::from_value(json!(child)).unwrap()
    }

    // The core's reply when `parent` asks for a subagent, with the token of its latest launch.
    fn ask_for_subagent(&self, parent: SessionId) -> Value {
        self.ask_for_subagent_in(parent, None)
    }

    fn ask_for_subagent_in(&self, parent: SessionId, worktree: Option<&Path>) -> Value {
        let token = wait_for("the parent's token", || {
            read_log(&self.claude_chat).iter().rev().find_map(|line| {
                let env = line.get("env")?;
                (env["GROVE_SESSION_ID"] == parent.to_string())
                    .then(|| env["GROVE_SESSION_TOKEN"].as_str().unwrap().to_owned())
            })
        });
        let mut ask = json!({"type": "subagent_spawn", "agent": "claude", "prompt": "hi"});
        if let Some(worktree) = worktree {
            ask["worktree"] = json!(worktree);
        }
        let request = json!({
            "session": parent.to_string(),
            "token": token,
            "request": ask,
        });
        let mut stream = UnixStream::connect(&self.cfg.socket).unwrap();
        stream.write_all(request.to_string().as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).unwrap();
        serde_json::from_slice(&reply).unwrap()
    }

    // The conversation the session goes on with, once the agent has reported it.
    fn resume_id(&self, session: SessionId) -> String {
        wait_for("a resume id", || self.core().resume_id(session))
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        self.core().sessions(&self.worktree).unwrap()
    }

    // The record of agent session `session`, as the list of all agent sessions has it.
    fn record(&self, session: SessionId) -> Option<AgentSessionInfo> {
        let mut listed = self.core().agent_sessions().into_iter();
        listed.find(|info| info.session == session)
    }

    // Ends the session's process and waits until the core has marked it ended.
    fn end(&mut self, session: SessionId) {
        self.core().kill(session).unwrap();
        self.wait_state(session, AgentState::Exited);
    }

    fn wait_state(&mut self, session: SessionId, state: AgentState) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(CoreEvent::AgentState {
                    session: s,
                    state: got,
                    ..
                }) if s == session && got == state => return,
                Ok(_) => {}
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "{session} never {state:?}");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    // Waits for the event that says the list of agent sessions changed.
    fn wait_list_changed(&mut self) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(CoreEvent::AgentSessionsChanged) => return,
                Ok(_) => {}
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "the list never changed");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    // Drops the events so far, so a wait sees only what comes next.
    fn drain_events(&mut self) {
        while self.events.try_recv().is_ok() {}
    }

    // The thread the latest `codex app-server` was asked to resume.
    fn thread_resumed(&self) -> String {
        wait_for("thread/resume", || {
            read_log(&self.codex_chat).iter().rev().find_map(|line| {
                let request: Value = serde_json::from_str(line["stdin"].as_str()?).ok()?;
                (request["method"] == "thread/resume")
                    .then(|| request["params"]["threadId"].as_str().unwrap().to_owned())
            })
        })
    }

    fn comment(&self, start_line: u32, end_line: u32, body: &str) -> CommentId {
        let new = NewReviewComment {
            path: "a.txt".into(),
            start_line,
            end_line,
            body: body.into(),
        };
        block_on(self.core().add_comment(&self.worktree, new)).unwrap()
    }
}

#[test]
fn claude_in_terminal_mode_and_codex_in_chat_mode_come_back_resumed_with_their_first_flags() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let claude = s.spawn(Agent::Claude, Mode::Terminal);
    let codex = s.spawn(Agent::Codex, Mode::Chat);
    let claude_id = s.resume_id(claude);
    let codex_id = s.resume_id(codex);
    let first_tui = wait_launches(&s.claude_tui, 1).remove(0);
    let first_chat = wait_launches(&s.codex_chat, 1).remove(0);

    s.restart();

    let mut resumed = first_tui;
    let at = resumed.iter().position(|a| a == "--session-id").unwrap();
    resumed[at] = "--resume".into();
    assert_eq!(resumed[at + 1], claude_id);
    assert_eq!(wait_launches(&s.claude_tui, 2)[1], resumed);
    assert_eq!(wait_launches(&s.codex_chat, 2)[1], first_chat);
    assert_eq!(s.thread_resumed(), codex_id);
    assert_eq!(
        s.sessions(),
        [
            agent_info(claude, Agent::Claude, Mode::Terminal),
            agent_info(codex, Agent::Codex, Mode::Chat),
        ]
    );
    assert_eq!(s.core().resume_id(claude), Some(claude_id));
    assert_eq!(s.core().resume_id(codex), Some(codex_id));
}

#[test]
fn claude_in_chat_mode_and_codex_in_terminal_mode_come_back_resumed_with_their_first_flags() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    let codex = s.spawn(Agent::Codex, Mode::Terminal);
    s.resume_id(claude);
    let codex_id = s.resume_id(codex);
    let first_chat = wait_launches(&s.claude_chat, 1).remove(0);
    let first_tui = wait_launches(&s.codex_tui, 1).remove(0);

    s.restart();

    let mut resumed = first_chat;
    resumed.push("--resume".into());
    assert_eq!(wait_launches(&s.claude_chat, 2)[1], resumed);
    let mut resumed = vec!["resume".to_owned(), codex_id.clone()];
    resumed.extend(first_tui);
    assert_eq!(wait_launches(&s.codex_tui, 2)[1], resumed);
    assert_eq!(
        s.sessions(),
        [
            agent_info(claude, Agent::Claude, Mode::Chat),
            agent_info(codex, Agent::Codex, Mode::Terminal),
        ]
    );
    assert_eq!(s.core().resume_id(codex), Some(codex_id));
}

/// Every Claude chat launch answers one message with "re: <message>" and adds the turn to its
/// record, as Claude writes its record only once the first message is sent.
fn one_recorded_turn() -> Value {
    let record = |message: Value| json!({ "record": message });
    json!([
        {"wait_message": "user_message"},
        record(json!({"type": "user", "text": "{{text}}"})),
        record(json!({"type": "turn_start"})),
        {"send": {"type": "turn_start"}},
        {"send": {"type": "text_delta", "text": "re: {{text}}"}},
        record(json!({"type": "text_delta", "text": "re: {{text}}"})),
        record(json!({"type": "turn_end"})),
        {"send": {"type": "turn_end"}},
        {"wait_message": "never"},
    ])
}

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

#[test]
fn a_claude_chat_session_that_never_got_a_message_comes_back_fresh_in_its_panel() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: one_recorded_turn(),
        ..Scenarios::default()
    });
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    let workspace = Workspace {
        chat: vec![panel(Agent::Claude, claude, true)],
        panels: vec![PanelKind::Diff],
        selected: Some(PanelKind::Diff),
        terminal: false,
        diff_base: DiffBase::Head,
    };
    block_on(s.core().set_workspace(claude, workspace.clone(), None)).unwrap();
    wait_launches(&s.claude_chat, 1);

    s.restart();

    wait_launches(&s.claude_chat, 2);
    assert_eq!(
        s.sessions(),
        [agent_info(claude, Agent::Claude, Mode::Chat)]
    );
    assert_eq!(s.core().workspace(claude).unwrap(), workspace);
    let (sink, _frames) = unbounded_channel();
    assert_eq!(s.core().subscribe(claude, sink).unwrap(), []);
    s.core().send_message(claude, "hi").unwrap();
    s.wait_state(claude, AgentState::Done);
    s.core().focus(claude);
    s.wait_state(claude, AgentState::Idle);
}

#[test]
fn a_claude_chat_session_with_messages_comes_back_resumed_with_its_history() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: one_recorded_turn(),
        ..Scenarios::default()
    });
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    s.core().send_message(claude, "fix the bug").unwrap();
    s.wait_state(claude, AgentState::Done);
    let first = wait_launches(&s.claude_chat, 1).remove(0);

    s.restart();

    let mut resumed = first;
    resumed.push("--resume".into());
    assert_eq!(wait_launches(&s.claude_chat, 2)[1], resumed);
    let history = wait_for("the history", || {
        let (sink, _frames) = unbounded_channel();
        let history = s.core().subscribe(claude, sink).ok()?;
        (!history.is_empty()).then_some(history)
    });
    // The record has no times, as a record from before grove read them, so the history has none.
    assert_eq!(history, turn_messages("fix the bug"));
}

#[test]
fn a_restored_claude_chat_session_keeps_the_times_its_record_has_not_the_time_of_the_restore() {
    let _serial = serial();
    let (started, called, ended) = (
        1_763_560_920_000_u64,
        1_763_560_921_500_u64,
        1_763_560_983_000_u64,
    );
    let record = |message: Value| json!({ "record": message });
    let input = json!({"file_path": "a.rs"});
    let mut s = setup(Scenarios {
        claude_chat: json!([
            {"wait_message": "user_message"},
            record(json!({"type": "user", "text": "{{text}}"})),
            record(json!({"type": "turn_start", "time_unix_ms": started})),
            record(json!({"type": "tool_call", "id": "toolu_1", "tool": "Read", "input": input,
                          "time_unix_ms": called})),
            record(json!({"type": "turn_end", "time_unix_ms": ended})),
            {"send": {"type": "turn_start"}},
            {"send": {"type": "turn_end"}},
            {"wait_message": "never"},
        ]),
        ..Scenarios::default()
    });
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    s.core().send_message(claude, "read it").unwrap();
    s.wait_state(claude, AgentState::Done);

    s.restart();

    let history = wait_for("the history", || {
        let (sink, _frames) = unbounded_channel();
        let history = s.core().subscribe(claude, sink).ok()?;
        (!history.is_empty()).then_some(history)
    });
    assert_eq!(
        history,
        [
            AgentMessage::User {
                text: "read it".into()
            },
            AgentMessage::TurnStarted {
                time_unix_ms: Some(started)
            },
            AgentMessage::ToolCall {
                id: "toolu_1".into(),
                tool: "Read".into(),
                input,
                time_unix_ms: Some(called),
            },
            AgentMessage::TurnEnded {
                outcome: TurnOutcome::Completed,
                time_unix_ms: Some(ended),
            },
        ]
    );
}

#[test]
fn a_subagent_lists_its_parent_and_comes_back_with_it_after_a_restart() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let parent = s.spawn(Agent::Claude, Mode::Chat);

    let child = s.spawn_subagent(parent);

    let listed = [
        agent_info(parent, Agent::Claude, Mode::Chat),
        subagent_info(child, Agent::Claude, Mode::Chat, Some(parent)),
    ];
    assert_eq!(s.sessions(), listed);
    // Both have started before grove quits, and both start again after.
    wait_launches(&s.claude_chat, 2);
    s.restart();
    wait_launches(&s.claude_chat, 4);
    assert_eq!(s.sessions(), listed);
}

#[test]
fn a_subagent_in_another_worktree_nests_it_under_the_parents_and_that_outlives_both_and_a_restart()
{
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let other = s.add_worktree("feat-nested");
    let parent = s.spawn(Agent::Claude, Mode::Chat);
    assert_eq!(s.nested_under(&other), None);

    let child = s.spawn_subagent_in(parent, Some(&other));

    assert_eq!(s.nested_under(&other), Some(s.worktree.clone()));
    assert_eq!(s.nested_under(&s.worktree), None);
    s.core().kill(child).unwrap();
    s.core().kill(parent).unwrap();
    s.wait_state(parent, AgentState::Exited);
    assert_eq!(s.nested_under(&other), Some(s.worktree.clone()));
    s.restart();
    assert_eq!(s.nested_under(&other), Some(s.worktree.clone()));
}

#[test]
fn a_worktree_keeps_the_first_worktree_it_nests_under_and_a_subagent_in_its_parents_own_nests_nothing()
 {
    let _serial = serial();
    let s = setup(Scenarios::default());
    let other = s.add_worktree("feat-nested");
    let third = s.add_worktree("feat-third");
    let first_parent = s.spawn(Agent::Claude, Mode::Chat);
    s.spawn_subagent_in(first_parent, Some(&other));
    let second_parent = s.spawn_in(&third, Agent::Claude, Mode::Chat);

    s.spawn_subagent_in(second_parent, Some(&other));
    s.spawn_subagent(first_parent);

    assert_eq!(s.nested_under(&other), Some(s.worktree.clone()));
    assert_eq!(s.nested_under(&s.worktree), None);
    assert_eq!(s.nested_under(&third), None);
}

#[test]
fn a_worktree_nested_under_one_that_is_deleted_or_forgotten_nests_under_nothing_and_can_nest_again()
{
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let (deleted, forgotten) = (s.add_worktree("plan-a"), s.add_worktree("plan-b"));
    let (child_a, child_b) = (s.add_worktree("feat-a"), s.add_worktree("feat-b"));
    for (parent, child) in [(&deleted, &child_a), (&forgotten, &child_b)] {
        let session = s.spawn_in(parent, Agent::Claude, Mode::Chat);
        s.spawn_subagent_in(session, Some(child));
        assert_eq!(s.nested_under(child), Some(parent.clone()));
    }
    let delete = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };

    block_on(s.core().remove_worktree(&deleted, delete)).unwrap();
    block_on(s.core().remove_worktree(&forgotten, RemoveOpts::Forget)).unwrap();

    assert_eq!(s.nested_under(&child_a), None);
    assert_eq!(s.nested_under(&child_b), None);
    s.restart();
    assert_eq!(s.nested_under(&child_a), None);
    // A worktree added at the deleted one's path is a stranger, and nothing nests under it.
    assert_eq!(s.add_worktree("plan-a"), deleted);
    assert_eq!(s.nested_under(&child_a), None);
    let main = s.spawn(Agent::Claude, Mode::Chat);
    s.spawn_subagent_in(main, Some(&child_a));
    assert_eq!(s.nested_under(&child_a), Some(s.worktree.clone()));
}

#[test]
fn a_subagent_that_would_close_a_loop_of_nesting_nests_nothing() {
    let _serial = serial();
    let s = setup(Scenarios::default());
    let (a, b, c) = (
        s.add_worktree("a"),
        s.add_worktree("b"),
        s.add_worktree("c"),
    );
    let in_a = s.spawn_in(&a, Agent::Claude, Mode::Chat);
    s.spawn_subagent_in(in_a, Some(&b));
    let in_b = s.spawn_in(&b, Agent::Claude, Mode::Chat);
    s.spawn_subagent_in(in_b, Some(&c));

    // Straight back, and back through the chain.
    s.spawn_subagent_in(in_b, Some(&a));
    let in_c = s.spawn_in(&c, Agent::Claude, Mode::Chat);
    s.spawn_subagent_in(in_c, Some(&a));

    assert_eq!(s.nested_under(&a), None);
    assert_eq!(s.nested_under(&b), Some(a.clone()));
    assert_eq!(s.nested_under(&c), Some(b.clone()));
}

#[test]
fn a_subagent_in_another_repo_or_in_a_main_worktree_nests_nothing() {
    let _serial = serial();
    let s = setup(Scenarios::default());
    let feat = s.add_worktree("feat");
    let other = s.dir.path().join("other");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    git(&other, &["commit", "-q", "--allow-empty", "-m", "first"]);
    let other = other.canonicalize().unwrap();
    block_on(s.core().add_repo(&other)).unwrap();
    let in_other = s.spawn_in(&other, Agent::Claude, Mode::Chat);
    let in_feat = s.spawn_in(&feat, Agent::Claude, Mode::Chat);

    s.spawn_subagent_in(in_other, Some(&feat));
    s.spawn_subagent_in(in_feat, Some(&s.worktree));

    assert_eq!(s.nested_under(&feat), None);
    assert_eq!(s.nested_under(&s.worktree), None);
    // The refused one used up nothing.
    let main = s.spawn(Agent::Claude, Mode::Chat);
    s.spawn_subagent_in(main, Some(&feat));
    assert_eq!(s.nested_under(&feat), Some(s.worktree.clone()));
}

#[test]
fn a_subagent_three_levels_deep_is_still_refused_one_after_a_restart() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let top = s.spawn(Agent::Claude, Mode::Chat);
    let first = s.spawn_subagent(top);
    let second = s.spawn_subagent(first);
    let third = s.spawn_subagent(second);
    wait_launches(&s.claude_chat, 4);

    s.restart();
    wait_launches(&s.claude_chat, 8);

    let reply = s.ask_for_subagent(third);
    assert_eq!(
        reply["error"],
        format!(
            "session {third} is a subagent 3 levels deep, the most grove allows, so it cannot \
             start another"
        )
    );
    assert_eq!(s.sessions().len(), 4);
}

#[test]
fn a_parent_at_the_subagent_limit_is_refused_one_more_until_one_of_its_subagents_ends() {
    let _serial = serial();
    let mut s = setup_with(Scenarios::default(), |cfg| {
        cfg.max_live_subagents_per_parent = 2;
    });
    let parent = s.spawn(Agent::Claude, Mode::Chat);
    let first = s.spawn_subagent(parent);
    s.spawn_subagent(parent);
    s.drain_events();

    let reply = s.ask_for_subagent(parent);

    assert_eq!(
        reply["error"],
        format!(
            "session {parent} has 2 live subagents, the most grove allows, so it cannot start \
             another until one ends"
        )
    );
    assert_eq!(s.core().agent_sessions().len(), 3);
    let panel_requested = std::iter::from_fn(|| s.events.try_recv().ok())
        .any(|event| matches!(event, CoreEvent::PanelRequested { .. }));
    assert!(!panel_requested);

    s.end(first);
    let another = s.spawn_subagent(parent);

    assert_eq!(s.record(another).unwrap().parent, Some(parent));
    assert_eq!(s.core().agent_sessions().len(), 4);
}

#[test]
fn a_chain_whose_middle_session_had_ended_still_counts_its_full_depth_after_a_restart() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let top = s.spawn(Agent::Claude, Mode::Chat);
    let first = s.spawn_subagent(top);
    let second = s.spawn_subagent(first);
    let third = s.spawn_subagent(second);
    wait_launches(&s.claude_chat, 4);
    s.end(second);

    s.restart();
    // The ended one in the middle gets no process.
    wait_launches(&s.claude_chat, 7);
    assert!(s.record(second).unwrap().ended);

    let reply = s.ask_for_subagent(third);
    assert_eq!(
        reply["error"],
        format!(
            "session {third} is a subagent 3 levels deep, the most grove allows, so it cannot \
             start another"
        )
    );
    assert_eq!(s.core().agent_sessions().len(), 4);
}

#[test]
fn a_subagent_saved_before_grove_kept_parents_comes_back_with_none() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let parent = s.spawn(Agent::Claude, Mode::Chat);
    let child = s.spawn_subagent(parent);
    drop(s.core.take());
    // The state file as grove wrote it before version 6: no parent on any session.
    let path = s.cfg.home.join("state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fn forget_parents(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.remove("parent");
                object.values_mut().for_each(forget_parents);
            }
            Value::Array(items) => items.iter_mut().for_each(forget_parents),
            _ => {}
        }
    }
    forget_parents(&mut state);
    state["version"] = json!(5);
    fs::write(&path, state.to_string()).unwrap();

    let (core, _events) = Core::start(s.cfg.clone()).unwrap();
    s.core = Some(core);

    assert_eq!(
        s.sessions(),
        [
            agent_info(parent, Agent::Claude, Mode::Chat),
            agent_info(child, Agent::Claude, Mode::Chat),
        ]
    );
}

#[test]
fn an_agent_session_with_no_resume_id_comes_back_as_a_fresh_conversation() {
    let _serial = serial();
    // Codex reports its conversation only with a hook, and this one runs none.
    let mut s = setup(Scenarios {
        codex_tui: json!([{"wait_input": "never typed"}]),
        ..Scenarios::default()
    });
    let codex = s.spawn(Agent::Codex, Mode::Terminal);
    let first = wait_launches(&s.codex_tui, 1).remove(0);
    assert_eq!(s.core().resume_id(codex), None);

    s.restart();

    assert_eq!(wait_launches(&s.codex_tui, 2)[1], first);
    assert_eq!(
        s.sessions(),
        [agent_info(codex, Agent::Codex, Mode::Terminal)]
    );
    assert_eq!(s.core().resume_id(codex), None);
}

#[test]
fn a_shell_comes_back_fresh_in_its_worktree_and_a_run_session_comes_back_stopped() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let dev = RunSession {
        name: "dev".into(),
        command: "sleep 600".into(),
        port: Some(3000),
    };
    let settings = RepoSettings {
        profile: None,
        overrides: ProfileSettings {
            run_sessions: Some(vec![dev]),
            ..ProfileSettings::default()
        },
    };
    block_on(s.core().set_repo_settings(&s.worktree, settings)).unwrap();
    let shell = s.core().spawn_shell(&s.worktree, 80, 24).unwrap();
    let run_spec = || RunSpec {
        name: "dev".into(),
        cols: 80,
        rows: 24,
    };
    let run = s.core().spawn_run(&s.worktree, run_spec()).unwrap();

    s.restart();

    let stopped = SessionInfo {
        session: run,
        kind: SessionKind::Run { name: "dev".into() },
        running: false,
    };
    let shell_info = SessionInfo {
        session: shell,
        kind: SessionKind::Shell,
        running: true,
    };
    assert_eq!(s.sessions(), [shell_info.clone(), stopped]);
    let out = s.dir.path().join("pwd.out");
    let command = format!("pwd -P > '{}'\n", out.display());
    s.core().write(shell, command.as_bytes()).unwrap();
    let worktree = s.worktree.display().to_string();
    wait_for("the shell's pwd", || {
        fs::read_to_string(&out)
            .ok()
            .filter(|pwd| pwd.trim_end() == worktree)
    });
    let (sink, _frames) = unbounded_channel();
    assert!(matches!(
        s.core().attach(run, sink),
        Err(CoreError::SessionNotRunning(id)) if id == run
    ));
    assert_eq!(s.core().port(run), None);

    // Starting it again replaces the stopped one.
    let again = s.core().spawn_run(&s.worktree, run_spec()).unwrap();
    assert!(again != run && again != shell);
    let running = SessionInfo {
        session: again,
        kind: SessionKind::Run { name: "dev".into() },
        running: true,
    };
    assert_eq!(s.sessions(), [shell_info, running]);
}

// An exited run session keeps its output while grove runs, but only one running at quit comes
// back.
#[test]
fn a_run_session_that_exited_before_quit_does_not_come_back() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let settings = RepoSettings {
        profile: None,
        overrides: ProfileSettings {
            run_sessions: Some(vec![
                RunSession {
                    name: "tests".into(),
                    command: "true".into(),
                    port: None,
                },
                RunSession {
                    name: "dev".into(),
                    command: "sleep 600".into(),
                    port: None,
                },
            ]),
            ..ProfileSettings::default()
        },
    };
    block_on(s.core().set_repo_settings(&s.worktree, settings)).unwrap();
    let run_spec = |name: &str| RunSpec {
        name: name.into(),
        cols: 80,
        rows: 24,
    };
    let tests = s.core().spawn_run(&s.worktree, run_spec("tests")).unwrap();
    let stopped = |session, name: &str| SessionInfo {
        session,
        kind: SessionKind::Run { name: name.into() },
        running: false,
    };
    wait_for("the tests listed stopped", || {
        (s.sessions() == [stopped(tests, "tests")]).then_some(())
    });
    let dev = s.core().spawn_run(&s.worktree, run_spec("dev")).unwrap();

    s.restart();

    assert_eq!(s.sessions(), [stopped(dev, "dev")]);
}

// The terminal panel opens the worktree's shell with spawn_shell, so a restart never adds one.
#[test]
fn a_worktree_keeps_one_shell_across_restarts_until_it_ends() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let shell = s.core().spawn_shell(&s.worktree, 80, 24).unwrap();
    assert_eq!(s.core().spawn_shell(&s.worktree, 80, 24).unwrap(), shell);
    for _ in 0..3 {
        s.restart();
        assert_eq!(s.core().spawn_shell(&s.worktree, 100, 30).unwrap(), shell);
    }
    let shell_info = SessionInfo {
        session: shell,
        kind: SessionKind::Shell,
        running: true,
    };
    assert_eq!(s.sessions(), [shell_info]);

    s.core().kill(shell).unwrap();
    wait_for("the shell gone", || s.sessions().is_empty().then_some(()));
    assert_ne!(s.core().spawn_shell(&s.worktree, 80, 24).unwrap(), shell);
}

#[test]
fn two_calls_at_once_open_one_shell() {
    let _serial = serial();
    let s = setup(Scenarios::default());
    let both = Barrier::new(2);
    let open = || {
        both.wait();
        s.core().spawn_shell(&s.worktree, 80, 24).unwrap()
    };
    let (first, second) = thread::scope(|scope| {
        let first = scope.spawn(open);
        let second = scope.spawn(open);
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(first, second);
    let shell_info = SessionInfo {
        session: first,
        kind: SessionKind::Shell,
        running: true,
    };
    assert_eq!(s.sessions(), [shell_info]);
}

// While grove is closed, a line is added above every comment. Each one not resolved is found
// one line down at start.
#[test]
fn review_comments_come_back_with_their_anchors_and_states() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: json!([
            {"wait_message": "user_message"},
            {"send": {"type": "turn_start"}},
            {"send": {"type": "turn_end"}},
            {"wait_message": "never"},
        ]),
        ..Scenarios::default()
    });
    let agent = s.spawn(Agent::Claude, Mode::Chat);
    s.core().send_message(agent, "hi").unwrap();
    s.wait_state(agent, AgentState::Done);
    s.comment(2, 2, "sent");
    s.core()
        .send_review_batch(&s.worktree, Some(agent))
        .unwrap();
    let resolved = s.comment(4, 4, "resolved");
    s.core().resolve_comment(resolved).unwrap();
    s.comment(6, 7, "open");
    s.comment(9, 9, "possibly addressed");
    let file = s.worktree.join("a.txt");
    fs::write(&file, numbered(20).replace("line 9\n", "changed\n")).unwrap();
    let flagged = |core: &Core, worktree: &Path| {
        let comments = core.comments(worktree).unwrap();
        comments
            .iter()
            .any(|c| c.possibly_addressed)
            .then_some(comments)
    };
    let open = wait_for("the flag", || flagged(s.core(), &s.worktree));
    let closed = s.core().resolved_comments(&s.worktree).unwrap();
    assert_eq!(open.len(), 3);
    assert_eq!(closed.len(), 1);

    drop(s.core.take());
    let text = fs::read_to_string(&file).unwrap();
    fs::write(&file, format!("new first line\n{text}")).unwrap();
    s.restart();

    let moved: Vec<ReviewComment> = open
        .iter()
        .cloned()
        .map(|mut c| {
            if !c.possibly_addressed {
                c.start_line += 1;
                c.end_line += 1;
            }
            c
        })
        .collect();
    wait_for("the comments placed at start", || {
        (s.core().comments(&s.worktree).unwrap() == moved).then_some(())
    });
    assert_eq!(s.core().resolved_comments(&s.worktree).unwrap(), closed);
    let states: Vec<_> = moved.iter().map(|c| (c.body.as_str(), c.state)).collect();
    assert_eq!(
        states,
        [
            ("sent", grove_core::CommentState::Sent),
            ("open", grove_core::CommentState::Open),
            ("possibly addressed", grove_core::CommentState::Open),
        ]
    );
    // A new comment never takes a restored one's id.
    let new = s.comment(1, 1, "new");
    assert!(open.iter().chain(&closed).all(|c| c.id != new));
}

// Worktrees come back at the same paths, one added again after a Delete and one adopted again
// after a Forget, and neither gets the old comments back.
#[test]
fn comments_of_a_deleted_or_forgotten_worktree_stay_gone_after_a_restart() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let repo = s.worktree.clone();
    let add = |core: &Core, branch: &str| {
        let new = NewWorktree {
            branch: branch.into(),
            ..NewWorktree::default()
        };
        block_on(core.create_worktree(&repo, new))
            .unwrap()
            .worktree
            .path
    };
    let (deleted, forgotten) = (add(s.core(), "deleted"), add(s.core(), "forgotten"));
    for worktree in [&deleted, &forgotten] {
        let comment = NewReviewComment {
            path: "a.txt".into(),
            start_line: 1,
            end_line: 1,
            body: "rename this".into(),
        };
        block_on(s.core().add_comment(worktree, comment)).unwrap();
    }
    let delete = RemoveOpts::Delete {
        confirmed: false,
        delete_branch: false,
    };

    block_on(s.core().remove_worktree(&deleted, delete)).unwrap();
    assert_eq!(add(s.core(), "deleted"), deleted);
    block_on(s.core().remove_worktree(&forgotten, RemoveOpts::Forget)).unwrap();
    block_on(s.core().remove_worktree(&repo, RemoveOpts::ForgetRepo)).unwrap();
    block_on(s.core().add_repo(&repo)).unwrap();
    s.restart();

    assert_eq!(s.core().comments(&deleted).unwrap(), vec![]);
    assert_eq!(s.core().comments(&forgotten).unwrap(), vec![]);
}

#[test]
fn each_workspace_comes_back_with_its_panels_on_the_restored_sessions() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    // In the same place, with a workspace of its own.
    let codex = s.spawn(Agent::Codex, Mode::Chat);
    let helper = s.spawn_subagent(claude);
    let ended = s.spawn_subagent(claude);
    s.end(ended);
    let of_claude = Workspace {
        chat: vec![
            panel(Agent::Claude, helper, true),
            panel(Agent::Claude, ended, true),
            panel(Agent::Claude, claude, false),
        ],
        panels: vec![PanelKind::Diff, PanelKind::Logs],
        selected: Some(PanelKind::Logs),
        terminal: false,
        diff_base: DiffBase::Head,
    };
    let of_codex = Workspace {
        chat: vec![panel(Agent::Codex, codex, true)],
        panels: vec![PanelKind::Browser],
        selected: None,
        terminal: true,
        diff_base: DiffBase::Head,
    };
    block_on(s.core().set_workspace(claude, of_claude.clone(), None)).unwrap();
    block_on(s.core().set_workspace(codex, of_codex.clone(), None)).unwrap();

    s.restart();
    // The session that had ended keeps its id and its panel, so this one never shows in it.
    let shell = s.core().spawn_shell(&s.worktree, 80, 24).unwrap();

    assert!(![claude, codex, helper, ended].contains(&shell));
    assert_eq!(s.core().workspace(claude).unwrap(), of_claude);
    assert_eq!(s.core().workspace(codex).unwrap(), of_codex);
}

// Issue #68. The Diff panel shows uncommitted changes until the user picks the whole branch.
#[test]
fn a_new_diff_panel_opens_on_head_and_each_sessions_choice_of_base_comes_back() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let on_whole_branch = s.spawn(Agent::Claude, Mode::Chat);
    // In the same place, with a choice of its own.
    let on_uncommitted = s.spawn(Agent::Codex, Mode::Chat);
    let starting = s.core().starting_workspace(&s.worktree).unwrap();
    assert_eq!(starting.diff_base, DiffBase::Head);
    let opened = s.core().workspace(on_whole_branch).unwrap();
    assert_eq!(opened.diff_base, DiffBase::Head);
    let chosen = Workspace {
        diff_base: DiffBase::MergeBase,
        ..opened
    };
    block_on(
        s.core()
            .set_workspace(on_whole_branch, chosen.clone(), None),
    )
    .unwrap();

    s.restart();

    assert_eq!(s.core().workspace(on_whole_branch).unwrap(), chosen);
    let other = s.core().workspace(on_uncommitted).unwrap();
    assert_eq!(other.diff_base, DiffBase::Head);
    // A session started now opens on uncommitted changes, whatever the others chose.
    let new = s.spawn(Agent::Claude, Mode::Chat);
    assert_eq!(s.core().workspace(new).unwrap().diff_base, DiffBase::Head);
}

#[test]
fn a_restart_starts_a_process_only_for_the_sessions_that_had_not_ended() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let running = s.spawn(Agent::Claude, Mode::Chat);
    let ended = s.spawn(Agent::Claude, Mode::Chat);
    wait_launches(&s.claude_chat, 2);
    s.end(ended);

    s.restart();

    assert!(s.record(ended).unwrap().ended);
    assert!(!s.record(running).unwrap().ended);
    assert_eq!(
        s.sessions(),
        [agent_info(running, Agent::Claude, Mode::Chat)]
    );
    // The restore starts each process before `Core::start` returns, so one for the ended
    // session would be running by now.
    let killed = s.core().kill(ended);
    assert!(
        matches!(killed, Err(CoreError::SessionNotRunning(session)) if session == ended),
        "{killed:?}"
    );
    assert_eq!(wait_launches(&s.claude_chat, 3).len(), 3);
}

#[test]
fn a_new_session_after_a_restart_never_gets_the_id_of_an_ended_one() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let running = s.spawn(Agent::Claude, Mode::Chat);
    // The newest session, so its id is the one a restart would hand out next.
    let ended = s.spawn(Agent::Claude, Mode::Chat);
    s.end(ended);

    s.restart();
    let new = s.spawn(Agent::Claude, Mode::Chat);

    assert!(new > ended, "{new} after {ended}");
    let listed: Vec<SessionId> = s
        .core()
        .agent_sessions()
        .into_iter()
        .map(|info| info.session)
        .collect();
    assert_eq!(listed, [running, ended, new]);
}

#[test]
fn a_session_that_ended_on_an_error_comes_back_exited_and_out_of_the_attention_queue() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: json!([{"wait_message": "user_message"}, {"exit": 1}]),
        ..Scenarios::default()
    });
    let failed = s.spawn(Agent::Claude, Mode::Chat);
    s.core().send_message(failed, "boom").unwrap();
    s.wait_state(failed, AgentState::Failed);
    let waiting: Vec<SessionId> = s
        .core()
        .attention_queue()
        .iter()
        .map(|e| e.session)
        .collect();
    assert_eq!(waiting, [failed]);

    s.restart();

    let record = s.record(failed).unwrap();
    assert!(record.ended);
    assert_eq!(record.state, Some(AgentState::Exited));
    assert_eq!(s.core().attention_queue(), []);
}

#[test]
fn a_first_message_titles_the_session_and_says_the_list_changed() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    assert_eq!(s.record(claude).unwrap().title, "New session");
    s.drain_events();

    s.core()
        .send_message(claude, "\n  fix the login bug  \nand add a test")
        .unwrap();

    s.wait_list_changed();
    assert_eq!(s.record(claude).unwrap().title, "fix the login bug");
    // Only the first message titles it.
    s.core().send_message(claude, "also the logout").unwrap();
    assert_eq!(s.record(claude).unwrap().title, "fix the login bug");
}

#[test]
fn an_ended_session_comes_back_listed_and_ended_with_its_title_and_created_time() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let started = SystemTime::now() - Duration::from_millis(1);
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    s.core().send_message(claude, "fix the login bug").unwrap();
    let before = s.record(claude).unwrap();
    assert!(before.created >= started && before.created <= SystemTime::now());
    assert!(!before.ended);
    s.end(claude);

    s.restart();

    let after = s.record(claude).unwrap();
    assert_eq!(
        after,
        AgentSessionInfo {
            ended: true,
            state: Some(AgentState::Exited),
            since: after.since,
            ..before
        }
    );
    assert_eq!(after.title, "fix the login bug");
    assert_eq!(after.worktree, s.worktree);
}

// The agent sessions listed, oldest first.
fn listed(s: &Setup) -> Vec<SessionId> {
    let listed = s.core().agent_sessions().into_iter();
    listed.map(|info| info.session).collect()
}

fn not_running(killed: Result<(), CoreError>, session: SessionId) -> bool {
    matches!(killed, Err(CoreError::SessionNotRunning(s)) if s == session)
}

#[test]
fn deleting_a_running_session_leaves_no_process_record_panel_or_folder_of_it_or_its_subagents() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let kept = s.spawn(Agent::Claude, Mode::Chat);
    let parent = s.spawn(Agent::Claude, Mode::Chat);
    let child = s.spawn_subagent(parent);
    let grandchild = s.spawn_subagent(child);
    wait_launches(&s.claude_chat, 4);
    let folder = |session: SessionId| s.cfg.home.join("sessions").join(session.to_string());
    let folders = [parent, child, grandchild].map(folder);
    assert!(folders.iter().all(|folder| folder.is_dir()));
    let kept_folder = folder(kept);
    // Whether some workspace still has an agent panel for the session.
    let has_panel = |s: &Setup, session| {
        let listed = s.core().agent_sessions().into_iter();
        let mut workspaces = listed.filter_map(|info| s.core().workspace(info.session).ok());
        workspaces.any(|workspace| workspace.chat.iter().any(|p| p.session == session))
    };
    for session in [parent, child, grandchild, kept] {
        assert!(has_panel(&s, session));
    }
    s.drain_events();

    block_on(s.core().delete_agent_session(parent)).unwrap();

    assert_eq!(listed(&s), [kept]);
    assert_eq!(s.sessions(), [agent_info(kept, Agent::Claude, Mode::Chat)]);
    for session in [parent, child, grandchild] {
        assert!(not_running(s.core().kill(session), session));
    }
    for session in [parent, child, grandchild] {
        assert!(!has_panel(&s, session));
        // Its workspace went with its record.
        assert!(matches!(
            s.core().workspace(session),
            Err(CoreError::NotAnAgentSession(_))
        ));
    }
    assert!(has_panel(&s, kept));
    assert!(folders.iter().all(|folder| !folder.exists()));
    assert!(kept_folder.is_dir());
    s.wait_list_changed();
    let again = block_on(s.core().delete_agent_session(parent));
    assert!(
        matches!(again, Err(CoreError::NotAnAgentSession(session)) if session == parent),
        "{again:?}"
    );

    s.restart();

    assert_eq!(listed(&s), [kept]);
    assert!(!has_panel(&s, parent));
}

#[test]
fn deleting_an_ended_session_drops_its_record_and_folder() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let ended = s.spawn(Agent::Claude, Mode::Chat);
    let folder = s.cfg.home.join("sessions").join(ended.to_string());
    s.end(ended);
    s.restart();
    assert_eq!(listed(&s), [ended]);
    // It has no process, so no exit says the list changed. Only the delete does.
    s.drain_events();

    block_on(s.core().delete_agent_session(ended)).unwrap();

    assert_eq!(listed(&s), []);
    assert!(!folder.exists());
    s.wait_list_changed();
    s.restart();
    assert_eq!(listed(&s), []);
}

#[test]
fn deleting_a_session_when_the_state_cannot_be_saved_still_drops_its_record_and_folder() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let ended = s.spawn(Agent::Claude, Mode::Chat);
    let folder = s.cfg.home.join("sessions").join(ended.to_string());
    s.end(ended);
    // The state file's temp file cannot be made in a home folder that takes no new files.
    let home = s.cfg.home.clone();
    let writable = fs::metadata(&home).unwrap().permissions();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o555)).unwrap();

    let deleted = block_on(s.core().delete_agent_session(ended));

    fs::set_permissions(&home, writable).unwrap();
    assert!(matches!(deleted, Err(CoreError::Io { .. })), "{deleted:?}");
    assert_eq!(listed(&s), []);
    assert!(!folder.exists());
}

#[test]
fn resuming_an_ended_session_starts_it_again_under_the_same_id_with_the_layout_it_had() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: one_recorded_turn(),
        ..Scenarios::default()
    });
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    s.core().send_message(claude, "fix the bug").unwrap();
    s.wait_state(claude, AgentState::Done);
    let first = wait_launches(&s.claude_chat, 1).remove(0);
    let layout = Workspace {
        chat: vec![panel(Agent::Claude, claude, true)],
        panels: vec![PanelKind::Logs, PanelKind::Diff],
        selected: Some(PanelKind::Diff),
        terminal: false,
        diff_base: DiffBase::Head,
    };
    block_on(s.core().set_workspace(claude, layout.clone(), None)).unwrap();
    s.end(claude);
    // An ended session keeps its workspace, with its own panel.
    assert_eq!(s.core().workspace(claude).unwrap(), layout);
    s.restart();
    assert_eq!(s.core().workspace(claude).unwrap(), layout);
    s.drain_events();

    s.core().resume_agent_session(claude).unwrap();

    let mut resumed = first;
    resumed.push("--resume".into());
    assert_eq!(wait_launches(&s.claude_chat, 2)[1], resumed);
    assert_eq!(
        s.sessions(),
        [agent_info(claude, Agent::Claude, Mode::Chat)]
    );
    let record = s.record(claude).unwrap();
    assert!(!record.ended);
    assert_eq!(record.state, Some(AgentState::Idle));
    assert_eq!(record.title, "fix the bug");
    // The layout is the one it had, and no second panel is added.
    assert_eq!(s.core().workspace(claude).unwrap(), layout);
    s.wait_list_changed();
    let history = wait_for("the history", || {
        let (sink, _frames) = unbounded_channel();
        let history = s.core().subscribe(claude, sink).ok()?;
        (!history.is_empty()).then_some(history)
    });
    assert_eq!(history, turn_messages("fix the bug"));
    // Resuming a session that runs starts nothing more.
    s.core().resume_agent_session(claude).unwrap();
    s.core().send_message(claude, "and the test").unwrap();
    s.wait_state(claude, AgentState::Done);
    assert_eq!(wait_launches(&s.claude_chat, 2).len(), 2);
}

#[test]
fn resuming_a_session_whose_panel_the_workspace_dropped_gives_it_one_panel() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    s.end(claude);
    // A workspace saved without the ended session's panel.
    let mut seen = s.core().workspace(claude).unwrap();
    seen.chat.clear();
    block_on(s.core().set_workspace(claude, seen.clone(), None)).unwrap();
    s.drain_events();

    s.core().resume_agent_session(claude).unwrap();

    let mut with_panel = seen;
    with_panel.chat.push(panel(Agent::Claude, claude, true));
    assert_eq!(s.core().workspace(claude).unwrap(), with_panel);
    let changed = wait_for("the workspace change", || match s.events.try_recv() {
        Ok(CoreEvent::WorkspaceChanged { session, by }) => Some((session, by)),
        _ => None,
    });
    assert_eq!(changed, (claude, None));
}

#[test]
fn resuming_a_subagent_whose_panel_is_hidden_shows_that_panel_in_its_top_parents_workspace() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let parent = s.spawn(Agent::Claude, Mode::Chat);
    let claude = s.spawn_subagent(parent);
    let mut hidden = s.core().workspace(parent).unwrap();
    let last = hidden.chat.last_mut().unwrap();
    assert_eq!(*last, panel(Agent::Claude, claude, true));
    last.shown = false;
    block_on(s.core().set_workspace(parent, hidden.clone(), None)).unwrap();
    s.end(claude);

    s.core().resume_agent_session(claude).unwrap();

    let mut shown = hidden;
    shown.chat.last_mut().unwrap().shown = true;
    assert_eq!(s.core().workspace(parent).unwrap(), shown);
    // The shown panel is saved.
    wait_for("the panel saved as shown", || {
        let state: Value =
            serde_json::from_slice(&fs::read(s.cfg.home.join("state.json")).ok()?).ok()?;
        let saved = state.to_string();
        (!saved.contains(r#""shown":false"#)).then_some(())
    });
}

#[test]
fn a_resume_that_cannot_start_leaves_the_session_ended_and_saved_as_ended() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    s.end(claude);
    s.cfg.claude_sidecar_command = s.dir.path().join("no-such-sidecar");
    s.restart();

    let resumed = s.core().resume_agent_session(claude);

    assert!(resumed.is_err(), "{resumed:?}");
    let record = s.record(claude).unwrap();
    assert!(record.ended);
    assert_eq!(record.state, Some(AgentState::Exited));
    assert_eq!(s.sessions(), []);
    let id = claude.to_string().parse::<u64>().unwrap();
    wait_for("the session saved as ended", || {
        (saved_sessions(&s.cfg.home) == [(id, true)]).then_some(())
    });
    // It is not left resuming, so a later resume is tried and not refused.
    let again = s.core().resume_agent_session(claude);
    assert!(
        matches!(again, Err(ref e) if !matches!(e, CoreError::SwitchingMode(_))),
        "{again:?}"
    );
}

#[test]
fn a_resumed_session_whose_process_exits_at_once_ends_again_with_the_state_of_that_exit() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: json!([{"exit": 3}]),
        ..Scenarios::default()
    });
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    s.wait_state(claude, AgentState::Failed);

    s.core().resume_agent_session(claude).unwrap();

    // `Idle` from the resume, then the exit.
    s.wait_state(claude, AgentState::Idle);
    s.wait_state(claude, AgentState::Failed);
    let record = s.record(claude).unwrap();
    assert!(record.ended);
    assert_eq!(record.state, Some(AgentState::Failed));
}

#[test]
fn resuming_a_session_grove_does_not_know_is_refused() {
    let _serial = serial();
    let s = setup(Scenarios::default());
    let shell = s.core().spawn_shell(&s.worktree, 80, 24).unwrap();

    let resumed = s.core().resume_agent_session(shell);

    assert!(
        matches!(resumed, Err(CoreError::NotAnAgentSession(session)) if session == shell),
        "{resumed:?}"
    );
}

#[test]
fn removing_a_worktree_drops_the_records_and_folders_of_its_sessions() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let forgotten = s.add_worktree("forgotten");
    let deleted = s.add_worktree("deleted");
    let kept = s.spawn(Agent::Claude, Mode::Chat);
    let in_forgotten = s.spawn_in(&forgotten, Agent::Claude, Mode::Chat);
    let ended_in_forgotten = s.spawn_in(&forgotten, Agent::Claude, Mode::Chat);
    let in_deleted = s.spawn_in(&deleted, Agent::Claude, Mode::Chat);
    s.end(ended_in_forgotten);
    let dropped = [in_forgotten, ended_in_forgotten, in_deleted];
    let folders = dropped.map(|session| s.cfg.home.join("sessions").join(session.to_string()));
    assert_eq!(listed(&s).len(), 4);
    s.drain_events();

    block_on(s.core().remove_worktree(&forgotten, RemoveOpts::Forget)).unwrap();
    let delete = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };
    block_on(s.core().remove_worktree(&deleted, delete)).unwrap();

    assert_eq!(listed(&s), [kept]);
    assert!(folders.iter().all(|folder| !folder.exists()));
    s.wait_list_changed();
    s.restart();
    assert_eq!(listed(&s), [kept]);
}

#[test]
fn a_version_8_state_file_loads_with_every_session_listed() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: one_recorded_turn(),
        ..Scenarios::default()
    });
    let with_history = s.spawn(Agent::Claude, Mode::Chat);
    s.core().send_message(with_history, "fix the bug").unwrap();
    s.wait_state(with_history, AgentState::Done);
    let codex = s.spawn(Agent::Codex, Mode::Terminal);
    let codex_id = s.resume_id(codex);
    let child = s.spawn_subagent(with_history);
    let (history_id, child_id) = (s.resume_id(with_history), s.resume_id(child));
    drop(s.core.take());
    // The sessions as a version 8 grove wrote them, field by field, so this does not follow
    // what later versions write: no title, created time, ended flag or workspace.
    let path = s.cfg.home.join("state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(state["version"], json!(11));
    let id = |session: SessionId| session.to_string().parse::<u64>().unwrap();
    state["repos"][0]["main_worktree"]["sessions"] = json!([
        {"kind": "agent", "id": id(with_history), "agent": "claude", "mode": "chat",
         "resume_id": history_id, "cols": 100, "rows": 30},
        {"kind": "agent", "id": id(codex), "agent": "codex", "mode": "terminal",
         "resume_id": codex_id, "cols": 100, "rows": 30},
        {"kind": "agent", "id": id(child), "agent": "claude", "mode": "chat",
         "resume_id": child_id, "cols": 80, "rows": 24, "parent": id(with_history)},
    ]);
    state["version"] = json!(8);
    fs::write(&path, state.to_string()).unwrap();
    let loaded = SystemTime::now() - Duration::from_millis(1);

    let (core, events) = Core::start(s.cfg.clone()).unwrap();
    s.core = Some(core);
    s.events = events;

    let records = s.core().agent_sessions();
    let summary: Vec<_> = records
        .iter()
        .map(|r| (r.session, r.agent, r.mode, r.parent, r.ended))
        .collect();
    assert_eq!(
        summary,
        [
            (with_history, Agent::Claude, Mode::Chat, None, false),
            (codex, Agent::Codex, Mode::Terminal, None, false),
            (child, Agent::Claude, Mode::Chat, Some(with_history), false),
        ]
    );
    assert!(records.iter().all(|r| r.worktree == s.worktree));
    assert!(records.iter().all(|r| r.created >= loaded));
    assert_eq!(s.record(codex).unwrap().title, "New session");
    assert_eq!(s.core().resume_id(codex), Some(codex_id));
    // Its history titles it once it is read.
    wait_for("the title from the history", || {
        (s.record(with_history).unwrap().title == "fix the bug").then_some(())
    });
}

// What grove would restore if it stopped now, from the state file: each session's id, and
// whether it is saved as ended.
fn saved_sessions(home: &Path) -> Vec<(u64, bool)> {
    let state: Value =
        serde_json::from_slice(&fs::read(home.join("state.json")).unwrap_or_default())
            .unwrap_or(Value::Null);
    state["repos"][0]["main_worktree"]["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| (s["id"].as_u64().unwrap(), s["ended"] == json!(true)))
        .collect()
}

// So a crash keeps the sessions that ran, and starts no process for one that had ended.
#[test]
fn the_state_file_follows_sessions_as_they_start_and_end() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let home = s.cfg.home.clone();
    let agent = s.spawn(Agent::Claude, Mode::Chat);
    let shell = s.core().spawn_shell(&s.worktree, 80, 24).unwrap();
    let id = |session: SessionId| session.to_string().parse::<u64>().unwrap();

    wait_for("both saved", || {
        (saved_sessions(&home) == [(id(agent), false), (id(shell), false)]).then_some(())
    });
    s.core().kill(agent).unwrap();
    s.wait_state(agent, AgentState::Exited);
    // The agent session stays, marked ended. A shell that ends is gone.
    wait_for("the agent saved as ended", || {
        (saved_sessions(&home) == [(id(agent), true), (id(shell), false)]).then_some(())
    });
    s.core().kill(shell).unwrap();
    wait_for("the shell gone", || {
        (saved_sessions(&home) == [(id(agent), true)]).then_some(())
    });
}

#[test]
fn a_session_that_cannot_relaunch_is_left_out_with_a_warning() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let claude = s.spawn(Agent::Claude, Mode::Terminal);
    let codex = s.spawn(Agent::Codex, Mode::Chat);
    s.resume_id(codex);
    wait_launches(&s.claude_tui, 1);

    s.cfg.claude_command = s.dir.path().join("no-such-claude");
    s.restart();

    let warning = wait_for("the warning", || match s.events.try_recv() {
        Ok(CoreEvent::Warning(warning)) => Some(warning),
        _ => None,
    });
    assert!(
        s.core()
            .warnings()
            .iter()
            .any(|entry| entry.warning == warning)
    );
    let Warning::SessionNotRestored { worktree, reason } = warning else {
        panic!("{warning:?}");
    };
    assert_eq!(worktree, s.worktree);
    assert!(reason.contains("no-such-claude"), "{reason}");
    assert_eq!(s.sessions(), [agent_info(codex, Agent::Codex, Mode::Chat)]);
    assert!(s.core().resume_id(claude).is_none());
}

// Issue #15. A session that is left out is not saved again, so a later run can hand its id to a
// new session. Its panel goes with it, so the new session never shows in the place it had.
#[test]
fn a_subagent_that_cannot_relaunch_loses_its_panel_and_no_new_session_shows_in_it() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: one_recorded_turn(),
        ..Scenarios::default()
    });
    let parent = s.spawn(Agent::Claude, Mode::Chat);
    let child = s.spawn_subagent(parent);
    // Its prompt's turn ends first: a mode switch is refused during one.
    s.wait_state(child, AgentState::Done);
    block_on(s.core().set_mode(child, Mode::Terminal)).unwrap();
    wait_launches(&s.claude_tui, 1);
    let chat_of = |s: &Setup| s.core().workspace(parent).unwrap().chat;
    // Hidden and first: a place no new panel gets.
    let mut workspace = s.core().workspace(parent).unwrap();
    workspace.chat = vec![
        panel(Agent::Claude, child, false),
        panel(Agent::Claude, parent, true),
    ];
    block_on(s.core().set_workspace(parent, workspace, None)).unwrap();

    // Only the subagent, in terminal mode, runs this command.
    s.cfg.claude_command = s.dir.path().join("no-such-claude");
    s.restart();
    // The parent is back, with the token a subagent request needs.
    wait_launches(&s.claude_chat, 3);

    let only_parent = [panel(Agent::Claude, parent, true)];
    assert_eq!(chat_of(&s), only_parent);
    let same_run = s.spawn_subagent(parent);
    assert!(same_run > child, "{same_run} after {child}");
    let after_parent = |new| [only_parent[0].clone(), panel(Agent::Claude, new, true)];
    assert_eq!(chat_of(&s), after_parent(same_run));

    // With that session gone too, nothing saved holds the left out subagent's id any more.
    wait_launches(&s.claude_chat, 4);
    block_on(s.core().delete_agent_session(same_run)).unwrap();
    s.restart();
    wait_launches(&s.claude_chat, 5);
    let later = s.spawn_subagent(parent);

    assert_eq!(later, child, "the id is handed out again");
    assert_eq!(chat_of(&s), after_parent(later));
}

// Version 4 saved workspaces with no sessions in their panels, and no sessions or comments. A
// workspace with no session has no session to go to, so it is dropped.
#[test]
fn a_state_file_from_before_restore_starts_with_no_sessions_or_comments() {
    let _serial = serial();
    let dir = tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let repo = repo.canonicalize().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let state = json!({
        "version": 4,
        "repos": [{
            "main_worktree": {
                "path": repo,
                "branch": "main",
                "display_name": "repo",
                "issue_link": null,
                "base_branch": null,
                "workspace": {
                    "chat": [{"agent": "codex", "shown": true}],
                    "panels": ["diff"],
                    "selected": "diff",
                    "terminal": true
                }
            },
            "linked_worktrees": [],
            "profile": null,
            "overrides": {}
        }]
    });
    fs::write(home.join("state.json"), state.to_string()).unwrap();

    let (core, _events) = Core::start(CoreConfig::new(&home)).unwrap();

    assert_eq!(core.sessions(&repo).unwrap(), []);
    assert_eq!(core.comments(&repo).unwrap(), []);
    assert_eq!(
        core.starting_workspace(&repo).unwrap(),
        Workspace {
            chat: vec![],
            panels: vec![PanelKind::Files, PanelKind::Diff],
            selected: Some(PanelKind::Files),
            terminal: true,
            diff_base: DiffBase::Head,
        }
    );
}

/// Every Claude chat launch answers each message with the model and effort its turn runs on.
fn turns_say_their_model() -> Value {
    json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"send": {"type": "text_delta", "text": "{{model}} {{effort}}"}},
        {"send": {"type": "turn_end"}},
        {"wait_message": "never"},
    ])
}

#[test]
fn a_model_choice_comes_back_after_a_restart_and_the_next_turn_runs_on_it() {
    let _serial = serial();
    let mut s = setup(Scenarios {
        claude_chat: turns_say_their_model(),
        ..Scenarios::default()
    });
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    assert_eq!(s.core().model_choice(claude).unwrap(), None);
    s.core().set_model(claude, "haiku", Some("low")).unwrap();
    let chosen = ModelChoice {
        model: "haiku".into(),
        effort: Some("low".into()),
    };
    assert_eq!(s.core().model_choice(claude).unwrap(), Some(chosen.clone()));
    wait_launches(&s.claude_chat, 1);

    s.restart();

    wait_launches(&s.claude_chat, 2);
    assert_eq!(s.core().model_choice(claude).unwrap(), Some(chosen));
    s.core().send_message(claude, "hi").unwrap();
    s.wait_state(claude, AgentState::Done);
    let (sink, _frames) = unbounded_channel();
    let conversation = s.core().subscribe(claude, sink).unwrap();
    assert!(
        conversation.contains(&AgentMessage::TextDelta {
            text: "haiku low".into()
        }),
        "{conversation:?}"
    );
}

#[test]
fn a_session_saved_before_grove_kept_models_comes_back_with_no_choice() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let claude = s.spawn(Agent::Claude, Mode::Chat);
    s.core().set_model(claude, "haiku", Some("low")).unwrap();
    // Not what the layout gives, so it can only come back from the file.
    let layout = Workspace {
        panels: vec![PanelKind::Logs],
        selected: Some(PanelKind::Logs),
        terminal: false,
        ..s.core().workspace(claude).unwrap()
    };
    block_on(s.core().set_workspace(claude, layout.clone(), None)).unwrap();
    drop(s.core.take());
    // The state file as grove wrote it at version 6: the workspace on the worktree, not on the
    // session, and nothing a later version added.
    let path = s.cfg.home.join("state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(state["version"], json!(11));
    let worktree = &mut state["repos"][0]["main_worktree"];
    let record = worktree["sessions"][0].as_object_mut().unwrap();
    assert!(record.contains_key("model"), "{record:?}");
    let mut workspace = record.remove("workspace").unwrap();
    // Version 11 added the diff base.
    assert!(
        workspace
            .as_object_mut()
            .unwrap()
            .remove("diff_base")
            .is_some()
    );
    // Version 7 added the model, and version 9 the title, the created time and the ended flag.
    record.retain(|field, _| {
        !["model", "title", "created_unix_ms", "ended"].contains(&field.as_str())
    });
    let known = ["agent", "cols", "id", "kind", "mode", "resume_id", "rows"];
    assert_eq!(record.keys().collect::<Vec<_>>(), known, "{record:?}");
    let worktree = worktree.as_object_mut().unwrap();
    // Version 8 added the worktree a worktree nests under.
    worktree.remove("nested_under");
    worktree.insert("workspace".into(), workspace);
    state["version"] = json!(6);
    fs::write(&path, state.to_string()).unwrap();

    let (core, _events) = Core::start(s.cfg.clone()).unwrap();
    s.core = Some(core);

    assert_eq!(
        s.sessions(),
        [agent_info(claude, Agent::Claude, Mode::Chat)]
    );
    assert_eq!(s.core().model_choice(claude).unwrap(), None);
    assert_eq!(s.core().workspace(claude).unwrap(), layout);
}
