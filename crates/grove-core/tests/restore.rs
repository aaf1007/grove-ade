//! Restore after a restart, against the fake agents in fixtures/fake-agents. A second core started
//! on the same home folder brings back agent sessions in their mode with the integration of their
//! first launch, shells as fresh shells, run sessions stopped, review comments and workspaces.

use std::fs;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Barrier, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Agent, AgentMessage, AgentPanel, AgentSpec, AgentState, CommentId, Core, CoreConfig, CoreError,
    CoreEvent, Mode, ModelChoice, NewReviewComment, NewWorktree, PanelKind, ProfileSettings,
    RemoveOpts, RepoSettings, ReviewComment, RunSession, RunSpec, SessionId, SessionInfo,
    SessionKind, TurnOutcome, Warning, Workspace,
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

fn panel(agent: Agent, session: Option<SessionId>, shown: bool) -> AgentPanel {
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
            agent,
            mode,
            prompt: None,
            cols: 100,
            rows: 30,
            panel: None,
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

    // The worktree whose agent session started a subagent in `worktree`, as the list says.
    fn linked_from(&self, worktree: &Path) -> Option<PathBuf> {
        block_on(self.core().list_worktrees())
            .into_iter()
            .find(|w| w.path == worktree)
            .unwrap()
            .linked_from
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
        AgentMessage::TurnStarted,
        AgentMessage::TextDelta {
            text: format!("re: {text}"),
        },
        AgentMessage::TurnEnded {
            outcome: TurnOutcome::Completed,
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
        chat: vec![panel(Agent::Claude, Some(claude), true)],
        panels: vec![PanelKind::Diff],
        selected: Some(PanelKind::Diff),
        terminal: false,
    };
    block_on(s.core().set_workspace(&s.worktree, workspace.clone(), None)).unwrap();
    wait_launches(&s.claude_chat, 1);

    s.restart();

    wait_launches(&s.claude_chat, 2);
    assert_eq!(
        s.sessions(),
        [agent_info(claude, Agent::Claude, Mode::Chat)]
    );
    assert_eq!(s.core().workspace(&s.worktree).unwrap(), workspace);
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
    assert_eq!(history, turn_messages("fix the bug"));
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
fn a_subagent_in_another_worktree_links_it_to_the_parents_and_the_link_outlives_both_and_a_restart()
{
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let other = s.add_worktree("feat-linked");
    let parent = s.spawn(Agent::Claude, Mode::Chat);
    assert_eq!(s.linked_from(&other), None);

    let child = s.spawn_subagent_in(parent, Some(&other));

    assert_eq!(s.linked_from(&other), Some(s.worktree.clone()));
    assert_eq!(s.linked_from(&s.worktree), None);
    s.core().kill(child).unwrap();
    s.core().kill(parent).unwrap();
    s.wait_state(parent, AgentState::Exited);
    assert_eq!(s.linked_from(&other), Some(s.worktree.clone()));
    s.restart();
    assert_eq!(s.linked_from(&other), Some(s.worktree.clone()));
}

#[test]
fn a_worktree_keeps_its_first_link_and_a_subagent_in_its_parents_own_worktree_links_nothing() {
    let _serial = serial();
    let s = setup(Scenarios::default());
    let other = s.add_worktree("feat-linked");
    let third = s.add_worktree("feat-third");
    let first_parent = s.spawn(Agent::Claude, Mode::Chat);
    s.spawn_subagent_in(first_parent, Some(&other));
    let second_parent = s.spawn_in(&third, Agent::Claude, Mode::Chat);

    s.spawn_subagent_in(second_parent, Some(&other));
    s.spawn_subagent(first_parent);

    assert_eq!(s.linked_from(&other), Some(s.worktree.clone()));
    assert_eq!(s.linked_from(&s.worktree), None);
    assert_eq!(s.linked_from(&third), None);
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
    let codex = s.spawn(Agent::Codex, Mode::Chat);
    let ended = s.spawn(Agent::Claude, Mode::Chat);
    s.core().kill(ended).unwrap();
    s.wait_state(ended, AgentState::Exited);
    let workspace = Workspace {
        chat: vec![
            panel(Agent::Codex, Some(codex), true),
            panel(Agent::Claude, Some(ended), true),
            panel(Agent::Claude, Some(claude), false),
            panel(Agent::Codex, None, true),
        ],
        panels: vec![PanelKind::Diff, PanelKind::Logs],
        selected: Some(PanelKind::Logs),
        terminal: false,
    };
    block_on(s.core().set_workspace(&s.worktree, workspace, None)).unwrap();

    s.restart();
    // The panel of the session that had ended is gone, so this one, which may take its id,
    // never shows in it.
    let shell = s.core().spawn_shell(&s.worktree, 80, 24).unwrap();

    assert!(![claude, codex].contains(&shell));
    assert_eq!(
        s.core().workspace(&s.worktree).unwrap(),
        Workspace {
            chat: vec![
                panel(Agent::Codex, Some(codex), true),
                panel(Agent::Claude, Some(claude), false),
                panel(Agent::Codex, None, true),
            ],
            panels: vec![PanelKind::Diff, PanelKind::Logs],
            selected: Some(PanelKind::Logs),
            terminal: false,
        }
    );
}

// What grove would restore if it stopped now, from the state file.
fn saved_session_ids(home: &Path) -> Vec<u64> {
    let state: Value =
        serde_json::from_slice(&fs::read(home.join("state.json")).unwrap_or_default())
            .unwrap_or(Value::Null);
    state["repos"][0]["main_worktree"]["sessions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| s["id"].as_u64().unwrap())
        .collect()
}

// So a crash keeps the sessions that ran.
#[test]
fn the_state_file_follows_sessions_as_they_start_and_end() {
    let _serial = serial();
    let mut s = setup(Scenarios::default());
    let home = s.cfg.home.clone();
    let agent = s.spawn(Agent::Claude, Mode::Chat);
    let shell = s.core().spawn_shell(&s.worktree, 80, 24).unwrap();
    let id = |session: SessionId| session.to_string().parse::<u64>().unwrap();

    wait_for("both saved", || {
        (saved_session_ids(&home) == [id(agent), id(shell)]).then_some(())
    });
    s.core().kill(agent).unwrap();
    s.wait_state(agent, AgentState::Exited);
    wait_for("the agent gone", || {
        (saved_session_ids(&home) == [id(shell)]).then_some(())
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
    let Warning::SessionNotRestored { worktree, reason } = warning else {
        panic!("{warning:?}");
    };
    assert_eq!(worktree, s.worktree);
    assert!(reason.contains("no-such-claude"), "{reason}");
    assert_eq!(s.sessions(), [agent_info(codex, Agent::Codex, Mode::Chat)]);
    assert!(s.core().resume_id(claude).is_none());
}

// Version 4 saved workspaces with no sessions in their panels, and no sessions or comments.
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
        core.workspace(&repo).unwrap(),
        Workspace {
            chat: vec![panel(Agent::Codex, None, true)],
            panels: vec![PanelKind::Diff],
            selected: Some(PanelKind::Diff),
            terminal: true,
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
    drop(s.core.take());
    // The state file as grove wrote it before version 7: no model on any session.
    let path = s.cfg.home.join("state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(state["version"], json!(8));
    fn forget_models(value: &mut Value) {
        match value {
            Value::Object(object) => {
                object.remove("model");
                object.values_mut().for_each(forget_models);
            }
            Value::Array(items) => items.iter_mut().for_each(forget_models),
            _ => {}
        }
    }
    forget_models(&mut state);
    state["version"] = json!(6);
    fs::write(&path, state.to_string()).unwrap();

    let (core, _events) = Core::start(s.cfg.clone()).unwrap();
    s.core = Some(core);

    assert_eq!(
        s.sessions(),
        [agent_info(claude, Agent::Claude, Mode::Chat)]
    );
    assert_eq!(s.core().model_choice(claude).unwrap(), None);
}
