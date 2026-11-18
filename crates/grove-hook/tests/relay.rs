//! Runs the real `grove-hook` binary against a temp socket (seam 2).

use std::env;
use std::ffi::OsStr;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{self, Child, Command, ExitStatus, Stdio};
use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const GROVE_HOOK: &str = env!("CARGO_BIN_EXE_grove-hook");

/// The hook relay contract: connect and write within 200 ms.
const BUDGET: Duration = Duration::from_millis(200);

/// How long a test waits for something that should happen, before it fails instead of hanging.
/// Not a bound under test: a loaded machine can take seconds to start a process.
const PATIENCE: Duration = Duration::from_secs(10);

/// A temp folder removed on drop. Short on purpose: macOS caps a socket path at 104 bytes.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "gh-{}-{}",
            process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let path = env::temp_dir().join(name);
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn socket(&self) -> PathBuf {
        self.0.join("grove.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Run {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// When grove-hook had taken all but the last pipe buffer of its input, so it was running.
    /// None when it exited before taking all of it.
    input_taken: Option<Instant>,
    exited: Instant,
}

/// Runs grove-hook with only the given environment, feeds it `stdin` and waits for it to exit.
fn run_hook(args: &[&str], envs: &[(&str, &dyn AsRef<OsStr>)], stdin: &[u8]) -> Run {
    // On macOS the first exec of grove-hook from a new test process takes about 150 ms
    // longer. That time is not grove-hook's own, so pay it before timing anything.
    static WARM_UP: Once = Once::new();
    WARM_UP.call_once(|| {
        let _ = Command::new(GROVE_HOOK)
            .env_clear()
            .stdin(Stdio::null())
            .status();
    });
    let mut child = Command::new(GROVE_HOOK)
        .args(args)
        .env_clear()
        .envs(envs.iter().map(|(key, value)| (key, value.as_ref())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let stdin = stdin.to_vec();
    // On its own thread, so a grove-hook that never reads fails the wait below, not hangs here.
    let writer = thread::spawn(move || input.write_all(&stdin).map(|()| Instant::now()));
    let status = wait(&mut child);
    let exited = Instant::now();
    // grove-hook may exit before reading all of stdin, and most tests allow that.
    let input_taken = writer.join().unwrap().ok();
    Run {
        status,
        stdout: read_all(child.stdout.take().unwrap()),
        stderr: read_all(child.stderr.take().unwrap()),
        input_taken,
        exited,
    }
}

/// Waits for grove-hook to exit, killing it if it outlasts `PATIENCE` so a hang fails the test.
fn wait(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("grove-hook still running after {PATIENCE:?}");
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn assert_silent_success(run: &Run) {
    assert_eq!(run.status.code(), Some(0), "exit code");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "", "stdout");
    assert_eq!(String::from_utf8_lossy(&run.stderr), "", "stderr");
}

/// Accepts the next connection, waiting up to `PATIENCE` for it.
fn accept(listener: &UnixListener) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("grove-hook never connected: {e}"),
        }
    }
}

fn assert_nothing_sent(listener: &UnixListener) {
    listener.set_nonblocking(true).unwrap();
    let accepted = listener.accept();
    assert!(
        matches!(&accepted, Err(e) if e.kind() == ErrorKind::WouldBlock),
        "grove-hook connected: {accepted:?}"
    );
}

/// Reads until the sender closes its end.
fn read_all(mut source: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    source.read_to_end(&mut bytes).unwrap();
    bytes
}

/// Parses the bytes as exactly one JSON message on one line.
fn one_message(bytes: &[u8]) -> Value {
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let line = text
        .strip_suffix('\n')
        .expect("message ends with a newline");
    assert!(!line.contains('\n'), "one message, got: {text}");
    serde_json::from_str(line).unwrap()
}

fn claude_payload(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/fake-agents/claude-code/payloads")
        .join(format!("{name}.json"));
    fs::read_to_string(path)
        .unwrap()
        .replace("{{session_id}}", "claude-abc")
}

#[test]
fn claude_payload_arrives_as_one_message_with_session_and_agent() {
    let dir = TempDir::new();
    let listener = UnixListener::bind(dir.socket()).unwrap();

    let run = run_hook(
        &["--session", "grove-1", "claude"],
        &[("GROVE_SOCKET", &dir.socket())],
        claude_payload("PreToolUse-AskUserQuestion").as_bytes(),
    );

    assert_silent_success(&run);
    assert_eq!(
        one_message(&read_all(accept(&listener))),
        json!({
            "session": "grove-1",
            "agent": "claude",
            "event": {
                "hook_event_name": "PreToolUse",
                "session_id": "claude-abc",
                "tool_name": "AskUserQuestion",
            },
        })
    );
}

#[test]
fn keeps_the_event_the_agent_session_id_and_the_matcher_field() {
    let cases = [
        (
            "SessionStart-clear",
            json!({"hook_event_name": "SessionStart", "session_id": "claude-abc", "source": "clear"}),
        ),
        (
            "SessionEnd-clear",
            json!({"hook_event_name": "SessionEnd", "session_id": "claude-abc", "reason": "clear"}),
        ),
        (
            "PermissionRequest-Bash",
            json!({"hook_event_name": "PermissionRequest", "session_id": "claude-abc", "tool_name": "Bash"}),
        ),
        (
            "Notification-permission_prompt",
            json!({"hook_event_name": "Notification", "session_id": "claude-abc", "notification_type": "permission_prompt"}),
        ),
        (
            "StopFailure",
            json!({"hook_event_name": "StopFailure", "session_id": "claude-abc", "error": "unknown"}),
        ),
        (
            "SubagentStop",
            json!({"hook_event_name": "SubagentStop", "session_id": "claude-abc", "agent_type": "general-purpose"}),
        ),
        (
            "Stop",
            json!({"hook_event_name": "Stop", "session_id": "claude-abc"}),
        ),
    ];
    for (payload, event) in cases {
        let dir = TempDir::new();
        let listener = UnixListener::bind(dir.socket()).unwrap();

        let run = run_hook(
            &["--session", "grove-1", "claude"],
            &[("GROVE_SOCKET", &dir.socket())],
            claude_payload(payload).as_bytes(),
        );

        assert_silent_success(&run);
        assert_eq!(
            one_message(&read_all(accept(&listener)))["event"],
            event,
            "{payload}"
        );
    }
}

#[test]
fn codex_takes_the_session_id_from_its_environment() {
    let dir = TempDir::new();
    let listener = UnixListener::bind(dir.socket()).unwrap();
    // Hand-written, because no Codex payloads are recorded yet. The relay reads the same fields.
    let payload = json!({
        "session_id": "codex-abc",
        "turn_id": "turn-1",
        "cwd": "/repo",
        "hook_event_name": "Stop",
    });

    let run = run_hook(
        &["codex"],
        &[
            ("GROVE_SOCKET", &dir.socket()),
            ("GROVE_SESSION_ID", &"grove-2"),
        ],
        payload.to_string().as_bytes(),
    );

    assert_silent_success(&run);
    assert_eq!(
        one_message(&read_all(accept(&listener))),
        json!({
            "session": "grove-2",
            "agent": "codex",
            "event": {"hook_event_name": "Stop", "session_id": "codex-abc"},
        })
    );
}

#[test]
fn socket_defaults_to_grovecode_in_home() {
    let home = TempDir::new();
    fs::create_dir(home.0.join(".grovecode")).unwrap();
    let listener = UnixListener::bind(home.0.join(".grovecode/grove.sock")).unwrap();

    let run = run_hook(
        &["--session", "grove-1", "claude"],
        &[("HOME", &home.0)],
        claude_payload("Stop").as_bytes(),
    );

    assert_silent_success(&run);
    assert_eq!(
        one_message(&read_all(accept(&listener)))["session"],
        "grove-1"
    );
}

#[test]
fn no_socket_exits_zero_at_once_with_no_output() {
    let dir = TempDir::new();
    let missing = dir.0.join("missing.sock");
    // A socket file nobody listens on, as left by a grove that did not shut down cleanly.
    let stale = dir.socket();
    drop(UnixListener::bind(&stale).unwrap());
    // Bigger than a pipe's buffer, so writing it ends only once grove-hook is reading it.
    // Real ones get this long: Stop carries Claude's whole last message.
    let mut payload: Value = serde_json::from_str(&claude_payload("Stop")).unwrap();
    payload["last_assistant_message"] = "x".repeat(1 << 18).into();
    let payload = payload.to_string();

    for socket in [missing, stale] {
        let run = run_hook(
            &["--session", "grove-1", "claude"],
            &[("GROVE_SOCKET", &socket)],
            payload.as_bytes(),
        );

        assert_silent_success(&run);
        // Timed from when grove-hook was running, so starting a process on a loaded machine
        // does not count. A relay that waited for a socket would wait out its budget. One that
        // tried the socket first, and gave up without reading, never takes its input.
        let input_taken = run.input_taken.expect("grove-hook did not read its input");
        let took = run.exited - input_taken;
        assert!(took < BUDGET, "{socket:?} took {took:?}");
    }
}

#[test]
fn socket_that_never_reads_gives_up_within_the_budget() {
    let dir = TempDir::new();
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let held = thread::spawn(move || {
        let stream = accept(&listener);
        (stream, Instant::now())
    });
    // A small message fits in the socket's send buffer (8 KB on macOS, about 200 KB on
    // Linux) and never blocks. 1 MB in a kept field cannot fit, so the write blocks
    // until grove-hook gives up.
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "claude-abc",
        "tool_name": "x".repeat(1 << 20),
    })
    .to_string();

    let run = run_hook(
        &["--session", "grove-1", "claude"],
        &[("GROVE_SOCKET", &dir.socket())],
        payload.as_bytes(),
    );

    let (stream, connected) = held.join().unwrap();
    assert_silent_success(&run);
    // Timed from the connection, so process start and parsing the payload do not count.
    let gave_up_after = run.exited - connected;
    assert!(
        gave_up_after < BUDGET + Duration::from_millis(50),
        "gave up after {gave_up_after:?}"
    );
    // Part of the message got through before the write blocked, and the rest never came.
    // The message is longer than the payload, so fewer bytes than the payload means the
    // message was cut off.
    let got = read_all(stream);
    assert!(!got.is_empty(), "grove-hook never wrote");
    assert!(got.len() < payload.len(), "the whole message got through");
}

#[test]
fn bad_json_exits_zero_and_sends_nothing() {
    let dir = TempDir::new();
    let listener = UnixListener::bind(dir.socket()).unwrap();

    for stdin in ["", "not json", "[1, 2]", "{\"hook_event_name\": "] {
        let run = run_hook(
            &["--session", "grove-1", "claude"],
            &[("GROVE_SOCKET", &dir.socket())],
            stdin.as_bytes(),
        );

        assert_silent_success(&run);
        assert_nothing_sent(&listener);
    }
}

#[test]
fn missing_session_id_exits_zero_and_sends_nothing() {
    let dir = TempDir::new();
    let listener = UnixListener::bind(dir.socket()).unwrap();
    let socket = dir.socket();
    let stop = claude_payload("Stop");
    let stop = stop.as_bytes();

    let runs = [
        run_hook(&["claude"], &[("GROVE_SOCKET", &socket)], stop),
        run_hook(
            &["--session", "", "claude"],
            &[("GROVE_SOCKET", &socket)],
            stop,
        ),
        run_hook(&["codex"], &[("GROVE_SOCKET", &socket)], stop),
        run_hook(
            &["codex"],
            &[("GROVE_SOCKET", &socket), ("GROVE_SESSION_ID", &"")],
            stop,
        ),
    ];

    for run in &runs {
        assert_silent_success(run);
    }
    assert_nothing_sent(&listener);
}
