//! Agent sessions, driven by the fake Claude Code and fake Codex in fixtures/fake-agents and
//! the real grove-hook binary.

use std::fs;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use grove_core::{
    Agent, AgentSpec, AgentState, AgentStateKind, Core, CoreConfig, CoreError, CoreEvent,
    InputKind, LaunchArgs, Mode, Notify, Profile, ProfileSettings, RepoSettings, SessionId,
    StateSource, Warning,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};

mod common;
use common::{grove_hook, write_script};

const TIMEOUT: Duration = Duration::from_secs(10);

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
// Tests keep the home folder, and the socket in it, under /tmp.
fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

// The core owns its runtime, and dropping it inside an async context panics.
// So tests make and drop the core outside, and only await its calls here.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn fake_claude() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents/claude")
}

fn fake_codex() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents/codex")
}

/// A core tracking one fresh repo, with the agent replaced by its fake playing `steps`.
struct Setup {
    _dir: TempDir,
    agent: Agent,
    cfg: CoreConfig,
    core: Core,
    events: broadcast::Receiver<CoreEvent>,
    worktree: PathBuf,
    log: PathBuf,
    // The fake Codex app server's own log, so `log` has only the agent's lines.
    app_server_log: PathBuf,
}

fn setup(steps: Value) -> Setup {
    setup_with(steps, Some(grove_hook()))
}

fn setup_with(steps: Value, hook_relay: Option<&Path>) -> Setup {
    setup_tuned(steps, hook_relay, |_| {})
}

/// Like `setup_with`, with `tune` changing the config first.
fn setup_tuned(
    steps: Value,
    hook_relay: Option<&Path>,
    tune: impl FnOnce(&mut CoreConfig),
) -> Setup {
    setup_agent(Agent::Claude, steps, hook_relay, "", tune)
}

/// Like `setup`, with Codex replaced by its fake.
fn setup_codex(steps: Value) -> Setup {
    setup_codex_tuned(steps, |_| {})
}

fn setup_codex_tuned(steps: Value, tune: impl FnOnce(&mut CoreConfig)) -> Setup {
    setup_agent(Agent::Codex, steps, Some(grove_hook()), "", tune)
}

/// Like `setup_codex`, with `lines` in the fake's wrapper script before it runs the fake.
fn setup_codex_wrapped(steps: Value, lines: &str) -> Setup {
    setup_agent(Agent::Codex, steps, Some(grove_hook()), lines, |_| {})
}

fn setup_agent(
    agent: Agent,
    steps: Value,
    hook_relay: Option<&Path>,
    lines: &str,
    tune: impl FnOnce(&mut CoreConfig),
) -> Setup {
    let dir = short_temp_dir();
    let repo = dir.path().join("repo");
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());

    // The fake reads its scenario and log paths from its environment, so a wrapper sets them.
    let scenario = dir.path().join("scenario.json");
    fs::write(&scenario, json!({ "steps": steps }).to_string()).unwrap();
    let log = dir.path().join("fake.log");
    let app_server_log = dir.path().join("app-server.log");
    let mut cfg = CoreConfig::new(dir.path().join("home"));
    let (command, fake, vars) = match agent {
        Agent::Claude => (&mut cfg.claude_command, fake_claude(), "FAKE_CLAUDE"),
        Agent::Codex => (&mut cfg.codex_command, fake_codex(), "FAKE_CODEX"),
    };
    let wrapper = dir.path().join(fake.file_name().unwrap());
    let mut script = format!(
        "export {vars}_SCENARIO='{}' {vars}_LOG='{}'\n",
        scenario.display(),
        log.display(),
    );
    if agent == Agent::Codex {
        // Codex hooks run through a login shell. A home of its own keeps the developer's
        // profile out and shows whether anything lands in it or in the Codex home. Apple's
        // Python would put its bytecode cache in that home.
        let user = dir.path().join("user");
        fs::create_dir_all(user.join(".codex")).unwrap();
        script += &format!(
            "export HOME='{0}' CODEX_HOME='{0}/.codex' SHELL=/bin/sh PYTHONDONTWRITEBYTECODE=1\n",
            user.display()
        );
        // grove asks `codex app-server` about its hooks at every launch.
        script += &format!(
            "if [ \"$1\" = app-server ]; then export FAKE_CODEX_LOG='{}'; fi\n",
            app_server_log.display()
        );
    }
    script += lines;
    script += &format!("\nexec '{}' \"$@\"", fake.display());
    write_script(&wrapper, &script);
    *command = wrapper;
    // Hooks run the built relay in place. A copy per core would be a new binary, and macOS
    // checks each one on its first run, one at a time. With many tests that queue passed the
    // 2 s warm-up limit. The start_ tests below cover the copy.
    if let Some(relay) = hook_relay {
        cfg.hook_relay = relay.to_path_buf();
    }
    // As in the app, which has grove-mcp next to it. These fakes never start it.
    cfg.mcp_server = Some("grove-mcp".into());
    tune(&mut cfg);
    let (core, events) = Core::start(cfg.clone()).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    Setup {
        _dir: dir,
        agent,
        cfg,
        core,
        events,
        worktree,
        log,
        app_server_log,
    }
}

// A failed test shows what the fake did.
impl Drop for Setup {
    fn drop(&mut self) {
        if thread::panicking() {
            eprintln!(
                "fake log:\n{}\napp server log:\n{}",
                fs::read_to_string(&self.log).unwrap_or_default(),
                fs::read_to_string(&self.app_server_log).unwrap_or_default()
            );
        }
    }
}

impl Setup {
    fn spawn(&self, prompt: Option<&str>) -> SessionId {
        let spec = AgentSpec {
            agent: Some(self.agent),
            mode: Mode::Terminal,
            prompt: prompt.map(str::to_owned),
            cols: 120,
            rows: 40,
        };
        self.core.spawn_agent(&self.worktree, spec).unwrap()
    }

    /// The fake's log lines so far.
    fn log(&self) -> Vec<Value> {
        read_log(&self.log)
    }

    /// The fake Codex app server's log lines so far.
    fn app_server_log(&self) -> Vec<Value> {
        read_log(&self.app_server_log)
    }

    /// Waits until the fake Codex app server has exited, so the core has its answer.
    fn wait_app_server_exit(&self) {
        let deadline = Instant::now() + TIMEOUT;
        while !self
            .app_server_log()
            .iter()
            .any(|l| l.get("exit").is_some())
        {
            assert!(Instant::now() < deadline, "the app server never exited");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits for the first log line that has `key`.
    fn wait_log(&self, key: &str) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(line) = self.log().into_iter().find(|l| l.get(key).is_some()) {
                return line;
            }
            assert!(Instant::now() < deadline, "no {key} in the log");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The `--session-id` grove gave the fake.
    fn resume_id(&self) -> String {
        let argv = self.wait_log("argv");
        let argv = argv["argv"].as_array().unwrap();
        let at = argv.iter().position(|a| a == "--session-id").unwrap();
        argv[at + 1].as_str().unwrap().to_owned()
    }

    fn wait_exit(&mut self, session: SessionId) {
        loop {
            if let CoreEvent::SessionExited { session: s, .. } = next_event(&mut self.events)
                && s == session
            {
                return;
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
            } = next_event(&mut self.events)
            {
                return (session, state, source);
            }
        }
    }

    /// Like `next_state`, failing on any warning before it.
    fn next_state_unwarned(&mut self) -> (SessionId, AgentState, StateSource) {
        loop {
            match next_event(&mut self.events) {
                CoreEvent::Warning(warning) => panic!("{warning:?}"),
                CoreEvent::AgentState {
                    session,
                    state,
                    source,
                } => return (session, state, source),
                _ => {}
            }
        }
    }

    /// The next state change the core says should notify, skipping other events.
    fn next_notify(&mut self) -> (SessionId, PathBuf, AgentState) {
        loop {
            if let CoreEvent::Notify {
                session,
                worktree,
                state,
            } = next_event(&mut self.events)
            {
                return (session, worktree, state);
            }
        }
    }

    /// Makes profile `name` with `states` as its notify list, and the repo use it.
    fn use_profile_notifying(&self, name: &str, states: Vec<AgentStateKind>) {
        let profile = Profile {
            name: name.into(),
            suggest_for: Vec::new(),
            settings: notifying(states),
        };
        block_on(self.core.set_profile(profile, None)).unwrap();
        let settings = RepoSettings {
            profile: Some(name.into()),
            ..RepoSettings::default()
        };
        block_on(self.core.set_repo_settings(&self.worktree, settings)).unwrap();
    }

    /// Runs grove-hook as the agent's hook would, with a recorded payload and its session_id.
    fn run_hook(&self, session: SessionId, payload: &str, session_id: &str) {
        let data = match self.agent {
            Agent::Claude => "claude-code",
            Agent::Codex => "codex-cli",
        };
        let path = fake_claude()
            .parent()
            .unwrap()
            .join(format!("{data}/payloads/{payload}.json"));
        let mut payload: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        payload["session_id"] = session_id.into();
        let mut relay = Command::new(&self.cfg.hook_relay);
        match self.agent {
            Agent::Claude => relay.args(["--session", &session.to_string(), "claude"]),
            Agent::Codex => relay
                .arg("codex")
                .env("GROVE_SESSION_ID", session.to_string()),
        };
        let mut child = relay
            .env("GROVE_SOCKET", &self.cfg.socket)
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    }
}

fn read_log(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn next_event(events: &mut broadcast::Receiver<CoreEvent>) -> CoreEvent {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match events.try_recv() {
            Ok(event) => return event,
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "no core event");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

fn approval() -> AgentState {
    AgentState::NeedsInput {
        kind: InputKind::Approval,
    }
}

fn question() -> AgentState {
    AgentState::NeedsInput {
        kind: InputKind::Question,
    }
}

fn is_uuid(s: &str) -> bool {
    let groups: Vec<_> = s.split('-').map(str::len).collect();
    groups == [8, 4, 4, 4, 12] && s.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

#[test]
fn claude_gets_exactly_the_flags_and_files_grove_passed() {
    let mut s = setup(json!([]));

    let session = s.spawn(Some("add a test"));
    s.wait_exit(session);

    let dir = s.cfg.home.join(format!("sessions/{session}"));
    let settings = dir.join("claude-settings.json");
    let mcp = dir.join("mcp.json");
    let log = s.log();
    let argv: Vec<&str> = log[0]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let resume_id = argv[5];
    assert!(is_uuid(resume_id), "{resume_id}");
    assert_eq!(
        argv[1..],
        [
            "--settings",
            settings.to_str().unwrap(),
            &format!("--mcp-config={}", mcp.display()),
            "--session-id",
            resume_id,
            "add a test",
        ]
    );
    let handler = json!({
        "type": "command",
        "command": format!("'{}' --session {session} claude", s.cfg.hook_relay.display()),
        "timeout": 1,
    });
    let hook = json!([{"hooks": [handler]}]);
    let tool_hook = json!([{"matcher": "*", "hooks": [handler]}]);
    let files: Vec<(&str, Value)> = log
        .iter()
        .filter_map(|l| {
            let content = serde_json::from_str(l.get("content")?.as_str()?).unwrap();
            Some((l["file"].as_str()?, content))
        })
        .collect();
    assert_eq!(
        files,
        [
            (
                settings.to_str().unwrap(),
                json!({"hooks": {
                    "SessionStart": hook,
                    "UserPromptSubmit": hook,
                    "PreToolUse": tool_hook,
                    "PermissionRequest": tool_hook,
                    "PostToolUse": tool_hook,
                    "Stop": hook,
                    "StopFailure": hook,
                    "SessionEnd": hook,
                }})
            ),
            (
                mcp.to_str().unwrap(),
                json!({"mcpServers": {"grove": {
                    "command": s.cfg.mcp_server,
                    "args": [],
                    "env": {
                        "GROVE_SESSION_ID": session.to_string(),
                        "GROVE_SESSION_TOKEN": s.token(session),
                        "GROVE_SOCKET": s.cfg.socket,
                    },
                }}})
            ),
        ]
    );
    assert_eq!(log.last().unwrap()["exit"], 0, "{log:?}");
}

#[test]
fn user_prompt_submit_then_stop_gives_working_then_done_from_hooks() {
    let mut s = setup(json!([{"hook": "UserPromptSubmit"}, {"hook": "Stop"}]));

    let session = s.spawn(Some("hi"));

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
    assert_eq!(
        s.next_state(),
        (session, AgentState::Done, StateSource::Hook)
    );
}

#[test]
fn focus_on_a_done_session_makes_it_idle_and_the_worktrees_last_focused() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    s.next_state();
    assert_eq!(s.next_state().1, AgentState::Done);
    assert_eq!(s.core.last_focused_agent(&s.worktree), None);

    s.core.focus(session);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Hook)
    );
    assert_eq!(s.core.last_focused_agent(&s.worktree), Some(session));
}

#[test]
fn a_hook_with_another_agent_session_id_changes_nothing() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    s.next_state();
    s.next_state();
    s.core.focus(session);
    assert_eq!(s.next_state().1, AgentState::Idle);

    // Counted, this Stop would make the session Done again.
    s.run_hook(session, "Stop", "0d5a7e3b-9c41-4f2a-8b6d-1e2f3a4b5c6d");
    // The core reads hook messages in order, so this one comes after it.
    s.run_hook(session, "UserPromptSubmit", &s.resume_id());

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
}

#[test]
fn a_client_that_never_closes_its_connection_does_not_stall_hooks() {
    let mut s = setup(json!([{"hook": "UserPromptSubmit"}]));
    // Connected before the agent starts, so the core reads it first. It never finishes.
    let mut stuck = UnixStream::connect(&s.cfg.socket).unwrap();
    stuck.write_all(b"{\"session\": ").unwrap();

    let session = s.spawn(Some("hi"));

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
    drop(stuck);
}

// Every path in `dir`, relative to it, skipping `.git`.
fn files_in(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name() == Some(".git".as_ref()) {
                continue;
            }
            found.push(path.strip_prefix(dir).unwrap().to_path_buf());
            if path.is_dir() {
                todo.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn grove_writes_only_into_its_home_folder() {
    let mut s = setup(json!([{"hook": "UserPromptSubmit"}, {"hook": "Stop"}]));
    let dir = s._dir.path().to_path_buf();
    let before = files_in(&dir);

    let session = s.spawn(Some("hi"));
    s.wait_exit(session);

    let new: Vec<_> = files_in(&dir)
        .into_iter()
        .filter(|p| !before.contains(p) && *p != Path::new("fake.log"))
        .collect();
    assert!(!new.is_empty());
    for path in &new {
        assert!(
            path.starts_with("home"),
            "{} is outside home",
            path.display()
        );
    }
    // The fake ran in the worktree, and its files went where grove pointed it.
    let log = s.log();
    assert_eq!(log[0]["cwd"], s.worktree.to_str().unwrap());
    assert!(files_in(&s.worktree).is_empty());
}

#[test]
fn an_agent_session_gets_its_grove_session_id() {
    let mut s = setup(json!([]));

    let session = s.spawn(None);
    s.wait_exit(session);

    let env = &s.log()[0]["env"];
    assert_eq!(env["GROVE_SESSION_ID"], session.to_string());
    assert_eq!(env["GROVE_SOCKET"], s.cfg.socket.to_str().unwrap());
}

/// Sends one panel request on the core socket as a client does: the message, then the end of
/// its half of the connection, then the reply up to the core's end. None when the core closed
/// the connection without a reply.
fn panel_request(socket: &Path, message: &Value) -> Option<Value> {
    use std::io::Read;
    use std::net::Shutdown;

    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.write_all(message.to_string().as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    (!reply.is_empty()).then(|| serde_json::from_slice(&reply).unwrap())
}

/// The one request there is: the calling session's id.
fn session_id_request(session: &str, token: &str) -> Value {
    json!({"session": session, "token": token, "request": {"type": "session_id"}})
}

/// The reply to every request the token check refuses.
fn not_authorized() -> Value {
    json!({"error": "not authorized"})
}

impl Setup {
    /// The GROVE_SESSION_TOKEN the fake for `session` got, from the first process it started.
    fn token(&self, session: SessionId) -> String {
        self.tokens(session, 1).remove(0)
    }

    /// The GROVE_SESSION_TOKEN each process of `session` got, oldest first, once there are
    /// at least `n`.
    fn tokens(&self, session: SessionId, n: usize) -> Vec<String> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let found: Vec<String> = self
                .log()
                .into_iter()
                .filter_map(|line| {
                    let env = line.get("env")?;
                    (env["GROVE_SESSION_ID"] == session.to_string())
                        .then(|| env["GROVE_SESSION_TOKEN"].as_str().unwrap().to_owned())
                })
                .collect();
            if found.len() >= n {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "session {session} logged {} envs",
                found.len()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    // Quits grove and starts it again on the same home folder, as a restart does.
    fn restart(&mut self) {
        // The setup owns its core, so a spare one on a home of its own takes the place while
        // this one quits.
        let (spare, _) = Core::start(CoreConfig::new(self._dir.path().join("spare"))).unwrap();
        drop(std::mem::replace(&mut self.core, spare));
        // A child another test has forked holds grove.lock until it execs.
        let deadline = Instant::now() + TIMEOUT;
        let (core, events) = loop {
            match Core::start(self.cfg.clone()) {
                Ok(started) => break started,
                Err(CoreError::AlreadyRunning(_)) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("{e}"),
            }
        };
        drop(std::mem::replace(&mut self.core, core));
        self.events = events;
    }

    /// Waits until the core has applied `session`'s exit.
    fn wait_ended(&mut self, session: SessionId) {
        loop {
            let (s, state, _) = self.next_state();
            if s == session && matches!(state, AgentState::Exited | AgentState::Failed) {
                return;
            }
        }
    }
}

#[test]
fn an_agent_session_gets_a_token_next_to_its_session_id() {
    let mut s = setup(json!([]));

    let session = s.spawn(None);
    s.wait_exit(session);

    let token = s.token(session);
    assert_eq!(token.len(), 64, "{token}");
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()), "{token}");
    let another = s.spawn(None);
    assert_ne!(s.token(another), token);
}

#[test]
fn a_panel_request_with_its_sessions_token_gets_the_sessions_id() {
    let s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn(None);
    let token = s.token(session);

    let reply = panel_request(
        &s.cfg.socket,
        &session_id_request(&session.to_string(), &token),
    );

    assert_eq!(
        reply,
        Some(json!({"result": {"session": session.to_string()}}))
    );
}

#[test]
fn a_panel_request_without_its_own_sessions_token_is_refused_the_same_way_every_time() {
    let s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn(None);
    let other = s.spawn(None);
    let token = s.token(session);
    let others_token = s.token(other);
    let id = session.to_string();
    let shell = s
        .core
        .spawn(
            &s.worktree,
            grove_core::SpawnSpec {
                command: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 30".into()],
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    // The right token with its last digit changed.
    let mut near = token.clone();
    let last = if near.ends_with('0') { "1" } else { "0" };
    near.replace_range(63.., last);

    let refused = [
        session_id_request(&id, &near),
        session_id_request(&id, &others_token),
        session_id_request(&id, ""),
        session_id_request(&id, &token[..32]),
        session_id_request(&id, &format!("{token}0")),
        json!({"session": id, "request": {"type": "session_id"}}),
        json!({"session": id, "token": null, "request": {"type": "session_id"}}),
        json!({"token": token, "request": {"type": "session_id"}}),
        session_id_request("999", &token),
        session_id_request("", &token),
        session_id_request("not a number", &token),
        // Only the form a session id prints in names it.
        session_id_request(&format!("+{id}"), &token),
        session_id_request(&format!("0{id}"), &token),
        // A shell session is no agent session, and has no token.
        session_id_request(&shell.to_string(), &token),
    ];

    for request in &refused {
        assert_eq!(
            panel_request(&s.cfg.socket, request),
            Some(not_authorized()),
            "{request}"
        );
    }
}

#[test]
fn an_ended_session_cannot_make_panel_requests() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn(None);
    let token = s.token(session);
    let request = session_id_request(&session.to_string(), &token);
    assert!(panel_request(&s.cfg.socket, &request).unwrap()["result"].is_object());

    s.core.kill(session).unwrap();
    s.wait_ended(session);

    assert_eq!(
        panel_request(&s.cfg.socket, &request),
        Some(not_authorized())
    );
}

#[test]
fn a_panel_request_for_something_unknown_or_with_bad_fields_gets_an_error_reply() {
    let s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn(None);
    let token = s.token(session);
    let id = session.to_string();

    let unknown = json!({"session": id, "token": token, "request": {"type": "panel_open"}});
    let number: u64 = id.parse().unwrap();
    let bad = json!({"session": number, "token": token, "request": {"type": "session_id"}});

    assert_eq!(
        panel_request(&s.cfg.socket, &unknown),
        Some(json!({"error": "unknown request"}))
    );
    assert_eq!(
        panel_request(&s.cfg.socket, &bad),
        Some(json!({"error": "bad request"}))
    );
}

// Every regular file under `dir`, `.git` included, that holds `text`, relative to `dir`.
fn files_holding(dir: &Path, text: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        for entry in fs::read_dir(next).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                todo.push(path);
            } else if meta.is_file()
                && String::from_utf8_lossy(&fs::read(&path).unwrap()).contains(text)
            {
                found.push(path.strip_prefix(dir).unwrap().to_path_buf());
            }
        }
    }
    found.sort();
    found
}

#[test]
fn a_session_token_lives_only_in_its_session_folder_readable_by_the_user() {
    use std::os::unix::fs::PermissionsExt;

    let mut s = setup(json!([{"hook": "UserPromptSubmit"}, {"hook": "Stop"}]));
    let dir = s._dir.path().to_path_buf();

    let session = s.spawn(Some("hi"));
    s.wait_exit(session);

    let token = s.token(session);
    let file = Path::new("home/sessions")
        .join(session.to_string())
        .join("token");
    assert_eq!(fs::read_to_string(dir.join(&file)).unwrap(), token);
    let mode = fs::metadata(dir.join(&file)).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    // grove-mcp gets it from the MCP config, which only the user may read too.
    let mcp = file.with_file_name("mcp.json");
    let mode = fs::metadata(dir.join(&mcp)).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    // The fake logs its whole environment and the MCP config, so its log holds it too.
    assert_eq!(
        files_holding(&dir, &token),
        [Path::new("fake.log"), &mcp, &file]
    );
    assert!(files_in(&s.worktree).is_empty());
}

#[test]
fn a_token_file_left_with_a_looser_mode_is_made_readable_only_by_the_user() {
    use std::os::unix::fs::PermissionsExt;

    let mut s = setup(json!([]));
    // Session ids start at 0, so the first session takes this folder.
    let folder = s.cfg.home.join("sessions/0");
    fs::create_dir_all(&folder).unwrap();
    let file = folder.join("token");
    fs::write(&file, "left from before").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

    let session = s.spawn(None);
    s.wait_exit(session);

    assert_eq!(session.to_string(), "0");
    assert_eq!(fs::read_to_string(&file).unwrap(), s.token(session));
    let mode = fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
}

#[test]
fn a_panel_request_that_trickles_in_holds_hooks_no_longer_than_the_read_deadline() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn(None);
    s.token(session);
    let resume_id = s.resume_id();
    // A request that sends one more byte every 50 ms for 5 s and never ends. A deadline per
    // read, not per connection, would wait on it all that time.
    let mut slow = UnixStream::connect(&s.cfg.socket).unwrap();
    slow.write_all(b"{\"session\": \"0\", \"token\": \"")
        .unwrap();
    let trickle = thread::spawn(move || {
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until && slow.write_all(b"0").is_ok() {
            thread::sleep(Duration::from_millis(50));
        }
    });

    let sent = Instant::now();
    s.run_hook(session, "UserPromptSubmit", &resume_id);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );

    // The read deadline is 500 ms. The rest leaves room for a loaded machine.
    let waited = sent.elapsed();
    assert!(waited < Duration::from_secs(2), "{waited:?}");
    trickle.join().unwrap();
}

#[test]
fn a_codex_session_gets_a_token_next_to_its_session_id_and_its_panel_requests_are_accepted() {
    let s = setup_codex(json!([{"wait_input": "\r"}]));

    let session = s.spawn(None);

    let token = s.token(session);
    let folder = s.cfg.home.join("sessions").join(session.to_string());
    assert_eq!(fs::read_to_string(folder.join("token")).unwrap(), token);
    assert_eq!(
        panel_request(
            &s.cfg.socket,
            &session_id_request(&session.to_string(), &token)
        ),
        Some(json!({"result": {"session": session.to_string()}}))
    );
}

#[test]
fn a_token_file_that_is_a_symlink_is_not_followed_and_the_launch_fails() {
    let s = setup(json!([]));
    let elsewhere = s._dir.path().join("elsewhere");
    fs::write(&elsewhere, "keep").unwrap();
    // Session ids start at 0, so the first session takes this folder.
    let folder = s.cfg.home.join("sessions/0");
    fs::create_dir_all(&folder).unwrap();
    std::os::unix::fs::symlink(&elsewhere, folder.join("token")).unwrap();

    let spec = AgentSpec {
        agent: Some(Agent::Claude),
        mode: Mode::Terminal,
        prompt: None,
        cols: 80,
        rows: 24,
    };
    let started = s.core.spawn_agent(&s.worktree, spec);

    assert!(matches!(started, Err(CoreError::Io { .. })), "{started:?}");
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "keep");
}

#[test]
fn a_restored_session_gets_a_new_token_and_its_old_one_is_refused() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn(None);
    let old = s.token(session);
    let id = session.to_string();

    s.restart();

    let tokens = s.tokens(session, 2);
    let new = &tokens[1];
    assert_ne!(*new, old);
    assert_eq!(
        panel_request(&s.cfg.socket, &session_id_request(&id, &old)),
        Some(not_authorized())
    );
    assert_eq!(
        panel_request(&s.cfg.socket, &session_id_request(&id, new)),
        Some(json!({"result": {"session": id}}))
    );
}

// A shell would inherit grove's own environment, so this test runs itself again with
// GROVE_SESSION_ID and GROVE_SESSION_TOKEN set, as when grove starts from inside an agent
// session.
#[test]
fn a_shell_session_gets_no_grove_session_id() {
    if std::env::var_os("GROVE_SESSION_ID").is_none() {
        let run = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "a_shell_session_gets_no_grove_session_id"])
            .env("GROVE_SESSION_ID", "41")
            .env("GROVE_SESSION_TOKEN", "0123")
            .env("GROVE_SOCKET", "/tmp/elsewhere.sock")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stdout)
        );
        return;
    }
    // It needs no hook relay, and building one here would race the other tests reading it.
    let s = setup_with(json!([]), None);
    let shell = s
        .core
        .spawn(
            &s.worktree,
            grove_core::SpawnSpec {
                command: "/bin/sh".into(),
                // Output before attach is dropped, so it waits for a line first.
                args: vec![
                    "-c".into(),
                    "read _; echo \"[${GROVE_SESSION_ID-none} ${GROVE_SESSION_TOKEN-none} ${GROVE_SOCKET-none}]\""
                        .into(),
                ],
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();
    let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
    s.core.attach(shell, sink).unwrap();
    s.core.write(shell, b"\r").unwrap();

    let mut output = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    while !output.contains(&b']') {
        assert!(
            Instant::now() < deadline,
            "{:?}",
            String::from_utf8_lossy(&output)
        );
        match rx.try_recv() {
            Ok(frame) => output.extend(frame.bytes),
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }
    // The line breaks around it come from the terminal echoing the typed line, and their
    // order varied once on Linux, so only the printed line counts.
    let output = String::from_utf8_lossy(&output);
    assert_eq!(output.trim_matches(['\r', '\n']), "[none none none]");
}

// Each start gets its own home, all sharing one hook relay path. Restarting on one home
// would need the old lock gone, and a PTY child another test forks holds a copy of it until
// it execs.
#[test]
fn start_copies_grove_hook_into_its_bin_only_when_it_changed() {
    use std::os::unix::fs::MetadataExt;
    let dir = short_temp_dir();
    let relay = dir.path().join("bin/grove-hook");
    let start = |home: &str| {
        let mut cfg = CoreConfig::new(dir.path().join(home));
        cfg.hook_relay = relay.clone();
        cfg.hook_relay_source = Some(grove_hook().to_path_buf());
        Core::start(cfg).unwrap()
    };

    let _first = start("first");
    assert_eq!(fs::read(&relay).unwrap(), fs::read(grove_hook()).unwrap());

    let installed = fs::metadata(&relay).unwrap().ino();
    let _second = start("second");
    assert_eq!(
        fs::metadata(&relay).unwrap().ino(),
        installed,
        "copied again"
    );

    fs::write(&relay, "stale").unwrap();
    let _third = start("third");
    assert_eq!(fs::read(&relay).unwrap(), fs::read(grove_hook()).unwrap());
}

// Only macOS runs the relay at start, for its first-run check of a new binary.
#[cfg(target_os = "macos")]
#[test]
fn start_does_not_wait_on_a_relay_that_hangs() {
    use std::os::unix::fs::PermissionsExt;

    let dir = short_temp_dir();
    let pid_file = dir.path().join("pid");
    let relay = dir.path().join("hangs");
    fs::write(
        &relay,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
            pid_file.display()
        ),
    )
    .unwrap();
    // Not write_script: this test is about the core's own first run of a new relay.
    fs::set_permissions(&relay, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = CoreConfig::new(dir.path().join("home"));
    cfg.hook_relay_source = Some(relay);

    let started = Instant::now();
    let _core = Core::start(cfg).unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    // macOS checks a new script on its first run, which took 1.2 to 1.5 s here when idle and
    // more while other tests start, so the core can kill the relay before its first line.
    // Then nothing of it ran.
    let Ok(pid) = fs::read_to_string(&pid_file) else {
        return;
    };
    // kill -0 also finds a zombie, so this fails unless the core killed and reaped it.
    let alive = Command::new("kill")
        .args(["-0", pid.trim()])
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!alive.success(), "the relay is still running");
}

#[test]
fn a_permission_request_gives_needs_input_approval_within_a_second() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "PreToolUse-Bash"},
        {"wait_input": "go"},
        {"hook": "PermissionRequest-Bash"},
        {"wait_input": "1"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    // The hook fires once the fake reads this, so the time to the state bounds its delay.
    let typed = Instant::now();
    s.core.write(session, b"go").unwrap();

    assert_eq!(s.next_state(), (session, approval(), StateSource::Hook));
    assert!(
        typed.elapsed() < Duration::from_secs(1),
        "{:?}",
        typed.elapsed()
    );
}

#[test]
fn ask_user_question_gives_needs_input_question_and_its_permission_request_keeps_it() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "PreToolUse-AskUserQuestion"},
        {"hook": "PermissionRequest-AskUserQuestion"},
        {"wait_input": "\r"},
        {"hook": "PostToolUse-AskUserQuestion"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    assert_eq!(s.next_state(), (session, question(), StateSource::Hook));
    // An Approval from the PermissionRequest would come before this.
    s.core.write(session, b"\r").unwrap();
    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
}

#[test]
fn stop_failure_gives_failed() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "StopFailure"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Hook)
    );
}

fn notifying(states: Vec<AgentStateKind>) -> ProfileSettings {
    ProfileSettings {
        notify: Notify {
            states: Some(states),
        },
        ..ProfileSettings::default()
    }
}

// A burst with no waits between the hooks. Done would come first if it notified.
#[test]
fn with_no_notify_list_needs_input_and_failed_notify_and_done_does_not() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"hook": "UserPromptSubmit"},
        {"hook": "PreToolUse-Bash"},
        {"hook": "PermissionRequest-Bash"},
        {"hook": "StopFailure"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));

    let worktree = s.worktree.clone();
    assert_eq!(s.next_notify(), (session, worktree.clone(), approval()));
    assert_eq!(s.next_notify(), (session, worktree, AgentState::Failed));
}

// The list replaces the default, so Failed, which comes first, does not notify.
#[test]
fn a_profile_that_lists_done_notifies_on_done_only() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "StopFailure"},
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"wait_input": "\r"},
    ]));
    s.use_profile_notifying("Done too", vec![AgentStateKind::Done]);
    let session = s.spawn(Some("hi"));

    assert_eq!(
        s.next_notify(),
        (session, s.worktree.clone(), AgentState::Done)
    );
}

#[test]
fn the_repo_override_of_the_notify_list_wins_over_its_profile() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"hook": "UserPromptSubmit"},
        {"hook": "StopFailure"},
        {"wait_input": "\r"},
    ]));
    s.use_profile_notifying("Done too", vec![AgentStateKind::Done]);
    let settings = RepoSettings {
        profile: Some("Done too".into()),
        overrides: notifying(vec![AgentStateKind::Failed]),
    };
    block_on(s.core.set_repo_settings(&s.worktree, settings)).unwrap();
    let session = s.spawn(Some("hi"));

    assert_eq!(
        s.next_notify(),
        (session, s.worktree.clone(), AgentState::Failed)
    );
}

#[test]
fn subagent_stop_and_notification_leave_the_state_as_it_was() {
    let mut s = setup(json!([{"hook": "UserPromptSubmit"}, {"wait_input": "\r"}]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    let resume_id = s.resume_id();

    // Counted, these would make the session Done or NeedsInput before the Failed below.
    s.run_hook(session, "SubagentStop", &resume_id);
    s.run_hook(session, "Notification-permission_prompt", &resume_id);
    s.run_hook(session, "StopFailure", &resume_id);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Hook)
    );
}

#[test]
fn after_clear_the_recorded_resume_id_is_the_new_one_and_only_its_hooks_count() {
    let new_id = "0d5a7e3b-9c41-4f2a-8b6d-1e2f3a4b5c6d";
    let mut s = setup(json!([
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"wait_input": "\r"},
        {"hook": "SessionEnd-clear"},
        {"session_id": new_id},
        {"hook": "SessionStart-clear"},
        {"hook": "UserPromptSubmit"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    let old_id = s.resume_id();
    assert_eq!(s.core.resume_id(session).as_deref(), Some(old_id.as_str()));

    // /clear. Its SessionEnd must not show the session as Exited.
    s.core.write(session, b"\r").unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
    assert_eq!(s.core.resume_id(session).as_deref(), Some(new_id));
    // Counted, the old id's Stop would make the session Done before the Failed.
    s.run_hook(session, "Stop", &old_id);
    s.run_hook(session, "StopFailure", new_id);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Hook)
    );
}

#[test]
fn session_end_gives_exited_before_the_process_ends() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"hook": "SessionEnd-prompt_input_exit"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Exited, StateSource::Hook)
    );
}

#[test]
fn exit_code_1_gives_failed() {
    let mut s = setup(json!([{"hook": "UserPromptSubmit"}, {"exit": 1}]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Hook)
    );
}

#[test]
fn exit_code_0_gives_exited_after_the_hooks_sent_before_it() {
    let mut s = setup(json!([{"hook": "UserPromptSubmit"}, {"hook": "Stop"}]));
    let session = s.spawn(Some("hi"));

    // The Stop hook runs just before the exit, and still comes first.
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Exited, StateSource::Hook)
    );
}

#[test]
fn groves_kill_gives_exited() {
    let mut s = setup(json!([{"hook": "UserPromptSubmit"}, {"wait_input": "\r"}]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    // kill sends SIGKILL, which from anyone else gives Failed.
    s.core.kill(session).unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Exited, StateSource::Hook)
    );
}

#[test]
fn a_sigkill_grove_did_not_send_gives_failed() {
    let mut s = setup(json!([{"hook": "UserPromptSubmit"}, {"signal": "KILL"}]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Hook)
    );
}

#[test]
fn a_double_ctrl_c_quit_with_no_session_end_gives_exited() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"wait_input": "\u{3}\u{3}"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);

    s.core.write(session, b"\x03\x03").unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Exited, StateSource::Hook)
    );
}

// No recording has a SessionEnd for /resume, so the /clear one takes its reason.
#[test]
fn after_resume_in_the_tui_the_recorded_resume_id_is_the_new_one_and_only_its_hooks_count() {
    let new_id = "6b1f0c2d-3e4a-4b5c-9d6e-7f8a9b0c1d2e";
    let mut s = setup(json!([
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"wait_input": "\r"},
        {"hook": "SessionEnd-clear", "set": {"reason": "resume"}},
        {"session_id": new_id},
        {"hook": "SessionStart-resume"},
        {"hook": "UserPromptSubmit"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    let old_id = s.resume_id();

    // /resume to another conversation. Its SessionEnd must not show the session as Exited.
    s.core.write(session, b"\r").unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
    assert_eq!(s.core.resume_id(session).as_deref(), Some(new_id));
    // Counted, the old id's Stop would make the session Done before the Failed.
    s.run_hook(session, "Stop", &old_id);
    s.run_hook(session, "StopFailure", new_id);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Hook)
    );
}

#[test]
fn writing_to_needs_input_then_output_gives_working_from_the_heuristic_until_a_hook() {
    let mut s = setup(json!([
        {"hook": "UserPromptSubmit"},
        {"hook": "PreToolUse-Bash"},
        {"hook": "PermissionRequest-Bash"},
        {"wait_input": "1"},
        {"print": "running touch note.txt"},
        {"wait_input": "x"},
        {"hook": "PostToolUse-Bash"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state(), (session, approval(), StateSource::Hook));

    // The approval fires no hook. The output that follows it shows the agent went on.
    s.core.write(session, b"1").unwrap();
    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Heuristic)
    );

    s.core.write(session, b"x").unwrap();
    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
}

#[test]
fn output_after_the_configured_window_leaves_needs_input_alone() {
    let mut s = setup_tuned(
        json!([
            {"hook": "UserPromptSubmit"},
            {"hook": "PermissionRequest-Bash"},
            {"wait_input": "1"},
            {"sleep": 1},
            {"print": "late"},
            // Time for the heuristics to look before the hook.
            {"sleep": 0.5},
            {"hook": "Stop"},
            {"wait_input": "\r"},
        ]),
        Some(grove_hook()),
        |cfg| cfg.needs_input_to_working = Duration::from_millis(300),
    );
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, approval());

    s.core.write(session, b"1").unwrap();

    // Taken as an answer, the late output would give Working before the Done.
    assert_eq!(
        s.next_state(),
        (session, AgentState::Done, StateSource::Hook)
    );
}

/// Like `setup`, with the stuck `Working` check after 300 ms of quiet.
fn setup_quick(steps: Value) -> Setup {
    setup_tuned(steps, Some(grove_hook()), |cfg| {
        cfg.stuck_working_after = Duration::from_millis(300);
    })
}

#[test]
fn a_quiet_working_session_with_the_prompt_on_screen_becomes_idle() {
    let mut s = setup_quick(json!([
        {"hook": "UserPromptSubmit"},
        {"screen": "idle-prompt"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Heuristic)
    );
}

#[test]
fn a_quiet_working_session_with_an_error_above_the_prompt_becomes_failed() {
    let mut s = setup_quick(json!([
        {"hook": "UserPromptSubmit"},
        {"screen": "error-prompt"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Heuristic)
    );
}

#[test]
fn the_trust_dialog_gives_needs_input_approval_until_the_next_hook() {
    // In a folder Claude does not trust yet, no hook runs before the user accepts.
    let mut s = setup(json!([
        {"screen": "trust-dialog"},
        {"wait_input": "\r"},
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));

    assert_eq!(
        s.next_state(),
        (session, approval(), StateSource::Heuristic)
    );

    s.core.write(session, b"\r").unwrap();
    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
}

#[test]
fn without_hooks_the_state_follows_output_and_the_screen() {
    let mut s = setup_quick(json!([
        {"screen": "idle-prompt"},
        {"wait_input": "\r"},
        {"print": "thinking"},
        {"screen": "error-prompt"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(None);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Heuristic)
    );

    s.core.write(session, b"\r").unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Heuristic)
    );
    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Heuristic)
    );
}

#[test]
fn the_screen_counts_only_while_the_agent_holds_the_terminal() {
    // An agent whose foreground job shows the prompt, then waits for a line. Job control
    // (set -m) gives that job the terminal.
    let screen = fake_claude()
        .parent()
        .unwrap()
        .join("claude-code/screens/idle-prompt.ansi");
    let mut s = setup_tuned(json!([]), None, |cfg| {
        cfg.stuck_working_after = Duration::from_millis(300);
        let agent = cfg.home.with_file_name("agent");
        write_script(
            &agent,
            &format!(
                "set -m\nstty -echo\n(cat '{}'; head -n 1 >/dev/null)\nexec sleep 1000",
                screen.display()
            ),
        );
        cfg.claude_command = agent;
    });
    let session = s.spawn(None);
    // Starting the agent can take seconds under load, so wait for the whole screen before
    // watching.
    let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
    s.core.attach(session, sink).unwrap();
    let size = fs::metadata(&screen).unwrap().len();
    let mut shown = 0;
    let deadline = Instant::now() + TIMEOUT;
    while shown < size {
        assert!(Instant::now() < deadline, "the screen never showed");
        match rx.try_recv() {
            Ok(frame) => shown += frame.bytes.len() as u64,
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }

    // Long enough for several quiet checks.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match s.events.try_recv() {
            Ok(CoreEvent::AgentState { state, .. }) => {
                panic!("{state:?} while another program held the terminal")
            }
            _ => thread::sleep(Duration::from_millis(10)),
        }
    }

    s.core.write(session, b"\n").unwrap();
    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Heuristic)
    );
}

/// The arguments after the program in the fake's first log line.
fn argv(log: &[Value]) -> Vec<String> {
    log[0]["argv"].as_array().unwrap()[1..]
        .iter()
        .map(|a| a.as_str().unwrap().to_owned())
        .collect()
}

/// Gives the repo different extra args for each agent.
fn set_launch_args(s: &Setup) {
    let settings = RepoSettings {
        overrides: ProfileSettings {
            launch_args: LaunchArgs {
                claude: Some(vec![
                    "--add-dir".into(),
                    "../shared".into(),
                    "--verbose".into(),
                ]),
                codex: Some(vec![
                    "--search".into(),
                    "--add-dir".into(),
                    "../shared".into(),
                ]),
            },
            ..ProfileSettings::default()
        },
        ..RepoSettings::default()
    };
    block_on(s.core.set_repo_settings(&s.worktree, settings)).unwrap();
}

// The fakes take only grove's own flags, so they exit 2 on these, after logging their argv.
#[test]
fn claude_gets_the_repo_launch_args_after_grove_flags_and_the_prompt() {
    let mut s = setup(json!([]));
    set_launch_args(&s);

    let session = s.spawn(Some("add a test"));
    s.wait_exit(session);

    let argv = argv(&s.log());
    // grove's flags, as claude_gets_exactly_the_flags_and_files_grove_passed checks them.
    assert_eq!(argv[0], "--settings");
    assert!(argv[2].starts_with("--mcp-config="), "{argv:?}");
    assert_eq!(argv[3], "--session-id");
    assert_eq!(
        argv[5..],
        ["add a test", "--add-dir", "../shared", "--verbose"]
    );
}

// The repo picks a profile and sets no launch args of its own.
#[test]
fn claude_gets_the_launch_args_of_the_repo_profile() {
    let mut s = setup(json!([]));
    let profile = Profile {
        name: "Mine".into(),
        suggest_for: Vec::new(),
        settings: ProfileSettings {
            launch_args: LaunchArgs {
                claude: Some(vec!["--verbose".into()]),
                ..LaunchArgs::default()
            },
            ..ProfileSettings::default()
        },
    };
    block_on(s.core.set_profile(profile, None)).unwrap();
    let settings = RepoSettings {
        profile: Some("Mine".into()),
        ..RepoSettings::default()
    };
    block_on(s.core.set_repo_settings(&s.worktree, settings)).unwrap();

    let session = s.spawn(Some("add a test"));
    s.wait_exit(session);

    assert_eq!(argv(&s.log())[5..], ["add a test", "--verbose"]);
}

#[test]
fn codex_gets_the_repo_launch_args_after_grove_flags_and_the_prompt() {
    let mut s = setup_codex(json!([]));
    set_launch_args(&s);

    let session = s.spawn(Some("add a test"));
    s.wait_exit(session);

    let argv = argv(&s.log());
    // grove's flags, as codex_gets_exactly_the_flags_and_environment_grove_passed checks them:
    // --no-daemon, then ten -c overrides.
    assert_eq!(argv[0], "--no-daemon");
    assert!(argv[1..21].iter().step_by(2).all(|a| a == "-c"), "{argv:?}");
    assert_eq!(
        argv[21..],
        ["add a test", "--search", "--add-dir", "../shared"]
    );
}

#[test]
fn codex_gets_exactly_the_flags_and_environment_grove_passed() {
    let mut s = setup_codex(json!([]));

    let session = s.spawn(Some("add a test"));
    s.wait_exit(session);

    let log = s.log();
    let relay = s.cfg.hook_relay.display().to_string();
    let handler = format!(r#"{{type="command",command="'{relay}' codex",timeout=1}}"#);
    let hook = |event: &str| format!(r#"hooks.{event}=[{{hooks=[{handler}]}}]"#);
    let tool_hook = |event: &str| format!(r#"hooks.{event}=[{{matcher="*",hooks=[{handler}]}}]"#);
    let mut expected = vec!["--no-daemon".to_owned()];
    for flag in [
        tool_hook("PreToolUse"),
        tool_hook("PermissionRequest"),
        tool_hook("PostToolUse"),
        hook("SessionStart"),
        hook("SessionEnd"),
        hook("UserPromptSubmit"),
        hook("Stop"),
        hook("Interrupt"),
        format!(
            r#"mcp_servers.grove={{command="{}",args=[],env_vars=["GROVE_SESSION_ID","GROVE_SESSION_TOKEN","GROVE_SOCKET"]}}"#,
            s.cfg.mcp_server.as_ref().unwrap().display()
        ),
        format!(r#"shell_environment_policy.set.GROVE_SESSION_ID="{session}""#),
    ] {
        expected.extend(["-c".to_owned(), flag]);
    }
    expected.push("add a test".to_owned());
    assert_eq!(argv(&log), expected);

    // What the fake made of them, as Codex would.
    let handler = json!({"type": "command", "command": format!("'{relay}' codex"), "timeout": 1});
    let hook = json!([{"hooks": [handler]}]);
    let tool_hook = json!([{"matcher": "*", "hooks": [handler]}]);
    assert_eq!(
        log[1]["config"],
        json!({
            "hooks": {
                "PreToolUse": tool_hook,
                "PermissionRequest": tool_hook,
                "PostToolUse": tool_hook,
                "SessionStart": hook,
                "SessionEnd": hook,
                "UserPromptSubmit": hook,
                "Stop": hook,
                "Interrupt": hook,
            },
            "mcp_servers": {"grove": {
                "command": s.cfg.mcp_server,
                "args": [],
                "env_vars": ["GROVE_SESSION_ID", "GROVE_SESSION_TOKEN", "GROVE_SOCKET"],
            }},
            "shell_environment_policy": {"set": {"GROVE_SESSION_ID": session.to_string()}},
        })
    );
    let env = &log[0]["env"];
    assert_eq!(env["GROVE_SESSION_ID"], session.to_string());
    assert_eq!(env["GROVE_SOCKET"], s.cfg.socket.to_str().unwrap());
    assert_eq!(env["GROVE_SESSION_TOKEN"], s.token(session));
    assert_eq!(log.last().unwrap()["exit"], 0, "{log:?}");
}

#[test]
fn codex_gets_byte_identical_hook_entries_on_every_launch() {
    let mut s = setup_codex(json!([]));

    let first = s.spawn(Some("hi"));
    s.wait_exit(first);
    let second = s.spawn(None);
    s.wait_exit(second);

    // Codex asks for approval again when an entry changes.
    let hooks: Vec<Vec<String>> = s
        .log()
        .iter()
        .filter(|l| l.get("argv").is_some())
        .map(|l| {
            argv(std::slice::from_ref(l))
                .into_iter()
                .filter(|a| a.starts_with("hooks."))
                .collect()
        })
        .collect();
    assert_eq!(hooks.len(), 2);
    assert_eq!(hooks[0].len(), 8);
    assert_eq!(hooks[0], hooks[1]);
}

#[test]
fn a_codex_permission_request_gives_needs_input_approval_within_a_second() {
    let mut s = setup_codex(json!([
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"hook": "PreToolUse-Bash"},
        {"wait_input": "go"},
        {"hook": "PermissionRequest-Bash"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    // The hook fires once the fake reads this, so the time to the state bounds its delay.
    let typed = Instant::now();
    s.core.write(session, b"go").unwrap();

    assert_eq!(s.next_state(), (session, approval(), StateSource::Hook));
    assert!(
        typed.elapsed() < Duration::from_secs(1),
        "{:?}",
        typed.elapsed()
    );
}

#[test]
fn request_user_input_gives_needs_input_question_and_its_post_tool_use_gives_working() {
    let mut s = setup_codex(json!([
        {"hook": "SessionStart-resume"},
        {"hook": "UserPromptSubmit"},
        {"hook": "PreToolUse-request_user_input"},
        {"wait_input": "\r"},
        {"hook": "PostToolUse-request_user_input"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state(), (session, question(), StateSource::Hook));

    // Answering the question fires PostToolUse.
    s.core.write(session, b"\r").unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
}

#[test]
fn codex_interrupt_gives_idle_and_stop_gives_done() {
    let mut s = setup_codex(json!([
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"hook": "PermissionRequest-Bash"},
        {"wait_input": "\u{1b}"},
        {"hook": "Interrupt"},
        {"wait_input": "\r"},
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, approval());

    // Esc on the approval prompt.
    s.core.write(session, b"\x1b").unwrap();
    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Hook)
    );

    s.core.write(session, b"\r").unwrap();
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Done, StateSource::Hook)
    );
}

#[test]
fn a_codex_session_has_no_resume_id_before_its_first_prompt() {
    let s = setup_codex(json!([{"wait_input": "\r"}, {"hook": "SessionStart-startup"}]));

    let session = s.spawn(None);

    // Codex picks its own id, and SessionStart reports it with the first prompt.
    assert_eq!(s.core.resume_id(session), None);
}

#[test]
fn the_codex_resume_id_comes_from_each_session_start_and_only_its_hooks_count() {
    let first_id = "019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b";
    let next_id = "6b1f0c2d-3e4a-4b5c-9d6e-7f8a9b0c1d2e";
    let mut s = setup_codex(json!([
        {"session_id": first_id},
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"hook": "Stop"},
        {"wait_input": "\r"},
        {"session_id": next_id},
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    assert_eq!(s.core.resume_id(session).as_deref(), Some(first_id));

    // The next SessionStart, from a new conversation in the same process.
    s.core.write(session, b"\r").unwrap();
    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
    assert_eq!(s.core.resume_id(session).as_deref(), Some(next_id));
    // Counted, the old id's Stop would make the session Done before the Idle.
    s.run_hook(session, "Stop", first_id);
    s.run_hook(session, "Interrupt", next_id);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Hook)
    );
}

#[test]
fn codex_sessions_write_nothing_into_the_worktree_or_the_codex_home() {
    let mut s = setup_codex(json!([
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"hook": "PermissionRequest-Bash"},
        {"hook": "Stop"},
        {"hook": "SessionEnd"},
    ]));
    let dir = s._dir.path().to_path_buf();
    let before = files_in(&dir);

    let session = s.spawn(Some("hi"));
    s.wait_exit(session);

    let new: Vec<_> = files_in(&dir)
        .into_iter()
        .filter(|p| {
            !before.contains(p)
                && !["fake.log", "app-server.log"]
                    .map(Path::new)
                    .contains(&p.as_path())
        })
        .collect();
    assert!(!new.is_empty());
    for path in &new {
        assert!(
            path.starts_with("home"),
            "{} is outside home",
            path.display()
        );
    }
    // The fake ran in the worktree with HOME and CODEX_HOME pointed into the test folder,
    // and its hooks ran there too.
    let log = s.log();
    assert_eq!(log[0]["cwd"], s.worktree.to_str().unwrap());
    let user = dir.join("user");
    assert_eq!(log[0]["env"]["HOME"], user.to_str().unwrap());
    assert_eq!(
        log[0]["env"]["CODEX_HOME"],
        user.join(".codex").to_str().unwrap()
    );
    assert_eq!(log.iter().filter(|l| l.get("hook").is_some()).count(), 5);
    assert!(files_in(&s.worktree).is_empty());
    assert_eq!(files_in(&user), [Path::new(".codex")]);
}

// By default Codex rejects request_user_input after its PreToolUse fires, and the question
// arrives as plain text. The core reports both states as they come, with no debounce.
#[test]
fn a_rejected_codex_question_shows_needs_input_then_done() {
    let mut s = setup_codex(json!([
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"hook": "PreToolUse-request_user_input"},
        {"sleep": 0.05},
        {"hook": "Stop"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    assert_eq!(s.next_state(), (session, question(), StateSource::Hook));
    assert_eq!(
        s.next_state(),
        (session, AgentState::Done, StateSource::Hook)
    );
}

#[test]
fn hooks_need_review_gives_needs_input_approval_until_the_next_hook() {
    // While grove's hooks are new or changed, Codex asks at launch and runs none until then.
    let mut s = setup_codex(json!([
        {"screen": "hooks-review"},
        {"wait_input": "\r"},
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));

    assert_eq!(
        s.next_state(),
        (session, approval(), StateSource::Heuristic)
    );

    s.core.write(session, b"\r").unwrap();
    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Hook)
    );
}

#[test]
fn untrusted_or_modified_codex_hooks_warn_and_leave_states_to_heuristics_until_a_hook() {
    for trust in ["untrusted", "modified"] {
        // The fake shows "Hooks need review" at launch and runs no hook.
        let mut s = setup_codex_wrapped(
            json!([{"wait_input": "\r"}]),
            &format!("export FAKE_CODEX_HOOK_TRUST={trust}"),
        );
        let session = s.spawn(Some("hi"));

        // The warning comes from the check and the state from the screen, in either order.
        let mut warning = None;
        let mut state = None;
        while warning.is_none() || state.is_none() {
            match next_event(&mut s.events) {
                CoreEvent::Warning(w) => warning = Some(w),
                CoreEvent::AgentState {
                    session,
                    state: st,
                    source,
                } => state = Some((session, st, source)),
                _ => {}
            }
        }
        assert_eq!(
            warning,
            Some(Warning::CodexHooksNotTrusted {
                session,
                worktree: s.worktree.clone(),
            }),
            "{trust}"
        );
        assert_eq!(
            state,
            Some((session, approval(), StateSource::Heuristic)),
            "{trust}"
        );

        // The user approved them in /hooks, and they run from the next prompt.
        let id = "019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b";
        s.run_hook(session, "SessionStart-startup", id);
        s.run_hook(session, "UserPromptSubmit", id);
        assert_eq!(
            s.next_state(),
            (session, AgentState::Working, StateSource::Hook),
            "{trust}"
        );
    }
}

#[test]
fn trusted_codex_hooks_give_no_warning() {
    let mut s = setup_codex(json!([
        {"mode": "bracketed_paste", "on": true},
        {"wait_input": "\r"},
        {"hook": "SessionStart-startup"},
        {"hook": "UserPromptSubmit"},
        {"wait_input": "\r"},
    ]));
    let session = s.spawn(Some("hi"));
    s.wait_app_server_exit();

    // The fake turns echo off before its first output. A write before that comes back as
    // output, which the heuristic takes for Working before the hook arrives.
    let (sink, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut shown = s.core.attach(session, sink).unwrap().bytes;
    let deadline = Instant::now() + TIMEOUT;
    while !shown.windows(8).any(|w| w == b"\x1b[?2004h") {
        assert!(
            Instant::now() < deadline,
            "the agent never turned on bracketed paste"
        );
        match rx.try_recv() {
            Ok(frame) => shown.extend(frame.bytes),
            Err(_) => thread::sleep(Duration::from_millis(10)),
        }
    }

    // The core sends any warning as soon as it has the answer, well before this hook.
    s.core.write(session, b"\r").unwrap();
    assert_eq!(
        s.next_state_unwarned(),
        (session, AgentState::Working, StateSource::Hook)
    );
}

#[test]
fn the_codex_hook_check_gets_the_launch_overrides_and_no_launch_bypasses_hook_trust() {
    let mut s = setup_codex_wrapped(json!([]), "export FAKE_CODEX_HOOK_TRUST=untrusted");
    let session = s.spawn(Some("hi"));
    s.wait_exit(session);
    s.wait_app_server_exit();

    let launch = argv(&s.log());
    let check = argv(&s.app_server_log());
    // The check runs `codex app-server` with the overrides that sit between --no-daemon and
    // the prompt.
    assert_eq!(launch[0], "--no-daemon");
    assert_eq!(check[0], "app-server");
    assert_eq!(check[1..], launch[1..launch.len() - 1]);
    assert_eq!(s.app_server_log()[0]["cwd"], s.worktree.to_str().unwrap());
    // Neither approves grove's hooks, by flag or through the hook state Codex keeps.
    for (argv, log) in [(launch, s.log()), (check, s.app_server_log())] {
        for arg in &argv {
            assert_ne!(arg, "--dangerously-bypass-hook-trust");
            assert!(!arg.contains("hooks.state"), "{arg}");
        }
        assert_eq!(log[1]["config"]["hooks"].get("state"), None);
    }
}

// A repo's `-c` overrides can change which hooks Codex loads, such as features.hooks.
#[test]
fn the_codex_hook_check_also_gets_the_repo_config_overrides_and_no_other_launch_args() {
    let mut s = setup_codex(json!([]));
    let hook =
        |event: &str| format!(r#"hooks.{event}=[{{hooks=[{{type="command",command="true"}}]}}]"#);
    let settings = RepoSettings {
        overrides: ProfileSettings {
            launch_args: LaunchArgs {
                codex: Some(vec![
                    "--search".into(),
                    "-c".into(),
                    hook("PreCompact"),
                    format!("--config={}", hook("PostCompact")),
                ]),
                ..LaunchArgs::default()
            },
            ..ProfileSettings::default()
        },
        ..RepoSettings::default()
    };
    block_on(s.core.set_repo_settings(&s.worktree, settings)).unwrap();

    let session = s.spawn(None);
    s.wait_exit(session);
    s.wait_app_server_exit();

    let launch = argv(&s.log());
    let check = argv(&s.app_server_log());
    // grove's overrides as the launch has them, then the repo's, each after its own -c.
    let own = &launch[1..launch.len() - 4];
    let mut expected = vec!["app-server".to_owned()];
    expected.extend(own.iter().cloned());
    expected.extend([
        "-c".into(),
        hook("PreCompact"),
        "-c".into(),
        hook("PostCompact"),
    ]);
    assert_eq!(check, expected);
    assert_eq!(s.app_server_log().last().unwrap()["exit"], 0);
}

#[test]
fn a_hanging_codex_app_server_delays_neither_the_launch_nor_the_hooks_and_is_killed() {
    let mut s = setup_codex_wrapped(
        json!([
            {"hook": "SessionStart-startup"},
            {"hook": "UserPromptSubmit"},
            {"wait_input": "\r"},
        ]),
        r#"if [ "$1" = app-server ]; then echo $$ > "${FAKE_CODEX_LOG%.log}.pid"; exec sleep 30; fi"#,
    );

    let started = Instant::now();
    let session = s.spawn(Some("hi"));

    assert!(
        started.elapsed() < Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    // With no answer the hooks are unknown, and they count when they run.
    assert_eq!(
        s.next_state_unwarned(),
        (session, AgentState::Working, StateSource::Hook)
    );
    // The core gives the check 5 s, then kills the app server and reaps it. kill -0 also finds
    // a zombie. macOS checks a new script on its first run, so the kill can come before the
    // script writes its pid, and then nothing of it ran.
    let pid_file = s.app_server_log.with_extension("pid");
    let deadline = started + Duration::from_secs(5) + TIMEOUT;
    loop {
        let pid = fs::read_to_string(&pid_file).unwrap_or_default();
        let alive = || {
            Command::new("kill")
                .args(["-0", pid.trim()])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        if !pid.trim().is_empty() && !alive() {
            break;
        }
        if Instant::now() > deadline {
            assert!(pid.trim().is_empty(), "the app server is still running");
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    while let Ok(event) = s.events.try_recv() {
        assert!(!matches!(event, CoreEvent::Warning(_)), "{event:?}");
    }
}

#[test]
fn a_failing_codex_app_server_leaves_the_hooks_unknown() {
    // Like a Codex that has no app server.
    let mut s = setup_codex_wrapped(
        json!([
            {"hook": "SessionStart-startup"},
            {"hook": "UserPromptSubmit"},
            {"wait_input": "\r"},
        ]),
        r#"if [ "$1" = app-server ]; then exit 1; fi"#,
    );
    let session = s.spawn(Some("hi"));

    assert_eq!(
        s.next_state_unwarned(),
        (session, AgentState::Working, StateSource::Hook)
    );
}

#[test]
fn a_quiet_working_codex_session_with_the_prompt_on_screen_becomes_idle() {
    let mut s = setup_codex_tuned(
        json!([
            {"hook": "SessionStart-startup"},
            {"hook": "UserPromptSubmit"},
            {"screen": "idle-prompt"},
            {"wait_input": "\r"},
        ]),
        |cfg| cfg.stuck_working_after = Duration::from_millis(300),
    );
    let session = s.spawn(Some("hi"));
    assert_eq!(s.next_state().1, AgentState::Working);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Heuristic)
    );
}

// The attention queue. Most of these fakes run no hook, and the test sends each hook itself,
// so it controls the order sessions enter their states.

/// A core whose fake agents wait for input and run no hook.
fn setup_waiting() -> Setup {
    setup(json!([{"wait_input": "\r"}]))
}

impl Setup {
    /// Runs the recorded hook `payload` for `session` and waits for the state it sets.
    fn hook(&mut self, session: SessionId, payload: &str, state: AgentState) {
        let resume_id = self.core.resume_id(session).unwrap();
        self.run_hook(session, payload, &resume_id);
        assert_eq!(self.next_state(), (session, state, StateSource::Hook));
    }

    fn queue(&self) -> Vec<(SessionId, AgentState)> {
        self.core
            .attention_queue()
            .into_iter()
            .map(|entry| (entry.session, entry.state))
            .collect()
    }
}

#[test]
fn the_attention_queue_ranks_approvals_then_questions_then_failed_then_done() {
    let mut s = setup_waiting();
    let done = s.spawn(None);
    let failed = s.spawn(None);
    let asking = s.spawn(None);
    let approving = s.spawn(None);

    // Each enters its state after the one ranked below it, so age alone gives the reverse.
    s.hook(done, "Stop", AgentState::Done);
    s.hook(failed, "StopFailure", AgentState::Failed);
    s.hook(asking, "PreToolUse-AskUserQuestion", question());
    s.hook(approving, "PermissionRequest-Bash", approval());

    assert_eq!(
        s.queue(),
        [
            (approving, approval()),
            (asking, question()),
            (failed, AgentState::Failed),
            (done, AgentState::Done),
        ]
    );
}

#[test]
fn two_approvals_come_back_oldest_first() {
    let mut s = setup_waiting();
    let later = s.spawn(None);
    let earlier = s.spawn(None);

    // The session started second asks first, so neither its id nor its start gives this order.
    let before = SystemTime::now();
    s.hook(earlier, "PermissionRequest-Bash", approval());
    let after = SystemTime::now();
    s.hook(later, "PermissionRequest-Bash", approval());

    assert_eq!(s.queue(), [(earlier, approval()), (later, approval())]);
    let head = &s.core.attention_queue()[0];
    assert_eq!(head.worktree, s.worktree);
    assert!(
        before <= head.since && head.since <= after,
        "{:?} is not within {before:?} to {after:?}",
        head.since
    );
}

#[test]
fn a_session_that_leaves_the_queue_comes_back_at_the_end_of_its_group() {
    let mut s = setup_waiting();
    let first = s.spawn(None);
    let second = s.spawn(None);
    s.hook(first, "PermissionRequest-Bash", approval());
    s.hook(second, "PermissionRequest-Bash", approval());

    s.hook(first, "PostToolUse-Bash", AgentState::Working);
    assert_eq!(s.queue(), [(second, approval())]);

    s.hook(first, "PermissionRequest-Bash", approval());
    assert_eq!(s.queue(), [(second, approval()), (first, approval())]);
}

#[test]
fn working_idle_and_exited_agent_sessions_and_shell_sessions_never_appear() {
    let mut s = setup_waiting();
    let waiting = s.spawn(None);
    let working = s.spawn(None);
    let idle = s.spawn(None);
    let exited = s.spawn(None);
    // No hook has given it a state yet.
    s.spawn(None);
    let shell = grove_core::SpawnSpec {
        command: "/bin/sh".into(),
        args: vec!["-c".into(), "read _".into()],
        cols: 80,
        rows: 24,
    };
    s.core.spawn(&s.worktree, shell).unwrap();

    s.hook(waiting, "PermissionRequest-Bash", approval());
    s.hook(working, "UserPromptSubmit", AgentState::Working);
    s.hook(idle, "Stop", AgentState::Done);
    // Focus moves Done to Idle.
    s.core.focus(idle);
    assert_eq!(s.next_state(), (idle, AgentState::Idle, StateSource::Hook));
    s.core.kill(exited).unwrap();
    assert_eq!(
        s.next_state(),
        (exited, AgentState::Exited, StateSource::Hook)
    );

    assert_eq!(s.queue(), [(waiting, approval())]);
}

#[test]
fn a_repeated_hook_with_the_same_state_keeps_the_sessions_place() {
    let mut s = setup_waiting();
    let first = s.spawn(None);
    let second = s.spawn(None);
    let other = s.spawn(None);
    s.hook(first, "PermissionRequest-Bash", approval());
    s.hook(second, "PermissionRequest-Bash", approval());

    let resume_id = s.core.resume_id(first).unwrap();
    s.run_hook(first, "PermissionRequest-Bash", &resume_id);
    // The repeat sends no event. The core reads hook messages in order, so once this one's
    // state arrives, it has read the repeat.
    s.hook(other, "UserPromptSubmit", AgentState::Working);

    assert_eq!(s.queue(), [(first, approval()), (second, approval())]);
}

#[test]
fn a_hook_that_confirms_a_heuristic_state_keeps_the_sessions_place() {
    // The trust dialog gives Approval from the heuristic.
    let mut s = setup(json!([{"screen": "trust-dialog"}, {"wait_input": "\r"}]));
    let first = s.spawn(None);
    assert_eq!(s.next_state(), (first, approval(), StateSource::Heuristic));
    let second = s.spawn(None);
    assert_eq!(s.next_state(), (second, approval(), StateSource::Heuristic));

    s.hook(first, "PermissionRequest-Bash", approval());

    assert_eq!(s.queue(), [(first, approval()), (second, approval())]);
}

#[test]
fn a_failed_session_leaves_the_queue_when_its_worktree_is_forgotten() {
    // An exit is final, so nothing else would take it out.
    let mut s = setup(json!([{"exit": 1}]));
    let session = s.spawn(None);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Hook)
    );
    assert_eq!(s.queue(), [(session, AgentState::Failed)]);

    block_on(
        s.core
            .remove_worktree(&s.worktree, grove_core::RemoveOpts::ForgetRepo),
    )
    .unwrap();

    assert_eq!(s.queue(), Vec::new());
}

#[test]
fn a_codex_hook_clears_only_its_sessions_warning_in_a_shared_worktree() {
    let mut s = setup_codex_wrapped(
        json!([{"wait_input":"\r"}]),
        "export FAKE_CODEX_HOOK_TRUST=untrusted",
    );
    let first = s.spawn(None);
    let second = s.spawn(None);
    for _ in 0..2 {
        while !matches!(
            next_event(&mut s.events),
            CoreEvent::Warning(Warning::CodexHooksNotTrusted { .. })
        ) {}
    }
    let hooks = || {
        s.core
            .warnings()
            .into_iter()
            .filter_map(|entry| match entry.warning {
                Warning::CodexHooksNotTrusted { session, .. } => Some((session, entry.id)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let warnings = hooks();
    assert_eq!(warnings.len(), 2);
    let first_id = warnings
        .iter()
        .find(|(session, _)| *session == first)
        .unwrap()
        .1;
    let second_id = warnings
        .iter()
        .find(|(session, _)| *session == second)
        .unwrap()
        .1;
    s.run_hook(first, "SessionStart-startup", "first-conversation");
    while !matches!(next_event(&mut s.events), CoreEvent::WarningCleared { id } if id == first_id) {
    }
    assert_eq!(hooks(), [(second, second_id)]);
    s.core.kill(second).unwrap();
    while !matches!(next_event(&mut s.events), CoreEvent::WarningCleared { id } if id == second_id)
    {
    }
    assert!(hooks().is_empty());
}
