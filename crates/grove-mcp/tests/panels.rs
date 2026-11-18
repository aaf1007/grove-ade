//! Log and command panels: an agent session opens one through grove-mcp, reads its last lines,
//! waits for it to exit and closes it. Only the session that opened a panel can act on it.

mod common;

use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use common::*;
use grove_core::{AgentState, RemoveOpts};
use tokio::sync::mpsc::unbounded_channel;

fn panel_id(id: &str) -> SessionId {
    // A panel's id is its session's.
    serde_json::from_value(json!(id.parse::<u64>().unwrap())).unwrap()
}

/// The next `PanelClosed`, skipping other events.
fn next_panel_closed(s: &mut Setup) -> SessionId {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match s.events.try_recv() {
            Ok(CoreEvent::PanelClosed { panel }) => return panel,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "no PanelClosed");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

fn panel_closed_by_now(s: &mut Setup) -> bool {
    while let Ok(event) = s.events.try_recv() {
        if matches!(event, CoreEvent::PanelClosed { .. }) {
            return true;
        }
    }
    false
}

fn is_alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// The text of `path` once the panel's command has written a line there.
fn written(path: &Path) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Ok(text) = fs::read_to_string(path)
            && text.ends_with('\n')
        {
            return text.trim().to_owned();
        }
        assert!(Instant::now() < deadline, "nothing in {}", path.display());
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn open_runs_the_command_in_the_agents_worktree_and_asks_for_a_command_panel() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (owner, mut client) = agent_with_client(&s, Agent::Claude);

    let id = client.open("command", "dev server", "pwd; echo hello");

    let (parent, panel) = s.next_panel_request();
    assert_eq!(parent, Some(owner));
    assert_eq!(
        panel,
        PanelSpec::Command {
            session: panel_id(&id),
            worktree: s.worktree.clone(),
            title: "dev server".into(),
        }
    );
    assert_eq!(
        client.wait_exited(&id),
        format!("Panel {id} exited with code 0.")
    );
    assert_eq!(
        client.read(&id, 10),
        format!(
            "Panel {id} exited with code 0.\n\n{}\nhello",
            s.worktree.display()
        )
    );
}

#[test]
fn a_log_panel_is_asked_for_as_a_log() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Codex);

    let id = client.open("log", "tests", "echo ok");

    let (_, panel) = s.next_panel_request();
    assert_eq!(
        panel,
        PanelSpec::Log {
            session: panel_id(&id),
            worktree: s.worktree.clone(),
            title: "tests".into(),
        }
    );
}

#[test]
fn an_agent_opens_a_panel_from_the_mcp_config_grove_passes_it() {
    let mut s = setup(json!([
        {"mcp_call": "grove", "tool": "panel_open",
         "arguments": {"kind": "log", "title": "tests", "cmd": "echo ok"}},
        {"wait_input": "\r"},
    ]));

    let owner = s.spawn_parent(Agent::Claude);

    let (parent, PanelSpec::Log { session, .. }) = s.next_panel_request() else {
        panic!("not a log panel");
    };
    assert_eq!(parent, Some(owner));
    let (text, is_error) = tool_text(&s.tool_reply("claude"));
    assert!(!is_error, "{text}");
    assert_eq!(text, format!("Opened panel {session}."));
}

#[test]
fn read_returns_the_last_lines_from_the_scrollback_too() {
    let s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    // Far more lines than the terminal has rows.
    let id = client.open("log", "count", "seq 1 300");
    client.wait_exited(&id);

    let read = client.read(&id, 3);

    assert_eq!(
        read,
        format!("Panel {id} exited with code 0.\n\n298\n299\n300")
    );
}

#[test]
fn a_running_panel_reads_as_running_and_its_wait_fails_after_the_timeout() {
    let s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    let id = client.open("command", "server", "echo started; sleep 600");

    let (text, is_error) = client.call(
        "panel_wait",
        json!({"id": id, "state": "exited", "timeout_seconds": 1}),
    );

    assert!(is_error);
    assert_eq!(text, format!("Panel {id} is still running after 1 s."));
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let read = client.read(&id, 1);
        if read == format!("Panel {id} is running.\n\nstarted") {
            break;
        }
        assert!(Instant::now() < deadline, "{read}");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn close_kills_every_process_of_the_panel_and_sends_panel_closed() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    let shell_pid = s.dir.path().join("shell.pid");
    let child_pid = s.dir.path().join("child.pid");
    let cmd = format!(
        "sleep 600 & echo $! > '{}'; echo $$ > '{}'; wait",
        child_pid.display(),
        shell_pid.display()
    );
    let id = client.open("command", "two sleeps", &cmd);
    let (shell, child) = (written(&shell_pid), written(&child_pid));
    assert!(is_alive(&shell) && is_alive(&child));

    let (text, is_error) = client.call("panel_close", json!({"id": id}));

    assert!(!is_error, "{text}");
    assert_eq!(text, format!("Closed panel {id}."));
    assert_eq!(next_panel_closed(&mut s), panel_id(&id));
    let deadline = Instant::now() + TIMEOUT;
    while is_alive(&shell) || is_alive(&child) {
        assert!(Instant::now() < deadline, "the panel's processes still run");
        thread::sleep(Duration::from_millis(10));
    }
    // Closed is gone.
    let (text, is_error) = client.call("panel_read", json!({"id": id}));
    assert!(is_error);
    assert!(text.ends_with(&format!("has no panel {id}")), "{text}");
}

#[test]
fn a_session_cannot_read_wait_on_or_close_another_sessions_panel() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut owner) = agent_with_client(&s, Agent::Claude);
    let (other, mut stranger) = agent_with_client(&s, Agent::Codex);
    let id = owner.open("command", "server", "sleep 600");
    s.next_panel_request();

    let refusal = format!("grove refused: session {other} has no panel {id}");
    for (tool, arguments) in [
        ("panel_read", json!({"id": id})),
        (
            "panel_wait",
            json!({"id": id, "state": "exited", "timeout_seconds": 1}),
        ),
        ("panel_close", json!({"id": id})),
    ] {
        assert_eq!(
            stranger.call(tool, arguments),
            (refusal.clone(), true),
            "{tool}"
        );
    }
    // A panel that does not exist gets the same refusal.
    assert_eq!(
        stranger.call("panel_read", json!({"id": "999"})),
        (
            format!("grove refused: session {other} has no panel 999"),
            true
        )
    );
    // The owner's panel still runs.
    assert!(!panel_closed_by_now(&mut s));
    assert!(
        owner
            .read(&id, 1)
            .starts_with(&format!("Panel {id} is running."))
    );
}

#[test]
fn a_session_holds_at_most_eight_panels_and_closing_one_frees_a_slot() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (owner, mut client) = agent_with_client(&s, Agent::Claude);
    // Each exits at once, and an exited panel still counts until it is closed.
    let ids: Vec<String> = (0..8)
        .map(|n| client.open("log", &format!("panel {n}"), "true"))
        .collect();
    for id in &ids {
        client.wait_exited(id);
    }
    for _ in &ids {
        s.next_panel_request();
    }

    let (text, is_error) = client.call(
        "panel_open",
        json!({"kind": "log", "title": "ninth", "cmd": "true"}),
    );

    assert!(is_error);
    assert_eq!(
        text,
        format!(
            "grove refused: session {owner} has 8 panels open, the most grove allows. Close \
             one first"
        )
    );
    assert!(!s.panel_requested());
    let (_, is_error) = client.call("panel_close", json!({"id": ids[0]}));
    assert!(!is_error);
    client.open("log", "ninth", "true");
    let (_, PanelSpec::Log { title, .. }) = s.next_panel_request() else {
        panic!("not a log panel");
    };
    assert_eq!(title, "ninth");
}

#[test]
fn a_panels_command_does_not_get_its_agent_sessions_token() {
    let s = setup(json!([{"wait_input": "\r"}]));
    let session = s.spawn_parent(Agent::Claude);
    let token = s.token("claude", session);
    let mut client = McpClient::start(&s.cfg.socket, session, &token);
    let id = client.open("log", "env", "env");
    client.wait_exited(&id);

    let read = client.read(&id, 1000);

    assert!(read.contains("PATH="), "{read}");
    assert!(!read.contains("GROVE_SESSION_TOKEN"), "{read}");
    assert!(!read.contains(&token), "{read}");
}

#[test]
fn a_panel_outlives_its_agent_session() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (owner, mut client) = agent_with_client(&s, Agent::Claude);
    let (_, mut other) = agent_with_client(&s, Agent::Codex);
    let pid = s.dir.path().join("panel.pid");
    let id = client.open(
        "command",
        "server",
        &format!("echo $$ > '{}'; sleep 600", pid.display()),
    );
    let pid = written(&pid);

    s.core.kill(owner).unwrap();

    // The core has handled the owner's exit once it sends the owner's final state.
    let mut closed = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match s.events.try_recv() {
            Ok(CoreEvent::AgentState { session, state, .. })
                if session == owner && state == AgentState::Exited =>
            {
                break;
            }
            Ok(CoreEvent::PanelClosed { panel }) => closed.push(panel),
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "the agent session never exited");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
    // A panel request after that passes through the socket task and the panel worker, so any
    // close the exit set off on either has been sent by the time it is answered.
    let other_panel = other.open("log", "after", "true");
    while let Ok(event) = s.events.try_recv() {
        if let CoreEvent::PanelClosed { panel } = event {
            closed.push(panel);
        }
    }

    assert!(closed.is_empty(), "closed {closed:?}");
    assert!(is_alive(&pid), "panel {id} ended with its agent session");
    assert_ne!(other_panel, id);
}

#[test]
fn removing_the_worktree_closes_its_panels() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    let pid = s.dir.path().join("panel.pid");
    let id = client.open(
        "command",
        "server",
        &format!("echo $$ > '{}'; sleep 600", pid.display()),
    );
    let pid = written(&pid);

    block_on(s.core.remove_worktree(&s.worktree, RemoveOpts::ForgetRepo)).unwrap();

    assert_eq!(next_panel_closed(&mut s), panel_id(&id));
    assert!(!is_alive(&pid));
}

#[test]
fn quitting_grove_sends_no_panel_closed() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    client.open("command", "server", "sleep 600");
    s.next_panel_request();
    // The setup owns its core, so a spare one on a home of its own takes its place.
    let (spare, _) = Core::start(CoreConfig::new(s.dir.path().join("spare"))).unwrap();

    drop(std::mem::replace(&mut s.core, spare));

    // Every event up to the bus closing. A session's wait thread holds a sender until its
    // killed process is reaped, so the bus can close a little after the drop.
    let mut events = Vec::new();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match s.events.try_recv() {
            Ok(event) => events.push(event),
            Err(TryRecvError::Closed) => break,
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "the event bus never closed");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, CoreEvent::PanelClosed { .. })),
        "{events:?}"
    );
}

#[test]
fn once_wait_says_exited_a_read_has_the_last_line_of_output() {
    // An attached panel that never acks holds the reader back after one chunk.
    let mut s = setup_with(json!([{"wait_input": "\r"}]), |cfg| {
        cfg.output_high_watermark = 1;
        cfg.output_low_watermark = 1;
    });
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    let attached = s.dir.path().join("attached");
    let first_read = s.dir.path().join("first-read");
    let until = |file: &Path| format!("until [ -e '{}' ]; do sleep 0.05; done", file.display());
    // The last line waits in the kernel's buffer behind the held reader as the command exits.
    let id = client.open(
        "log",
        "tests",
        &format!(
            "{}; echo first; {}; echo last",
            until(&attached),
            until(&first_read)
        ),
    );
    let (_, PanelSpec::Log { session, .. }) = s.next_panel_request() else {
        panic!("not a log panel");
    };
    let (sink, _frames) = unbounded_channel();
    s.core.attach(session, sink).unwrap();
    fs::write(&attached, "").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !client.read(&id, 1).ends_with("first") {
        assert!(
            Instant::now() < deadline,
            "the panel never showed its first line"
        );
        thread::sleep(Duration::from_millis(10));
    }
    fs::write(&first_read, "").unwrap();

    let (text, _) = client.call(
        "panel_wait",
        json!({"id": id, "state": "exited", "timeout_seconds": 1}),
    );
    if text != format!("Panel {id} exited with code 0.") {
        // Still running while the reader is held, which lets it go on.
        assert_eq!(text, format!("Panel {id} is still running after 1 s."));
        s.core.detach(session);
        client.wait_exited(&id);
    }

    assert_eq!(
        client.read(&id, 1),
        format!("Panel {id} exited with code 0.\n\nlast")
    );
}

#[test]
fn deleting_a_worktree_closes_its_panels() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let other = s.other_worktree();
    let spec = AgentSpec {
        agent: Agent::Claude,
        mode: Mode::Terminal,
        prompt: None,
        cols: 100,
        rows: 30,
        panel: None,
    };
    let owner = s.core.spawn_agent(&other, spec).unwrap();
    let token = s.token("claude", owner);
    let mut client = McpClient::start(&s.cfg.socket, owner, &token);
    let id = client.open("command", "server", "sleep 600");
    let (_, PanelSpec::Command { worktree, .. }) = s.next_panel_request() else {
        panic!("not a command panel");
    };
    assert_eq!(worktree, other);

    let delete = RemoveOpts::Delete {
        confirmed: true,
        delete_branch: false,
    };
    block_on(s.core.remove_worktree(&other, delete)).unwrap();

    assert_eq!(next_panel_closed(&mut s), panel_id(&id));
}

#[test]
fn close_kills_what_the_command_left_running_after_its_shell_exited() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    let pid_file = s.dir.path().join("server.pid");
    let id = client.open("command", "server", &left_running(&pid_file));
    let pid = written(&pid_file);
    client.wait_exited(&id);
    assert!(is_alive(&pid), "it should outlive its shell");

    let (text, is_error) = client.call("panel_close", json!({"id": id}));

    assert!(!is_error, "{text}");
    assert_eq!(next_panel_closed(&mut s), panel_id(&id));
    wait_until_dead(&pid);
}

#[test]
fn removing_the_worktree_kills_what_an_exited_panel_left_running() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    let pid_file = s.dir.path().join("server.pid");
    let id = client.open("command", "server", &left_running(&pid_file));
    let pid = written(&pid_file);
    client.wait_exited(&id);
    assert!(is_alive(&pid), "it should outlive its shell");

    block_on(s.core.remove_worktree(&s.worktree, RemoveOpts::ForgetRepo)).unwrap();

    assert_eq!(next_panel_closed(&mut s), panel_id(&id));
    wait_until_dead(&pid);
}

#[test]
fn grove_mcp_ends_when_its_client_leaves_during_a_long_wait() {
    let core = FakeCore::start();
    let mut client = McpClient::start(&core.socket, panel_id("1"), "token");
    writeln!(client.stdin(), "{}", long_wait(99)).unwrap();
    core.first_read
        .recv_timeout(TIMEOUT)
        .expect("the wait never asked grove");
    core.release.send(()).unwrap();

    client.stdin = None;

    wait_until_gone(&mut client);
}

#[test]
fn a_cancelled_wait_stops_asking_grove() {
    let core = FakeCore::start();
    let mut client = McpClient::start(&core.socket, panel_id("1"), "token");
    writeln!(client.stdin(), "{}", long_wait(99)).unwrap();
    core.first_read
        .recv_timeout(TIMEOUT)
        .expect("the wait never asked grove");

    let cancel = json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": 99}});
    writeln!(client.stdin(), "{cancel}").unwrap();
    // grove-mcp reads its input in order, so it has had the cancel once the ping is answered.
    client.send(json!({}), "ping");
    core.release.send(()).unwrap();
    // Its exit comes after any read the wait still makes.
    client.stdin = None;
    wait_until_gone(&mut client);

    assert_eq!(core.later_reads.load(Ordering::SeqCst), 0);
}

#[test]
fn quitting_grove_kills_what_an_exited_panel_left_running() {
    let mut s = setup(json!([{"wait_input": "\r"}]));
    let (_, mut client) = agent_with_client(&s, Agent::Claude);
    let pid_file = s.dir.path().join("server.pid");
    let id = client.open("command", "server", &left_running(&pid_file));
    let pid = written(&pid_file);
    client.wait_exited(&id);
    assert!(is_alive(&pid), "it should outlive its shell");
    // The setup owns its core, so a spare one on a home of its own takes its place.
    let (spare, _) = Core::start(CoreConfig::new(s.dir.path().join("spare"))).unwrap();

    drop(std::mem::replace(&mut s.core, spare));

    wait_until_dead(&pid);
}

/// A 600 s wait on panel 7, as a raw tool call with `id`.
fn long_wait(id: u64) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
        "name": "panel_wait",
        "arguments": {"id": "7", "state": "exited", "timeout_seconds": 600},
    }})
}

/// Waits for grove-mcp to exit after its client left. rmcp gives a call that still runs 5 s to
/// finish before it cancels it, so this allows well over that.
fn wait_until_gone(client: &mut McpClient) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while client.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "grove-mcp still runs");
        thread::sleep(Duration::from_millis(10));
    }
}

/// A stand-in core that says every panel is running. It holds its answer to the first
/// `panel_read` until `release` is sent, and counts the `panel_read`s after that one.
struct FakeCore {
    _dir: TempDir,
    socket: PathBuf,
    first_read: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
    later_reads: Arc<AtomicUsize>,
}

impl FakeCore {
    fn start() -> Self {
        let dir = short_temp_dir();
        let socket = dir.path().join("grove.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (first_tx, first_read) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let later_reads = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&later_reads);
        // Left blocked in accept when the test ends.
        thread::spawn(move || {
            let mut first = true;
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut message = Vec::new();
                stream.read_to_end(&mut message).unwrap();
                let message: Value = serde_json::from_slice(&message).unwrap();
                if message["request"]["type"] != "panel_read" {
                    continue;
                }
                if first {
                    first = false;
                    first_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                } else {
                    counted.fetch_add(1, Ordering::SeqCst);
                }
                let reply = json!({"result": {"lines": [], "running": true, "code": null}});
                let _ = stream.write_all(reply.to_string().as_bytes());
            }
        });
        Self {
            _dir: dir,
            socket,
            first_read,
            release,
            later_reads,
        }
    }
}

/// A command that starts a process which ignores the hangup, holds nothing of the terminal and
/// writes its pid to `pid_file`, then exits. The hangup is ignored before the fork, so the
/// process has it ignored from its start.
fn left_running(pid_file: &Path) -> String {
    format!(
        "trap '' HUP; sh -c 'echo $$ > \"{}\"; exec sleep 600' </dev/null >/dev/null 2>&1 &",
        pid_file.display()
    )
}

fn wait_until_dead(pid: &str) {
    let deadline = Instant::now() + TIMEOUT;
    while is_alive(pid) {
        assert!(Instant::now() < deadline, "process {pid} still runs");
        thread::sleep(Duration::from_millis(10));
    }
}
