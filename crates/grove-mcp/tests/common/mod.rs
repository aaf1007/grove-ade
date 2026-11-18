//! What the process tests share: a core with fake agents, and ways to watch it.
// Each test file uses its own part of this.
#![allow(dead_code, unused_imports)]

pub use std::fs;
pub use std::io::{BufRead, BufReader, Lines, Read, Write};
pub use std::net::Shutdown;
pub use std::os::unix::net::UnixStream;
pub use std::path::{Path, PathBuf};
pub use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
pub use std::thread;
pub use std::time::{Duration, Instant};

pub use grove_core::{
    Agent, AgentSpec, Core, CoreConfig, CoreEvent, Mode, NewWorktree, PanelSpec, Profile,
    ProfileSettings, RepoSettings, SessionId,
};
pub use serde_json::{Value, json};
pub use tempfile::TempDir;
pub use tokio::sync::broadcast::{self, error::TryRecvError};

pub const TIMEOUT: Duration = Duration::from_secs(10);

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
pub fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

// The core owns its runtime, and dropping it inside an async context panics.
pub fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

// Cargo builds the binary for this package's tests.
pub fn grove_mcp() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_grove-mcp"))
}

pub fn fakes() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents")
}

/// A core tracking one fresh repo, with both agents and the Claude sidecar replaced by fakes.
/// A parent in terminal mode plays `steps`. The sidecar waits, unless `sidecar_plays` says
/// otherwise, and so does Codex's app server.
pub struct Setup {
    pub dir: TempDir,
    pub cfg: CoreConfig,
    pub core: Core,
    pub events: broadcast::Receiver<CoreEvent>,
    pub worktree: PathBuf,
}

pub fn setup(steps: Value) -> Setup {
    setup_with(steps, |_| {})
}

/// A setup whose core config `configure` changes first.
pub fn setup_with(steps: Value, configure: impl FnOnce(&mut CoreConfig)) -> Setup {
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

    let parent = dir.path().join("parent.json");
    fs::write(&parent, json!({ "steps": steps }).to_string()).unwrap();
    let mut cfg = CoreConfig::new(dir.path().join("home"));
    // A home of its own keeps the developer's profile out of Codex's login shell.
    let user = dir.path().join("user");
    fs::create_dir_all(user.join(".codex")).unwrap();
    let wrap = |name: &str, vars: &str| {
        let wrapper = dir.path().join(name);
        let script = format!(
            "#!/bin/sh\nexport {vars}_LOG='{log}' HOME='{user}' CODEX_HOME='{user}/.codex' \
             PYTHONDONTWRITEBYTECODE=1\nexec '{fake}' \"$@\"\n",
            log = dir.path().join(format!("{name}.log")).display(),
            user = user.display(),
            fake = fakes().join(name).display(),
        );
        fs::write(&wrapper, script).unwrap();
        make_executable(&wrapper);
        wrapper
    };
    // A subagent of Claude runs on the sidecar and one of Codex on its app server, which waits
    // for the core without a scenario.
    let sidecar = dir.path().join("sidecar.json");
    fs::write(
        &sidecar,
        json!({"steps": [{"wait_message": "never"}]}).to_string(),
    )
    .unwrap();
    let env = format!(
        "FAKE_CLAUDE_SCENARIO='{0}' FAKE_CODEX_SCENARIO='{0}' FAKE_CLAUDE_SIDECAR_SCENARIO='{1}'",
        parent.display(),
        sidecar.display()
    );
    cfg.claude_command = wrap("claude", &format!("{env} FAKE_CLAUDE"));
    cfg.codex_command = wrap("codex", &format!("{env} FAKE_CODEX"));
    cfg.claude_sidecar_command = wrap("claude-sidecar", &format!("{env} FAKE_CLAUDE_SIDECAR"));
    cfg.mcp_server = grove_mcp().to_path_buf();
    // No hooks run here, so no relay is needed.
    cfg.hook_relay = "/usr/bin/true".into();
    configure(&mut cfg);
    let (core, events) = Core::start(cfg.clone()).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    Setup {
        dir,
        cfg,
        core,
        events,
        worktree,
    }
}

// A failed test shows what the fakes did.
impl Drop for Setup {
    fn drop(&mut self) {
        if thread::panicking() {
            for name in ["claude", "codex", "claude-sidecar"] {
                let log = self.dir.path().join(format!("{name}.log"));
                let log = fs::read_to_string(log).unwrap_or_default();
                eprintln!("{name} log:\n{log}");
            }
        }
    }
}

pub fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn read_log(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

impl Setup {
    /// Starts the parent: `agent` in terminal mode, playing the setup's steps.
    pub fn spawn_parent(&self, agent: Agent) -> SessionId {
        self.spawn_parent_in(agent, Mode::Terminal)
    }

    pub fn spawn_parent_in(&self, agent: Agent, mode: Mode) -> SessionId {
        let spec = AgentSpec {
            agent,
            mode,
            prompt: None,
            cols: 100,
            rows: 30,
            panel: None,
        };
        self.core.spawn_agent(&self.worktree, spec).unwrap()
    }

    /// From now on a parent in terminal mode plays `steps`.
    pub fn parent_plays(&self, steps: Value) {
        let parent = self.dir.path().join("parent.json");
        fs::write(parent, json!({ "steps": steps }).to_string()).unwrap();
    }

    /// From now on every sidecar plays `steps`, a Claude subagent's too.
    pub fn sidecar_plays(&self, steps: Value) {
        let sidecar = self.dir.path().join("sidecar.json");
        fs::write(sidecar, json!({ "steps": steps }).to_string()).unwrap();
    }

    /// The log of the fake `name`.
    pub fn log(&self, name: &str) -> Vec<Value> {
        read_log(&self.dir.path().join(format!("{name}.log")))
    }

    /// The reply to the parent's tool call, once the fake `name` logged it.
    pub fn tool_reply(&self, name: &str) -> Value {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(line) = self
                .log(name)
                .into_iter()
                .find(|l| l.get("mcp_call").is_some())
            {
                return line["reply"].clone();
            }
            assert!(Instant::now() < deadline, "no tool call in the {name} log");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The replies to the first `n` tool calls in the log of the fake `name`, in log order.
    pub fn tool_replies(&self, name: &str, n: usize) -> Vec<Value> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let replies: Vec<Value> = self
                .log(name)
                .into_iter()
                .filter(|l| l.get("mcp_call").is_some())
                .map(|l| l["reply"].clone())
                .collect();
            if replies.len() >= n {
                return replies;
            }
            assert!(
                Instant::now() < deadline,
                "{} of {n} tool calls in the {name} log",
                replies.len()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The next panel request the core sends, skipping other events.
    pub fn next_panel_request(&mut self) -> (Option<SessionId>, PanelSpec) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(CoreEvent::PanelRequested { parent, panel }) => return (parent, panel),
                Ok(_) => {}
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "no panel request");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// Whether the core has sent a panel request by now.
    pub fn panel_requested(&mut self) -> bool {
        while let Ok(event) = self.events.try_recv() {
            if matches!(event, CoreEvent::PanelRequested { .. }) {
                return true;
            }
        }
        false
    }

    /// A second tracked worktree of the setup's repo, on a branch of its own.
    pub fn other_worktree(&self) -> PathBuf {
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=grove",
                "-c",
                "user.email=grove@example.com",
            ])
            .args(["commit", "-q", "--allow-empty", "-m", "first"])
            .current_dir(&self.worktree)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success());
        let new = NewWorktree {
            branch: "feature".into(),
            ..NewWorktree::default()
        };
        block_on(self.core.create_worktree(&self.worktree, new))
            .unwrap()
            .worktree
            .path
    }

    /// Saves a profile named `name` with `settings` and has the repo whose main worktree is at
    /// `repo` use it.
    pub fn use_profile(&self, repo: &Path, name: &str, settings: ProfileSettings) {
        let profile = Profile {
            name: name.into(),
            suggest_for: Vec::new(),
            settings,
        };
        block_on(self.core.set_profile(profile, None)).unwrap();
        let settings = RepoSettings {
            profile: Some(name.into()),
            ..RepoSettings::default()
        };
        block_on(self.core.set_repo_settings(repo, settings)).unwrap();
    }

    /// The file of the profile named `name`, as grove names a new one.
    pub fn profile_file(&self, name: &str) -> PathBuf {
        self.dir
            .path()
            .join("home/profiles")
            .join(format!("{}.toml", name.to_lowercase()))
    }

    /// Drops the events sent so far.
    pub fn drain_events(&mut self) {
        while self.events.try_recv().is_ok() {}
    }

    /// The next `WorkspaceChanged` or `ProfilesChanged`, skipping other events.
    pub fn next_change(&mut self) -> CoreEvent {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.events.try_recv() {
                Ok(
                    event
                    @ (CoreEvent::WorkspaceChanged { .. } | CoreEvent::ProfilesChanged { .. }),
                ) => {
                    return event;
                }
                Ok(_) => {}
                Err(TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "no change");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    /// Whether the core has sent a `WorkspaceChanged` or `ProfilesChanged` by now.
    pub fn changed_by_now(&mut self) -> bool {
        while let Ok(event) = self.events.try_recv() {
            if matches!(
                event,
                CoreEvent::WorkspaceChanged { .. } | CoreEvent::ProfilesChanged { .. }
            ) {
                return true;
            }
        }
        false
    }

    /// The GROVE_SESSION_TOKEN the fake `name` got for `session`.
    pub fn token(&self, name: &str, session: SessionId) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let found = self.log(name).into_iter().find_map(|line| {
                let env = line.get("env")?;
                (env["GROVE_SESSION_ID"] == session.to_string())
                    .then(|| env["GROVE_SESSION_TOKEN"].as_str().unwrap().to_owned())
            });
            if let Some(token) = found {
                return token;
            }
            assert!(Instant::now() < deadline, "no token for {session}");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

/// The text of a tool call reply, and whether it is an error.
pub fn tool_text(reply: &Value) -> (String, bool) {
    let result = &reply["result"];
    let text = result["content"][0]["text"].as_str().unwrap_or_default();
    (text.to_owned(), result["isError"] == true)
}

/// Sends one request on the core socket and returns the reply.
pub fn panel_request(socket: &Path, message: &Value) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.write_all(message.to_string().as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    serde_json::from_slice(&reply).unwrap()
}

/// grove-mcp started with an agent session's id and token, as its agent starts it, and spoken
/// to as an MCP client.
pub struct McpClient {
    pub child: Child,
    // None once the client has left.
    pub stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl McpClient {
    pub fn start(socket: &Path, session: SessionId, token: &str) -> Self {
        let mut child = Command::new(grove_mcp())
            .env_clear()
            .env("GROVE_SESSION_ID", session.to_string())
            .env("GROVE_SESSION_TOKEN", token)
            .env("GROVE_SOCKET", socket)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut client = Self {
            child,
            stdin: Some(stdin),
            lines,
            next_id: 1,
        };
        client.send(
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}}),
            "initialize",
        );
        writeln!(
            client.stdin(),
            "{}",
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
        )
        .unwrap();
        client
    }

    pub fn stdin(&mut self) -> &mut ChildStdin {
        self.stdin.as_mut().unwrap()
    }

    pub fn send(&mut self, params: Value, method: &str) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin(), "{message}").unwrap();
        let reply: Value = serde_json::from_str(&self.lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }

    /// Calls `tool` and returns its text and whether it is an error.
    pub fn call(&mut self, tool: &str, arguments: Value) -> (String, bool) {
        let reply = self.send(json!({"name": tool, "arguments": arguments}), "tools/call");
        tool_text(&reply)
    }

    /// Opens a panel that must open, and returns its id.
    pub fn open(&mut self, kind: &str, title: &str, cmd: &str) -> String {
        let (text, is_error) = self.call(
            "panel_open",
            json!({"kind": kind, "title": title, "cmd": cmd}),
        );
        assert!(!is_error, "{text}");
        let id = text
            .strip_prefix("Opened panel ")
            .and_then(|rest| rest.strip_suffix('.'))
            .unwrap_or_else(|| panic!("{text}"));
        id.to_owned()
    }

    /// Waits for a panel that must exit within the test timeout.
    pub fn wait_exited(&mut self, id: &str) -> String {
        let (text, is_error) = self.call(
            "panel_wait",
            json!({"id": id, "state": "exited", "timeout_seconds": 10}),
        );
        assert!(!is_error, "{text}");
        text
    }

    pub fn read(&mut self, id: &str, lines: u64) -> String {
        let (text, is_error) = self.call("panel_read", json!({"id": id, "lines": lines}));
        assert!(!is_error, "{text}");
        text
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A Claude Code session that waits, with a grove-mcp of its own.
pub fn agent_with_client(s: &Setup, agent: Agent) -> (SessionId, McpClient) {
    let session = s.spawn_parent(agent);
    let name = match agent {
        Agent::Claude => "claude",
        Agent::Codex => "codex",
    };
    let token = s.token(name, session);
    (session, McpClient::start(&s.cfg.socket, session, &token))
}
