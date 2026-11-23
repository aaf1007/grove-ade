//! Agent sessions in chat mode, driven by the fake Claude sidecar in fixtures/fake-agents.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use grove_core::{
    AccountInfo, Agent, AgentMessage, AgentSpec, AgentState, ContextReading, ContextWindow, Core,
    CoreConfig, CoreError, CoreEvent, InputKind, Mode, ModelInfo, PlanLimits, Question,
    QuestionOption, RemoveOpts, Reply, RequestId, SessionId, SlashCommand, SpawnSpec, StateSource,
    TurnOutcome, UsageReading, UsageWindow,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::sync::mpsc::unbounded_channel;

mod common;
use common::{grove_hook, now_unix_ms, times, untimed, write_script};

const TIMEOUT: Duration = Duration::from_secs(10);

// macOS caps a socket path at 104 bytes, and its default temp folder is long.
fn short_temp_dir() -> TempDir {
    tempfile::Builder::new()
        .prefix("grove")
        .tempdir_in("/tmp")
        .unwrap()
}

// The core owns its runtime, and dropping it inside an async context panics.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(future)
}

fn fakes() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/fake-agents")
}

/// A core tracking one fresh repo, with the Claude sidecar replaced by its fake playing `steps`.
struct Setup {
    _dir: TempDir,
    cfg: CoreConfig,
    core: Core,
    events: broadcast::Receiver<CoreEvent>,
    worktree: PathBuf,
    log: PathBuf,
}

fn setup(steps: Value) -> Setup {
    setup_tuned(steps, |_| {})
}

fn setup_tuned(steps: Value, tune: impl FnOnce(&mut CoreConfig)) -> Setup {
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
    let wrapper = dir.path().join("claude-sidecar");
    let script = format!(
        "export FAKE_CLAUDE_SIDECAR_SCENARIO='{}' FAKE_CLAUDE_SIDECAR_LOG='{}'\nexec '{}' \"$@\"",
        scenario.display(),
        log.display(),
        fakes().join("claude-sidecar").display(),
    );
    write_script(&wrapper, &script);
    let mut cfg = CoreConfig::new(dir.path().join("home"));
    cfg.claude_sidecar_command = wrapper;
    cfg.claude_command = "/opt/claude/bin/claude".into();
    cfg.hook_relay = grove_hook().to_path_buf();
    tune(&mut cfg);
    let (core, events) = Core::start(cfg.clone()).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    Setup {
        _dir: dir,
        cfg,
        core,
        events,
        worktree,
        log,
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

impl Setup {
    fn spawn(&self, prompt: Option<&str>) -> SessionId {
        self.core
            .spawn_agent(&self.worktree, chat_spec(Agent::Claude, prompt))
            .unwrap()
    }

    /// The fake's log lines so far.
    fn log(&self) -> Vec<Value> {
        fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
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

    /// The whole lines the core has sent the fake so far, as JSON.
    fn sent(&self) -> Vec<Value> {
        let stdin: String = self
            .log()
            .iter()
            .filter_map(|line| line["stdin"].as_str().map(str::to_owned))
            .collect();
        let whole = stdin.rfind('\n').map_or("", |end| &stdin[..end]);
        whole
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// Waits until the core has sent the fake `n` lines of type `kind`, and returns them.
    fn wait_sent(&self, kind: &str, n: usize) -> Vec<Value> {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let sent: Vec<Value> = self
                .sent()
                .into_iter()
                .filter(|line| line["type"] == kind)
                .collect();
            if sent.len() >= n {
                return sent;
            }
            assert!(
                Instant::now() < deadline,
                "the fake got {} {kind}",
                sent.len()
            );
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

    /// The next usage reading the core sends for Claude, skipping other events.
    fn next_usage_reading(&mut self) -> Option<UsageReading> {
        loop {
            if let CoreEvent::UsageReading { agent, reading } = self.next_event() {
                assert_eq!(agent, Agent::Claude);
                return reading;
            }
        }
    }

    /// The next context reading the core sends, skipping other events.
    fn next_context_reading(&mut self) -> (SessionId, ContextReading) {
        loop {
            if let CoreEvent::ContextReading { session, reading } = self.next_event() {
                return (session, reading);
            }
        }
    }

    /// Fails if an agent state comes within `wait`.
    fn no_state_for(&mut self, wait: Duration) {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            match self.events.try_recv() {
                Ok(CoreEvent::AgentState { state, .. }) => panic!("unexpected {state:?}"),
                Ok(_) | Err(TryRecvError::Empty) => thread::sleep(Duration::from_millis(1)),
                Err(e) => panic!("{e}"),
            }
        }
    }

    fn wait_exit(&mut self, session: SessionId) -> Option<i32> {
        loop {
            if let CoreEvent::SessionExited { session: s, code } = self.next_event()
                && s == session
            {
                return code;
            }
        }
    }
}

fn chat_spec(agent: Agent, prompt: Option<&str>) -> AgentSpec {
    AgentSpec {
        agent,
        mode: Mode::Chat,
        prompt: prompt.map(str::to_owned),
        cols: 120,
        rows: 40,
        panel: None,
    }
}

/// A subscribed sink's frames, each stamped when it came. A thread of its own receives and
/// stamps them, so a busy test never makes a frame look late. Its messages have their times
/// taken out, which the tests that check times read from `Core::subscribe` themselves.
struct Frames {
    arrivals: mpsc::Receiver<(Vec<AgentMessage>, Instant)>,
    frames: Vec<(Vec<AgentMessage>, Instant)>,
}

impl Frames {
    fn subscribe(core: &Core, session: SessionId) -> (Vec<AgentMessage>, Frames) {
        let (sink, mut rx) = unbounded_channel();
        let history = untimed(core.subscribe(session, sink).unwrap());
        let (stamped, arrivals) = mpsc::channel();
        // Ends once the core drops the sink.
        thread::spawn(move || {
            while let Some(frame) = rx.blocking_recv() {
                if stamped.send((untimed(frame), Instant::now())).is_err() {
                    return;
                }
            }
        });
        let frames = Frames {
            arrivals,
            frames: Vec::new(),
        };
        (history, frames)
    }

    /// Takes the frames that have come.
    fn take(&mut self) {
        self.frames.extend(self.arrivals.try_iter());
    }

    fn messages(&self) -> Vec<AgentMessage> {
        self.frames.iter().flat_map(|(f, _)| f.clone()).collect()
    }

    /// Takes the next frame, or None once the core has dropped the sink.
    fn next_frame(&mut self, deadline: Instant, waiting_for: &str) -> Option<&[AgentMessage]> {
        match self
            .arrivals
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(frame) => {
                self.frames.push(frame);
                self.frames.last().map(|(f, _)| f.as_slice())
            }
            Err(RecvTimeoutError::Disconnected) => None,
            Err(RecvTimeoutError::Timeout) => panic!("{waiting_for} never came"),
        }
    }

    /// Takes frames until one ends a turn.
    fn until_turn_end(&mut self) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let frame = self
                .next_frame(deadline, "the turn end")
                .expect("the sink closed before the turn ended");
            if frame
                .iter()
                .any(|m| matches!(m, AgentMessage::TurnEnded { .. }))
            {
                return;
            }
        }
    }

    /// Takes frames until the core drops the sink.
    fn until_closed(&mut self) {
        let deadline = Instant::now() + TIMEOUT;
        while self.next_frame(deadline, "the sink close").is_some() {}
    }
}

fn delta(text: &str) -> AgentMessage {
    AgentMessage::TextDelta { text: text.into() }
}

fn user(text: &str) -> AgentMessage {
    AgentMessage::User { text: text.into() }
}

fn started() -> AgentMessage {
    AgentMessage::TurnStarted { time_unix_ms: None }
}

fn ended() -> AgentMessage {
    AgentMessage::TurnEnded {
        outcome: TurnOutcome::Completed,
        time_unix_ms: None,
    }
}

fn send(message: Value) -> Value {
    json!({ "send": message })
}

/// One turn: waits for a message, then streams `deltas` and ends the turn.
fn turn(deltas: &[&str]) -> Vec<Value> {
    let mut steps = vec![
        json!({"wait_message": "user_message"}),
        send(json!({"type": "turn_start"})),
    ];
    steps.extend(
        deltas
            .iter()
            .map(|d| send(json!({"type": "text_delta", "text": d}))),
    );
    steps.push(send(json!({"type": "turn_end"})));
    steps
}

/// `steps`, then waits until the core is done with the session.
fn then_wait(mut steps: Vec<Value>) -> Value {
    steps.push(json!({"wait_message": "never"}));
    Value::Array(steps)
}

#[test]
fn a_message_makes_the_session_working_its_deltas_arrive_in_order_and_the_turn_end_makes_it_done() {
    let mut s = setup(then_wait(turn(&["Hel", "lo", " there"])));
    let session = s.spawn(None);
    let (history, mut frames) = Frames::subscribe(&s.core, session);
    assert_eq!(history, []);
    // The fake is up and reading before the clock starts.
    s.wait_log("file");

    let sent = Instant::now();
    s.core.send_message(session, "hi").unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Protocol)
    );
    let working = sent.elapsed();
    assert!(
        working < Duration::from_millis(100),
        "Working took {working:?}"
    );
    frames.until_turn_end();
    assert_eq!(
        frames.messages(),
        [
            user("hi"),
            started(),
            delta("Hel"),
            delta("lo"),
            delta(" there"),
            ended()
        ]
    );
    assert_eq!(
        s.next_state(),
        (session, AgentState::Done, StateSource::Protocol)
    );
    let line = s.wait_log("stdin");
    let sent: Value = serde_json::from_str(line["stdin"].as_str().unwrap()).unwrap();
    assert_eq!(sent, json!({"type": "user_message", "text": "hi"}));
}

#[test]
fn focus_after_the_turn_makes_done_idle() {
    let mut s = setup(then_wait(turn(&["ok"])));
    let session = s.spawn(Some("go"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Done, StateSource::Protocol)
    );

    s.core.focus(session);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Protocol)
    );
}

// The core has no blur, so a focus during the turn changes nothing by itself. The UI focuses a
// session again when its focused panel's session turns `Done` (#37), and that makes it `Idle`.
#[test]
fn focus_during_the_turn_makes_done_idle_once_the_ui_focuses_the_done_session() {
    let mut steps = turn(&["ok"]);
    steps.insert(2, json!({"sleep": 0.3}));
    let mut s = setup(then_wait(steps));
    let session = s.spawn(Some("go"));
    assert_eq!(s.next_state().1, AgentState::Working);

    s.core.focus(session);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Done, StateSource::Protocol)
    );
    s.core.focus(session);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Protocol)
    );
}

#[test]
fn a_burst_of_10000_deltas_arrives_whole_in_frames_at_least_a_flush_window_apart() {
    let texts: Vec<String> = (0..10_000).map(|i| format!("{i} ")).collect();
    let deltas: Vec<&str> = texts.iter().map(String::as_str).collect();
    let s = setup(then_wait(turn(&deltas)));
    let window = s.cfg.output_flush_window;
    assert_eq!(window, Duration::from_millis(8));
    let session = s.spawn(None);
    let (_, mut frames) = Frames::subscribe(&s.core, session);

    s.core.send_message(session, "go").unwrap();

    frames.until_turn_end();
    let streamed: Vec<String> = frames
        .messages()
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::TextDelta { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, texts);
    let count = frames.frames.len();
    assert!(count < 1000, "{count} frames for 10,000 deltas");
    // The burst is the frames with deltas. The user's message goes out long before it, and a
    // late turn end alone in a frame would pad the span.
    let has_delta = |(f, _): &(Vec<AgentMessage>, Instant)| {
        f.iter()
            .any(|m| matches!(m, AgentMessage::TextDelta { .. }))
    };
    let first = frames.frames.iter().position(has_delta).unwrap();
    let last = frames.frames.iter().rposition(has_delta).unwrap();
    let burst = &frames.frames[first..=last];
    let gaps: Vec<Duration> = burst.windows(2).map(|pair| pair[1].1 - pair[0].1).collect();
    // A frame stamped late makes the gap after it look short. So the burst as a whole must
    // span a window per gap, less one window for a late first frame, and each gap only has a
    // floor far below the window, yet far above frames sent with no wait.
    let span = burst[burst.len() - 1].1 - burst[0].1;
    let gap_count = u32::try_from(gaps.len()).unwrap();
    assert!(
        span + window >= window * gap_count,
        "{gap_count} gaps in {span:?}: {gaps:?}"
    );
    for gap in &gaps {
        assert!(*gap >= window / 8, "frames came {gap:?} apart: {gaps:?}");
    }
}

#[test]
fn each_subscribe_mid_stream_gets_the_messages_so_far_then_live_ones_with_no_gap_or_repeat() {
    let texts: Vec<String> = (0..5_000).map(|i| format!("{i} ")).collect();
    let mut steps = vec![
        json!({"wait_message": "user_message"}),
        send(json!({"type": "turn_start"})),
    ];
    for (i, text) in texts.iter().enumerate() {
        steps.push(send(json!({"type": "text_delta", "text": text})));
        if i % 50 == 0 {
            steps.push(json!({"sleep": 0.005}));
        }
    }
    steps.push(send(json!({"type": "turn_end"})));
    let s = setup(then_wait(steps));
    let session = s.spawn(Some("go"));

    // Each subscribe replaces the sink before it, while the deltas stream. Until it is replaced,
    // each sink sees its history, then its frames, with nothing missing or repeated: a prefix
    // of the whole conversation. The last one sees all of it.
    let mut seen_before = Vec::new();
    let (mut history, mut frames) = Frames::subscribe(&s.core, session);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        // Longer than the flush window, so each sink gets frames before the next replaces it.
        thread::sleep(Duration::from_millis(12));
        frames.take();
        // A subscribe while the end waits in the batch gets it in its history instead.
        if history.contains(&ended()) || frames.messages().contains(&ended()) {
            break;
        }
        assert!(Instant::now() < deadline, "the turn never ended");
        let (next_history, next_frames) = Frames::subscribe(&s.core, session);
        // Every frame to the old sink went out before the new one took over.
        frames.until_closed();
        seen_before.push([history, frames.messages()].concat());
        (history, frames) = (next_history, next_frames);
    }
    let seen_last = [history, frames.messages()].concat();
    let (whole, _) = Frames::subscribe(&s.core, session);

    assert_eq!(seen_last, whole);
    let mid_stream = seen_before
        .iter()
        .filter(|seen| seen.len() > 2 && seen.len() < whole.len())
        .count();
    assert!(mid_stream >= 10, "only {mid_stream} subscribes mid-stream");
    for (n, seen) in seen_before.iter().enumerate() {
        assert!(whole.starts_with(seen), "subscriber {n}");
    }
    let streamed: Vec<String> = whole
        .into_iter()
        .filter_map(|m| match m {
            AgentMessage::TextDelta { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, texts);
}

#[test]
fn a_sidecar_that_exits_non_zero_makes_the_session_failed() {
    let mut s = setup(json!([{"exit": 3}]));
    let session = s.spawn(None);

    assert_eq!(s.wait_exit(session), Some(3));
    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Protocol)
    );
}

#[test]
fn a_sidecar_that_exits_0_makes_the_session_exited() {
    let mut s = setup(json!([]));
    let session = s.spawn(None);

    assert_eq!(
        s.next_state(),
        (session, AgentState::Exited, StateSource::Protocol)
    );
}

// The kill ends every process in the session the sidecar leads, so this also shows it leads one.
#[test]
fn kill_makes_the_session_exited() {
    let mut s = setup(then_wait(Vec::new()));
    let session = s.spawn(None);
    s.wait_log("file");

    s.core.kill(session).unwrap();

    assert_eq!(s.wait_exit(session), None);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Exited, StateSource::Protocol)
    );
    assert!(matches!(
        s.core.send_message(session, "late"),
        Err(CoreError::SessionNotRunning(_))
    ));
}

#[test]
fn a_turn_that_ends_on_an_error_makes_the_session_failed_with_the_error_kind() {
    let steps = json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"send": {"type": "turn_end", "error": {"kind": "overloaded", "message": "API Error: 529 overloaded"}}},
        {"wait_message": "never"},
    ]);
    let mut s = setup(steps);
    let session = s.spawn(Some("go"));
    let (_, mut frames) = Frames::subscribe(&s.core, session);

    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Protocol)
    );
    frames.until_turn_end();
    assert!(frames.messages().contains(&AgentMessage::TurnEnded {
        outcome: TurnOutcome::Failed {
            kind: "overloaded".into(),
            message: "API Error: 529 overloaded".into(),
        },
        time_unix_ms: None,
    }));
}

#[test]
fn the_sidecar_starts_in_the_worktree_with_the_session_files_and_writes_nothing_there() {
    let mut s = setup(json!([]));
    let session = s.spawn(None);
    s.wait_exit(session);

    let launch = s.wait_log("argv");
    let dir = s.cfg.home.join("sessions").join(session.to_string());
    let settings = dir.join("claude-settings.json");
    let mcp = dir.join("mcp.json");
    let resume_id = s.core.resume_id(session).unwrap();
    let argv: Vec<&str> = launch["argv"]
        .as_array()
        .unwrap()
        .iter()
        .skip(1)
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(
        argv,
        [
            "--settings",
            settings.to_str().unwrap(),
            "--mcp-config",
            mcp.to_str().unwrap(),
            "--session-id",
            &resume_id,
            "--claude",
            "/opt/claude/bin/claude",
        ]
    );
    assert_eq!(launch["cwd"], s.worktree.to_str().unwrap());
    assert_eq!(launch["env"]["GROVE_SESSION_ID"], session.to_string());
    assert_eq!(
        launch["env"]["GROVE_SOCKET"],
        s.cfg.socket.to_str().unwrap()
    );
    // The files the fake read are the ones terminal mode passes, hooks included.
    let read: Vec<(String, String)> = s
        .log()
        .iter()
        .filter(|l| l.get("file").is_some())
        .map(|l| {
            (
                l["file"].as_str().unwrap().to_owned(),
                l["content"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        read,
        [
            (
                settings.to_str().unwrap().to_owned(),
                fs::read_to_string(&settings).unwrap()
            ),
            (
                mcp.to_str().unwrap().to_owned(),
                fs::read_to_string(&mcp).unwrap()
            ),
        ]
    );
    let hook = format!("--session {session} claude");
    assert!(read[0].1.contains(&hook), "{}", read[0].1);
    let entries: Vec<_> = fs::read_dir(&s.worktree)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(entries, [".git"]);
}

/// Asks the core socket for the calling session's id, as a panel request client does: the
/// request, then the end of its half of the connection, then the reply.
fn session_id_request(socket: &Path, session: &str, token: &str) -> Value {
    use std::io::Read;
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;

    let request = json!({"session": session, "token": token, "request": {"type": "session_id"}});
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(TIMEOUT)).unwrap();
    stream.write_all(request.to_string().as_bytes()).unwrap();
    stream.shutdown(Shutdown::Write).unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).unwrap();
    serde_json::from_slice(&reply).unwrap()
}

#[test]
fn a_chat_session_gets_its_token_where_it_gets_its_id_and_its_panel_requests_are_accepted() {
    use std::os::unix::fs::PermissionsExt;

    let s = setup(then_wait(vec![]));
    let session = s.spawn(None);

    let launch = s.wait_log("argv");
    let token = launch["env"]["GROVE_SESSION_TOKEN"].as_str().unwrap();
    let file = s
        .cfg
        .home
        .join("sessions")
        .join(session.to_string())
        .join("token");
    assert_eq!(fs::read_to_string(&file).unwrap(), token);
    let mode = fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    assert_eq!(
        session_id_request(&s.cfg.socket, &session.to_string(), token),
        json!({"result": {"session": session.to_string()}})
    );
}

/// Points `cfg` at the fake Claude Code for terminal mode, which waits until grove ends it and
/// logs to `claude.log` next to the home folder.
fn use_fake_claude(cfg: &mut CoreConfig) {
    let dir = cfg.home.parent().unwrap();
    let scenario = dir.join("claude-scenario.json");
    fs::write(
        &scenario,
        json!({"steps": [{"wait_input": "\r"}]}).to_string(),
    )
    .unwrap();
    let wrapper = dir.join("claude");
    let script = format!(
        "export FAKE_CLAUDE_SCENARIO='{}' FAKE_CLAUDE_LOG='{}'\nexec '{}' \"$@\"",
        scenario.display(),
        dir.join("claude.log").display(),
        fakes().join("claude").display(),
    );
    write_script(&wrapper, &script);
    cfg.claude_command = wrapper;
}

/// The GROVE_SESSION_TOKEN in the first env line of the fake log at `log`, once there is one.
fn logged_token(log: &Path) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let line = fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .find(|line| line.get("env").is_some());
        if let Some(line) = line {
            return line["env"]["GROVE_SESSION_TOKEN"]
                .as_str()
                .unwrap()
                .to_owned();
        }
        assert!(Instant::now() < deadline, "{} has no env", log.display());
        thread::sleep(Duration::from_millis(10));
    }
}

/// After a switch from a process that had `old` to one that has `new`, only `new` works.
fn assert_only_the_new_token_works(s: &Setup, session: SessionId, old: &str, new: &str) {
    let id = session.to_string();
    assert_ne!(old, new);
    assert_eq!(
        session_id_request(&s.cfg.socket, &id, old),
        json!({"error": "not authorized"})
    );
    assert_eq!(
        session_id_request(&s.cfg.socket, &id, new),
        json!({"result": {"session": id}})
    );
}

#[test]
fn switching_to_terminal_mode_gives_the_session_a_new_token_and_refuses_the_old_one() {
    let s = setup_tuned(then_wait(vec![]), use_fake_claude);
    let session = s.spawn(None);
    let chat_token = logged_token(&s.log);

    block_on(s.core.set_mode(session, Mode::Terminal)).unwrap();

    let terminal_token = logged_token(&s._dir.path().join("claude.log"));
    assert_only_the_new_token_works(&s, session, &chat_token, &terminal_token);
}

#[test]
fn switching_to_chat_mode_gives_the_session_a_new_token_and_refuses_the_old_one() {
    let s = setup_tuned(then_wait(vec![]), use_fake_claude);
    let spec = AgentSpec {
        mode: Mode::Terminal,
        ..chat_spec(Agent::Claude, None)
    };
    let session = s.core.spawn_agent(&s.worktree, spec).unwrap();
    let terminal_token = logged_token(&s._dir.path().join("claude.log"));

    // The switch waits a while for the history, so its runtime needs timers.
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(s.core.set_mode(session, Mode::Chat))
        .unwrap();

    let chat_token = logged_token(&s.log);
    assert_only_the_new_token_works(&s, session, &terminal_token, &chat_token);
}

#[test]
fn the_account_the_sidecar_reports_arrives_as_an_account_event() {
    let steps = json!([
        {"send": {"type": "account", "account": {"email": "ann@example.com", "organization": "Ann's org", "plan": "max"}}},
        {"send": {"type": "account", "account": null}},
        {"wait_message": "never"},
    ]);
    let mut s = setup(steps);
    s.spawn(None);

    let mut accounts = Vec::new();
    while accounts.len() < 2 {
        if let CoreEvent::Account { agent, account } = s.next_event() {
            accounts.push((agent, account));
        }
    }

    let ann = AccountInfo {
        email: Some("ann@example.com".into()),
        organization: Some("Ann's org".into()),
        plan: Some("max".into()),
        api_key_source: None,
    };
    assert_eq!(
        accounts,
        [(Agent::Claude, Some(ann)), (Agent::Claude, None)]
    );
}

#[test]
fn the_first_prompt_goes_as_the_first_message() {
    let mut s = setup(then_wait(turn(&["ok"])));
    let session = s.spawn(Some("fix the bug"));

    assert_eq!(s.next_state().1, AgentState::Working);
    let (history, _) = Frames::subscribe(&s.core, session);
    assert_eq!(history[0], user("fix the bug"));
}

// The prompt is queued before the sidecar starts, so even one that exits at once cannot lose
// it or fail the spawn. The exit says what became of the session, and the conversation says the
// prompt never reached the agent.
#[test]
fn a_first_prompt_is_kept_when_the_sidecar_exits_at_once() {
    let mut s = setup_tuned(json!([]), |cfg| {
        cfg.claude_sidecar_command = "/usr/bin/false".into()
    });

    let session = s.spawn(Some("fix the bug"));

    assert_eq!(s.wait_exit(session), Some(1));
    assert_eq!(
        s.next_state(),
        (session, AgentState::Failed, StateSource::Protocol)
    );
    let (history, _) = Frames::subscribe(&s.core, session);
    assert_eq!(history, [user("fix the bug"), not_delivered("fix the bug")]);
}

fn not_delivered(text: &str) -> AgentMessage {
    AgentMessage::NotDelivered { text: text.into() }
}

#[test]
fn a_message_the_sidecar_exits_before_starting_a_turn_on_shows_as_not_delivered() {
    let mut steps = turn(&["ok"]);
    steps.extend([json!({"wait_message": "user_message"}), json!({"exit": 0})]);
    let mut s = setup(Value::Array(steps));
    let session = s.spawn(Some("one"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    let (_, mut frames) = Frames::subscribe(&s.core, session);

    s.core.send_message(session, "two").unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Exited, StateSource::Protocol)
    );
    let deadline = Instant::now() + TIMEOUT;
    while frames.messages().len() < 2 {
        frames
            .next_frame(deadline, "the not delivered message")
            .expect("the sink closed");
    }
    assert_eq!(frames.messages(), [user("two"), not_delivered("two")]);
    let (history, _) = Frames::subscribe(&s.core, session);
    assert_eq!(
        history,
        [
            user("one"),
            started(),
            delta("ok"),
            ended(),
            user("two"),
            not_delivered("two")
        ]
    );
}

// The sidecar starts a turn on a message as it reads it, unless a turn runs. Then the message
// joins that turn, or starts the next one as that turn ends.
#[test]
fn a_message_sent_during_a_turn_is_not_delivered_only_when_the_sidecar_exits_before_the_turn_ends()
{
    let first_turn = || {
        vec![
            json!({"wait_message": "user_message"}),
            send(json!({"type": "turn_start"})),
            json!({"wait_message": "user_message"}),
        ]
    };
    let mut answered = first_turn();
    answered.extend([send(json!({"type": "turn_end"})), json!({"exit": 0})]);
    let mut crashed = first_turn();
    crashed.push(json!({"exit": 1}));

    for (steps, last) in [(answered, ended()), (crashed, not_delivered("and this"))] {
        let mut s = setup(Value::Array(steps));
        let session = s.spawn(Some("one"));
        assert_eq!(s.next_state().1, AgentState::Working);

        s.core.send_message(session, "and this").unwrap();

        s.wait_exit(session);
        let (history, _) = Frames::subscribe(&s.core, session);
        assert_eq!(history, [user("one"), started(), user("and this"), last]);
    }
}

#[test]
fn a_message_the_sidecar_started_a_turn_on_stays_delivered_when_it_exits_mid_turn() {
    let steps = json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"exit": 1},
    ]);
    let mut s = setup(steps);
    let session = s.spawn(Some("go"));

    s.wait_exit(session);

    let (history, _) = Frames::subscribe(&s.core, session);
    assert_eq!(history, [user("go"), started()]);
}

#[test]
fn lines_that_are_not_protocol_messages_are_skipped() {
    let steps = json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"send": {"type": "something_new", "text": "x"}},
        {"send": {"type": "text_delta"}},
        {"send": {"type": "text_delta", "text": "ok"}},
        {"send": {"type": "turn_end"}},
        {"wait_message": "never"},
    ]);
    let mut s = setup(steps);
    let session = s.spawn(Some("go"));
    let (_, mut frames) = Frames::subscribe(&s.core, session);

    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    frames.take();
    let (history, _) = Frames::subscribe(&s.core, session);
    assert_eq!(history, [user("go"), started(), delta("ok"), ended()]);
}

#[test]
fn tool_calls_and_results_are_kept_in_order_and_never_change_the_state() {
    let input = json!({"command": "npm test", "description": "Run the tests"});
    let steps = json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"send": {"type": "tool_call", "id": "toolu_03", "tool": "Bash", "input": input}},
        {"send": {"type": "tool_result", "id": "toolu_03", "output": "1 passed", "is_error": false}},
        {"send": {"type": "tool_call", "id": "toolu_04", "tool": "Read", "input": {"file_path": "x"}}},
        {"send": {"type": "tool_result", "id": "toolu_04", "output": "no such file", "is_error": true}},
        {"send": {"type": "tool_result", "id": "toolu_05", "output": ""}},
        {"send": {"type": "text_delta", "text": "ok"}},
        {"send": {"type": "turn_end"}},
        {"wait_message": "never"},
    ]);
    let mut s = setup(steps);
    let session = s.spawn(Some("go"));

    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    let (history, _) = Frames::subscribe(&s.core, session);
    let result = |id: &str, output: &str, is_error| AgentMessage::ToolResult {
        id: id.into(),
        output: output.into(),
        is_error,
    };
    assert_eq!(
        history,
        [
            user("go"),
            started(),
            AgentMessage::ToolCall {
                id: "toolu_03".into(),
                tool: "Bash".into(),
                input,
                time_unix_ms: None,
            },
            result("toolu_03", "1 passed", false),
            AgentMessage::ToolCall {
                id: "toolu_04".into(),
                tool: "Read".into(),
                input: json!({"file_path": "x"}),
                time_unix_ms: None,
            },
            result("toolu_04", "no such file", true),
            result("toolu_05", "", false),
            delta("ok"),
            ended()
        ]
    );
}

#[test]
fn a_tool_call_and_its_turn_from_the_sidecar_carry_the_times_the_core_got_them() {
    let steps = json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"send": {"type": "tool_call", "id": "toolu_03", "tool": "Bash", "input": {"command": "ls"}}},
        {"send": {"type": "turn_end"}},
        {"wait_message": "never"},
    ]);
    let mut s = setup(steps);
    let before = now_unix_ms();
    let session = s.spawn(Some("go"));

    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    let after = now_unix_ms();
    let (sink, _frames) = unbounded_channel();
    let stamped = times(&s.core.subscribe(session, sink).unwrap());
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
fn hooks_never_set_a_chat_mode_session_state() {
    let mut s = setup(then_wait(turn(&["ok"])));
    let session = s.spawn(None);
    let resume_id = s.core.resume_id(session).unwrap();

    for payload in ["UserPromptSubmit", "Stop"] {
        run_hook(&s, session, payload, &resume_id);
    }
    s.no_state_for(Duration::from_millis(300));
    s.core.send_message(session, "go").unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Protocol)
    );
}

/// Runs grove-hook as Claude's hook would, with a recorded payload and its session_id.
fn run_hook(s: &Setup, session: SessionId, payload: &str, session_id: &str) {
    let path = fakes().join(format!("claude-code/payloads/{payload}.json"));
    let mut payload: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    payload["session_id"] = session_id.into();
    let mut child = Command::new(&s.cfg.hook_relay)
        .args(["--session", &session.to_string(), "claude"])
        .env("GROVE_SOCKET", &s.cfg.socket)
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

#[test]
fn terminal_calls_fail_on_a_chat_session_and_chat_calls_on_a_terminal_session() {
    let s = setup(then_wait(Vec::new()));
    let chat = s.spawn(None);
    let shell = s
        .core
        .spawn(
            &s.worktree,
            SpawnSpec {
                command: "/bin/sh".into(),
                args: Vec::new(),
                cols: 80,
                rows: 24,
            },
        )
        .unwrap();

    assert!(matches!(
        s.core.write(chat, b"x"),
        Err(CoreError::NoTerminal(id)) if id == chat
    ));
    assert!(matches!(
        s.core.resize(chat, 80, 24),
        Err(CoreError::NoTerminal(_))
    ));
    assert!(matches!(
        s.core.attach(chat, unbounded_channel().0),
        Err(CoreError::NoTerminal(_))
    ));
    assert!(matches!(
        s.core.send_message(shell, "hi"),
        Err(CoreError::NotInChatMode(id)) if id == shell
    ));
    assert!(matches!(
        s.core.subscribe(shell, unbounded_channel().0),
        Err(CoreError::NotInChatMode(_))
    ));
    assert!(matches!(
        s.core.answer(shell, id("r1"), Reply::AllowOnce),
        Err(CoreError::NotInChatMode(_))
    ));
    assert!(matches!(
        s.core.interrupt(shell),
        Err(CoreError::NotInChatMode(_))
    ));
}

#[test]
fn removing_the_worktree_ends_its_chat_session() {
    let mut s = setup(then_wait(Vec::new()));
    let repo = s.worktree.clone();
    let session = s.spawn(None);
    s.wait_log("file");

    block_on(s.core.remove_worktree(&repo, RemoveOpts::ForgetRepo)).unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Exited, StateSource::Protocol)
    );
}

const APPROVAL: AgentState = AgentState::NeedsInput {
    kind: InputKind::Approval,
};
const QUESTION: AgentState = AgentState::NeedsInput {
    kind: InputKind::Question,
};

fn id(id: &str) -> RequestId {
    RequestId(id.into())
}

fn approval_request(id: &str) -> Value {
    send(
        json!({"type": "approval_request", "id": id, "tool": "Bash", "input": {"command": "npm test"}}),
    )
}

fn question_request(id: &str) -> Value {
    send(json!({"type": "question_request", "id": id, "questions": [
        {"question": "Which runner?", "header": "Runner", "multi_select": false, "options": [
            {"label": "vitest", "description": "Fast"},
            {"label": "jest", "description": "Familiar"},
        ]},
        {"question": "Which kinds?", "header": "Kinds", "multi_select": true, "options": [
            {"label": "unit", "description": "Small"},
            {"label": "integration", "description": "Wide"},
        ]},
    ]}))
}

/// One turn that makes `requests`, a little apart, waits for an answer to each, then says
/// "done" and ends.
fn asking(requests: Vec<Value>) -> Value {
    let answers = requests.len();
    let mut steps = vec![
        json!({"wait_message": "user_message"}),
        send(json!({"type": "turn_start"})),
    ];
    for (n, request) in requests.into_iter().enumerate() {
        if n > 0 {
            steps.push(json!({"sleep": 0.05}));
        }
        steps.push(request);
    }
    steps.extend((0..answers).map(|_| json!({"wait_message": "answer"})));
    steps.push(send(json!({"type": "text_delta", "text": "done"})));
    steps.push(send(json!({"type": "turn_end"})));
    then_wait(steps)
}

fn approval_asked(id: &str) -> AgentMessage {
    AgentMessage::ApprovalRequested {
        request: RequestId(id.into()),
        tool: "Bash".into(),
        input: json!({"command": "npm test"}),
    }
}

fn answered(id: &str, reply: Reply) -> AgentMessage {
    AgentMessage::Answered {
        request: RequestId(id.into()),
        reply,
    }
}

/// Waits until the session has `n` requests in its history, so a test answers only once the
/// sidecar has made them all.
fn wait_requests(core: &Core, session: SessionId, n: usize) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (history, _) = Frames::subscribe(core, session);
        let made = history
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    AgentMessage::ApprovalRequested { .. } | AgentMessage::QuestionAsked { .. }
                )
            })
            .count();
        if made >= n {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "only {made} of {n} requests came"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn answers(answers: &[&[&str]]) -> Reply {
    Reply::Answers(
        answers
            .iter()
            .map(|a| a.iter().map(|s| s.to_string()).collect())
            .collect(),
    )
}

#[test]
fn an_approval_request_makes_the_session_need_approval_and_allow_once_reaches_the_sidecar() {
    // The request goes out as soon as a second message arrives, so the clock starts with it.
    let steps = json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"wait_message": "user_message"},
        approval_request("r1"),
        {"wait_message": "answer"},
        {"send": {"type": "text_delta", "text": "done"}},
        {"send": {"type": "turn_end"}},
        {"wait_message": "never"},
    ]);
    let mut s = setup(steps);
    let session = s.spawn(Some("go"));
    assert_eq!(s.next_state().1, AgentState::Working);

    let sent = Instant::now();
    s.core.send_message(session, "now").unwrap();

    assert_eq!(s.next_state(), (session, APPROVAL, StateSource::Protocol));
    let waited = sent.elapsed();
    assert!(
        waited < Duration::from_millis(100),
        "NeedsInput took {waited:?}"
    );
    // The request is in the history by the time the state says so, and never comes again.
    let (history, mut frames) = Frames::subscribe(&s.core, session);
    assert_eq!(
        history,
        [user("go"), started(), user("now"), approval_asked("r1")]
    );

    s.core.answer(session, id("r1"), Reply::AllowOnce).unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Working, StateSource::Protocol)
    );
    assert_eq!(
        s.wait_sent("answer", 1),
        [json!({"type": "answer", "id": "r1", "reply": "allow_once"})]
    );
    assert_eq!(s.next_state().1, AgentState::Done);
    frames.until_turn_end();
    assert_eq!(
        frames.messages(),
        [answered("r1", Reply::AllowOnce), delta("done"), ended()]
    );
}

#[test]
fn deny_reaches_the_sidecar_and_the_state_follows_it() {
    let mut s = setup(asking(vec![approval_request("r1"), approval_request("r2")]));
    let session = s.spawn(Some("go"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, APPROVAL);
    wait_requests(&s.core, session, 2);

    let deny = Reply::Deny {
        message: Some("use pnpm".into()),
    };
    s.core.answer(session, id("r1"), deny.clone()).unwrap();
    // r2 still waits.
    s.no_state_for(Duration::from_millis(200));
    let plain_deny = Reply::Deny { message: None };
    s.core
        .answer(session, id("r2"), plain_deny.clone())
        .unwrap();

    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(
        s.wait_sent("answer", 2),
        [
            json!({"type": "answer", "id": "r1", "reply": "deny", "message": "use pnpm"}),
            json!({"type": "answer", "id": "r2", "reply": "deny"}),
        ]
    );
    assert_eq!(s.next_state().1, AgentState::Done);
    let (history, _) = Frames::subscribe(&s.core, session);
    assert_eq!(
        history[4..],
        [
            answered("r1", deny),
            answered("r2", plain_deny),
            delta("done"),
            ended()
        ]
    );
}

#[test]
fn a_question_makes_the_session_need_an_answer_and_the_answers_reach_the_sidecar() {
    let mut s = setup(asking(vec![question_request("q1")]));
    let session = s.spawn(Some("go"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state(), (session, QUESTION, StateSource::Protocol));
    let (history, _) = Frames::subscribe(&s.core, session);
    let option = |label: &str, description: &str| QuestionOption {
        label: label.into(),
        description: description.into(),
    };
    assert_eq!(
        history[2],
        AgentMessage::QuestionAsked {
            request: id("q1"),
            questions: vec![
                Question {
                    question: "Which runner?".into(),
                    header: "Runner".into(),
                    options: vec![option("vitest", "Fast"), option("jest", "Familiar")],
                    multi_select: false,
                },
                Question {
                    question: "Which kinds?".into(),
                    header: "Kinds".into(),
                    options: vec![option("unit", "Small"), option("integration", "Wide")],
                    multi_select: true,
                },
            ],
        }
    );

    let reply = answers(&[&["vitest"], &["unit", "integration"]]);
    s.core.answer(session, id("q1"), reply).unwrap();

    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(
        s.wait_sent("answer", 1),
        [
            json!({"type": "answer", "id": "q1", "reply": "answers", "answers": [["vitest"], ["unit", "integration"]]})
        ]
    );
    assert_eq!(s.next_state().1, AgentState::Done);
}

#[test]
fn an_answer_for_a_request_that_is_not_pending_or_does_not_fit_it_errors_and_changes_nothing() {
    let mut s = setup(asking(vec![approval_request("r1"), question_request("q1")]));
    let session = s.spawn(Some("go"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, APPROVAL);
    wait_requests(&s.core, session, 2);
    let (before, _) = Frames::subscribe(&s.core, session);
    assert_eq!(before.len(), 4);

    let not_pending = s.core.answer(session, id("r9"), Reply::AllowOnce);
    assert!(
        matches!(not_pending, Err(CoreError::NoPendingRequest { .. })),
        "{not_pending:?}"
    );
    for (request, reply) in [
        ("r1", answers(&[&["vitest"], &["unit"]])),
        ("q1", Reply::AllowOnce),
        ("q1", Reply::AllowForSession),
        ("q1", answers(&[&["vitest"]])),
    ] {
        let wrong = s.core.answer(session, id(request), reply);
        assert!(
            matches!(wrong, Err(CoreError::WrongReply { .. })),
            "{request}: {wrong:?}"
        );
    }

    s.no_state_for(Duration::from_millis(200));
    assert_eq!(Frames::subscribe(&s.core, session).0, before);
    assert!(!s.sent().iter().any(|line| line["type"] == "answer"));
    // Both are still pending, and each takes one answer.
    s.core.answer(session, id("r1"), Reply::AllowOnce).unwrap();
    assert_eq!(s.next_state().1, QUESTION);
    let again = s.core.answer(session, id("r1"), Reply::AllowOnce);
    assert!(
        matches!(again, Err(CoreError::NoPendingRequest { .. })),
        "{again:?}"
    );
    s.core
        .answer(session, id("q1"), answers(&[&["jest"], &[]]))
        .unwrap();
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    assert_eq!(s.wait_sent("answer", 2).len(), 2);
}

#[test]
fn interrupt_mid_turn_reaches_the_sidecar_and_leaves_the_session_idle() {
    let steps = json!([
        {"wait_message": "user_message"},
        {"send": {"type": "turn_start"}},
        {"send": {"type": "text_delta", "text": "working on it"}},
        approval_request("r1"),
        {"wait_message": "interrupt"},
        {"send": {"type": "turn_end", "interrupted": true}},
        {"wait_message": "never"},
    ]);
    let mut s = setup(steps);
    let session = s.spawn(Some("go"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, APPROVAL);

    let interrupted = Instant::now();
    s.core.interrupt(session).unwrap();

    assert_eq!(
        s.next_state(),
        (session, AgentState::Idle, StateSource::Protocol)
    );
    let took = interrupted.elapsed();
    assert!(took < Duration::from_secs(2), "Idle took {took:?}");
    assert_eq!(s.wait_sent("interrupt", 1), [json!({"type": "interrupt"})]);
    // The turn's requests ended with it.
    let late = s.core.answer(session, id("r1"), Reply::AllowOnce);
    assert!(
        matches!(late, Err(CoreError::NoPendingRequest { .. })),
        "{late:?}"
    );
    let (history, _) = Frames::subscribe(&s.core, session);
    assert_eq!(
        history.last(),
        Some(&AgentMessage::TurnEnded {
            outcome: TurnOutcome::Interrupted,
            time_unix_ms: None,
        })
    );
}

#[test]
fn interrupt_and_answer_fail_after_the_sidecar_exits() {
    let mut s = setup(json!([]));
    let session = s.spawn(None);
    s.wait_exit(session);

    assert!(matches!(
        s.core.interrupt(session),
        Err(CoreError::SessionNotRunning(_))
    ));
    assert!(matches!(
        s.core.answer(session, id("r1"), Reply::AllowOnce),
        Err(CoreError::SessionNotRunning(_))
    ));
}

/// Two requests in one turn, answered in `order`. The approval ranks first while both wait.
fn two_requests_answered_in(order: [&str; 2]) {
    let mut s = setup(asking(vec![approval_request("r1"), question_request("q1")]));
    let session = s.spawn(Some("go"));
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, APPROVAL);
    wait_requests(&s.core, session, 2);

    let reply = |request: &str| match request {
        "r1" => Reply::AllowForSession,
        _ => answers(&[&["jest"], &["unit"]]),
    };
    s.core
        .answer(session, id(order[0]), reply(order[0]))
        .unwrap();
    if order[0] == "r1" {
        assert_eq!(s.next_state().1, QUESTION);
    } else {
        s.no_state_for(Duration::from_millis(200));
    }
    s.core
        .answer(session, id(order[1]), reply(order[1]))
        .unwrap();

    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
    let sent: Vec<Value> = s
        .wait_sent("answer", 2)
        .iter()
        .map(|line| line["id"].clone())
        .collect();
    assert_eq!(sent, order.map(|id| json!(id)));
    let (history, _) = Frames::subscribe(&s.core, session);
    assert_eq!(history[4..6], order.map(|id| answered(id, reply(id))));
}

#[test]
fn two_pending_requests_are_answered_separately_approval_first() {
    two_requests_answered_in(["r1", "q1"]);
}

#[test]
fn two_pending_requests_are_answered_separately_question_first() {
    two_requests_answered_in(["q1", "r1"]);
}

// The sidecar ends each turn the moment the answer arrives, so the turn end races the state
// the answer sets. The answer's Working must still come first.
#[test]
fn the_working_an_answer_sets_always_comes_before_the_turn_end_it_lets_through() {
    const TURNS: usize = 30;
    let mut steps = Vec::new();
    for turn in 0..TURNS {
        steps.extend([
            json!({"wait_message": "user_message"}),
            send(json!({"type": "turn_start"})),
            approval_request(&format!("r{turn}")),
            json!({"wait_message": "answer"}),
            send(json!({"type": "turn_end"})),
        ]);
    }
    let mut s = setup(then_wait(steps));
    let session = s.spawn(None);

    for turn in 0..TURNS {
        s.core.send_message(session, "go").unwrap();
        assert_eq!(s.next_state().1, AgentState::Working, "turn {turn}");
        assert_eq!(s.next_state().1, APPROVAL, "turn {turn}");
        s.core
            .answer(session, id(&format!("r{turn}")), Reply::AllowOnce)
            .unwrap();
        assert_eq!(s.next_state().1, AgentState::Working, "turn {turn}");
        assert_eq!(s.next_state().1, AgentState::Done, "turn {turn}");
    }
}

/// fixtures/chat-session/session.jsonl: a real Claude session's protocol stream, each line timed.
fn chat_recording() -> Vec<Value> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/chat-session/session.jsonl");
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The recording's first user message and a turn start for it, then the busiest `seconds` of
/// the recording, moved to start right after them. Without the turn start, the message would
/// never reach the agent.
fn recording_slice(seconds: f64) -> Vec<Value> {
    let lines = chat_recording();
    let t = |line: &Value| line["t"].as_f64().unwrap();
    let window = |start: usize| {
        lines[start..]
            .iter()
            .take_while(|line| line["from"] == "sidecar" && t(line) <= t(&lines[start]) + seconds)
            .count()
    };
    let busiest = (0..lines.len()).max_by_key(|&start| window(start)).unwrap();
    let user = lines.iter().find(|line| line["from"] == "core").unwrap();
    let start = t(&lines[busiest]);
    let mut slice = vec![
        json!({"t": start, "from": "core", "message": user["message"]}),
        json!({"t": start, "from": "sidecar", "message": {"type": "turn_start"}}),
    ];
    slice.extend_from_slice(&lines[busiest..busiest + window(busiest)]);
    assert!(slice.len() > 20, "only {} lines in the slice", slice.len());
    slice
}

/// A recording replayed through the core.
struct Replayed {
    _setup: Setup,
    /// The recorded lines that reach the sink.
    expected: Vec<Played>,
    /// What the sink got, each message with the time its frame came.
    got: Vec<(AgentMessage, Instant)>,
}

/// One recorded line that reaches the sink.
struct Played {
    /// When it was recorded, in seconds since the recording started.
    recorded: f64,
    /// The Unix time in seconds when the fake heard it, if the core sent it, or had sent it.
    at_fake: f64,
    /// The Unix time just before a sidecar write. Core lines have no sidecar write.
    before_send: Option<f64>,
    /// The message the sink should get for it.
    message: AgentMessage,
}

/// An Instant and its Unix time, for comparing sink arrivals to the fake's log.
fn clock_anchor(
    mut sample: impl FnMut() -> (Instant, SystemTime, Instant),
) -> (Instant, SystemTime) {
    // Keep the clock conversion error within the replay test's 1 ms clock tolerance.
    const MAX_SAMPLE_SPAN: Duration = Duration::from_millis(1);
    loop {
        let (before, wall, after) = sample();
        let span = after - before;
        if span <= MAX_SAMPLE_SPAN {
            return (before + span / 2, wall);
        }
    }
}

#[test]
fn a_pause_while_sampling_the_clocks_does_not_move_a_sink_arrival() {
    let arrival = Instant::now();
    let unix_at_arrival = UNIX_EPOCH + Duration::from_secs(100);
    let pause = Duration::from_millis(300);
    let resumed = arrival + pause;
    let wall = unix_at_arrival + pause;
    let mut samples = [(arrival, wall, resumed), (resumed, wall, resumed)].into_iter();

    let (anchor, unix) = clock_anchor(|| samples.next().unwrap());

    assert_eq!(unix - anchor.duration_since(arrival), unix_at_arrival);
}

/// Plays `lines` through the fake sidecar at `speed` until the fake exits, sending each recorded
/// user message once the replay reaches it.
fn replay(lines: &[Value], speed: f64) -> Replayed {
    let dir = short_temp_dir();
    let path = dir.path().join("recording.jsonl");
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(&path, text).unwrap();
    let mut s = setup(json!([{"replay": path, "speed": speed}]));
    let session = s.spawn(None);
    let (history, mut frames) = Frames::subscribe(&s.core, session);
    let subscribed = Instant::now();
    let launch = s.wait_log("argv");
    let slots = [
        ("cwd", launch["cwd"].as_str().unwrap().to_owned()),
        ("home", launch["env"]["HOME"].as_str().unwrap().to_owned()),
        ("session_id", s.core.resume_id(session).unwrap()),
    ];
    let span = lines.last().unwrap()["t"].as_f64().unwrap() - lines[0]["t"].as_f64().unwrap();
    let deadline = Instant::now() + Duration::from_secs_f64(span / speed) + TIMEOUT;
    let received =
        |frames: &Frames| history.len() + frames.frames.iter().map(|(f, _)| f.len()).sum::<usize>();
    // Each with its line's place in the recording.
    let mut expected = Vec::new();
    for (place, line) in lines.iter().enumerate() {
        let mut message = line["message"].clone();
        fill(&mut message, &slots);
        let Some(sink_message) = sink_message(&message) else {
            continue;
        };
        if line["from"] == "core" {
            // The replay waits for it after the lines before it, so it goes once they all came.
            while received(&frames) < expected.len() {
                frames
                    .next_frame(deadline, "the replay before a user message")
                    .expect("the sink closed before the replay before a user message");
            }
            let text = message["text"].as_str().unwrap();
            s.core.send_message(session, text).unwrap();
        }
        expected.push((place, line["t"].as_f64().unwrap(), sink_message));
    }
    while received(&frames) < expected.len() {
        frames
            .next_frame(deadline, "the end of the replay")
            .expect("the sink closed before the end of the replay");
    }
    // The fake exits after its last line. Anything the core sends after it would come by then.
    s.wait_exit(session);
    thread::sleep(Duration::from_millis(50));
    frames.take();
    let log = s.wait_log("replay_times");
    let mut send_starts = log["replay_send_starts"].as_array().unwrap().iter();
    // The send starts cover only sidecar lines, including those the sink never sees.
    let before_send: Vec<_> = lines
        .iter()
        .map(|line| {
            (line["from"] == "sidecar").then(|| send_starts.next().unwrap().as_f64().unwrap())
        })
        .collect();
    assert!(send_starts.next().is_none());
    let expected = expected
        .into_iter()
        .map(|(place, recorded, message)| Played {
            recorded,
            at_fake: log["replay_times"][place].as_f64().unwrap(),
            before_send: before_send[place],
            message,
        })
        .collect();
    let mut got: Vec<(AgentMessage, Instant)> =
        history.into_iter().map(|m| (m, subscribed)).collect();
    for (frame, at) in frames.frames {
        got.extend(frame.into_iter().map(|m| (m, at)));
    }
    Replayed {
        _setup: s,
        expected,
        got,
    }
}

/// Fills the recording's `{{name}}` placeholders in every string, as the fake does.
fn fill(value: &mut Value, slots: &[(&str, String)]) {
    match value {
        Value::String(text) => {
            for (name, with) in slots {
                *text = text.replace(&format!("{{{{{name}}}}}"), with);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| fill(v, slots)),
        Value::Object(map) => map.values_mut().for_each(|v| fill(v, slots)),
        _ => {}
    }
}

/// The message the sink gets for a recorded protocol message, if any.
fn sink_message(message: &Value) -> Option<AgentMessage> {
    let text = |key: &str| message[key].as_str().unwrap().to_owned();
    Some(match message["type"].as_str().unwrap() {
        "user_message" => user(&text("text")),
        "turn_start" => started(),
        "text_delta" => delta(&text("text")),
        "tool_call" => AgentMessage::ToolCall {
            id: text("id"),
            tool: text("tool"),
            input: message["input"].clone(),
            time_unix_ms: None,
        },
        "tool_result" => AgentMessage::ToolResult {
            id: text("id"),
            output: text("output"),
            is_error: message["is_error"].as_bool().unwrap_or(false),
        },
        "turn_end" => AgentMessage::TurnEnded {
            time_unix_ms: None,
            outcome: if message["interrupted"] == true {
                TurnOutcome::Interrupted
            } else if let Some(error) = message.get("error") {
                TurnOutcome::Failed {
                    kind: error["kind"].as_str().unwrap().to_owned(),
                    message: error["message"].as_str().unwrap().to_owned(),
                }
            } else {
                TurnOutcome::Completed
            },
        },
        // The core sends the account as an event, not into the conversation.
        "account" => return None,
        other => panic!("a replay cannot play {other}"),
    })
}

impl Replayed {
    /// The sink got exactly the recorded messages, in order. Names the first difference, since
    /// a whole recording is too long to print.
    fn assert_whole_and_in_order(&self) {
        let differs = self
            .expected
            .iter()
            .zip(&self.got)
            .position(|(want, (got, _))| want.message != *got);
        if let Some(i) = differs {
            panic!(
                "message {i} differs: want {:?}, got {:?}",
                self.expected[i].message, self.got[i].0
            );
        }
        assert_eq!(self.got.len(), self.expected.len(), "message count");
    }

    fn assert_on_time(&self, speed: f64) {
        let (anchor, unix) = clock_anchor(|| (Instant::now(), SystemTime::now(), Instant::now()));
        let arrivals = self
            .expected
            .iter()
            .zip(&self.got)
            .map(|(played, (_, at))| {
                let wall = unix - anchor.duration_since(*at);
                (
                    played,
                    wall.duration_since(UNIX_EPOCH).unwrap().as_secs_f64(),
                )
            });
        assert_replay_timing(arrivals, speed);
    }
}

/// Each message came on time, given the Unix time its frame reached the sink. The core and
/// the fake are timed apart, against the fake's own log, so a slow fake on a busy machine
/// never counts as the core being late, and a slow core never hides behind the fake.
fn assert_replay_timing<'a>(arrivals: impl Iterator<Item = (&'a Played, f64)>, speed: f64) {
    // The original 250 ms budget covers both directions of core delivery together.
    // The fake has a separate 250 ms limit on finishing a scheduled write.
    const LATE: f64 = 0.250;
    // The fake paces itself on one clock and logs on another.
    const CLOCK_SKEW: f64 = 0.001;
    let mut turn = None;
    for (i, (played, at_sink)) in arrivals.enumerate() {
        let sink_after_fake = at_sink - played.at_fake;
        if matches!(played.message, AgentMessage::User { .. }) {
            assert!(
                sink_after_fake <= LATE,
                "the sink got user message {i} {sink_after_fake:.3} s after the fake heard it"
            );
            assert!(
                -sink_after_fake <= LATE,
                "the fake heard user message {i} {:.3} s after the sink got it",
                -sink_after_fake
            );
            turn = Some((played.recorded, played.at_fake, (-sink_after_fake).max(0.0)));
            continue;
        }
        // Start before the write: a pause before the fake logs completion cannot hide a slow core.
        let before_send = played.before_send.unwrap();
        let inbound = (at_sink - before_send).max(0.0);
        let outbound = turn.map_or(0.0, |(_, _, outbound)| outbound);
        assert!(
            outbound + inbound <= LATE,
            "core delivery for message {i} took {outbound:.3} s outbound plus {inbound:.3} s inbound"
        );
        if let Some((user_recorded, heard, _)) = turn {
            let due = (played.recorded - user_recorded) / speed;
            let began = before_send - heard;
            let finished = played.at_fake - heard;
            assert!(
                began + CLOCK_SKEW >= due && finished <= due + LATE,
                "fake write for message {i} began at {began:.3} s and finished at {finished:.3} s, due at {due:.3} s"
            );
        }
    }
}

// The fake immediately schedules a reply due 100 ms after receiving the user's message.
fn check_turn_with_delays(outbound: f64, fake_lateness: f64, inbound: f64) {
    let user = Played {
        recorded: 0.0,
        at_fake: outbound,
        before_send: None,
        message: user("go"),
    };
    let sent = outbound + 0.1 + fake_lateness;
    let reply = Played {
        recorded: 0.1,
        at_fake: sent,
        before_send: Some(sent),
        message: started(),
    };
    assert_replay_timing([(&user, 0.0), (&reply, sent + inbound)].into_iter(), 1.0);
}

#[test]
#[should_panic(expected = "the fake heard user message")]
fn replay_rejects_a_350_ms_outbound_delay_even_when_the_fake_replies_on_time() {
    check_turn_with_delays(0.350, 0.0, 0.0);
}

#[test]
#[should_panic(expected = "core delivery")]
fn replay_rejects_two_core_delays_that_together_exceed_250_ms() {
    check_turn_with_delays(0.150, 0.0, 0.150);
}

#[test]
#[should_panic(expected = "core delivery")]
fn replay_rejects_a_late_arrival_even_if_the_fake_logged_completion_late() {
    let reply = Played {
        recorded: 0.1,
        before_send: Some(0.1),
        at_fake: 0.4,
        message: started(),
    };
    assert_replay_timing([(&reply, 0.5)].into_iter(), 1.0);
}

#[test]
fn replay_charges_a_late_fake_wakeup_to_the_fake() {
    check_turn_with_delays(0.100, 0.200, 0.100);
}

#[test]
fn a_recorded_session_replayed_at_1x_reaches_the_sink_whole_in_order_and_on_time() {
    let replayed = replay(&recording_slice(2.0), 1.0);

    replayed.assert_whole_and_in_order();
    replayed.assert_on_time(1.0);
}

#[test]
fn a_recorded_session_replayed_at_10x_reaches_the_sink_whole_in_order_and_on_time() {
    let replayed = replay(&recording_slice(10.0), 10.0);

    replayed.assert_whole_and_in_order();
    replayed.assert_on_time(10.0);
}

#[test]
fn the_whole_recorded_session_replayed_at_100x_reaches_the_sink_whole_and_in_order() {
    replay(&chat_recording(), 100.0).assert_whole_and_in_order();
}

#[test]
#[ignore = "takes the recording's length divided by 10, over a minute"]
fn the_whole_recorded_session_replayed_at_10x_reaches_the_sink_whole_in_order_and_on_time() {
    let replayed = replay(&chat_recording(), 10.0);

    replayed.assert_whole_and_in_order();
    replayed.assert_on_time(10.0);
}

#[test]
#[ignore = "takes the recording's length, over 10 minutes"]
fn the_whole_recorded_session_replayed_at_1x_reaches_the_sink_whole_in_order_and_on_time() {
    let replayed = replay(&chat_recording(), 1.0);

    replayed.assert_whole_and_in_order();
    replayed.assert_on_time(1.0);
}

/// The `models` line a sidecar sends, and the list the core makes of it.
fn sidecar_models() -> (Value, Vec<ModelInfo>) {
    let line = send(json!({"type": "models", "models": [
        {"id": "default", "name": "Default (recommended)", "description": "Opus 5.5",
         "efforts": ["low", "high", "max"], "default_effort": null, "is_default": true},
        {"id": "haiku", "name": "Haiku 5.5", "description": "Fastest", "efforts": [],
         "default_effort": null, "is_default": false},
    ]}));
    let listed = vec![
        ModelInfo {
            id: "default".into(),
            name: "Default (recommended)".into(),
            description: "Opus 5.5".into(),
            efforts: vec!["low".into(), "high".into(), "max".into()],
            default_effort: None,
            is_default: true,
        },
        ModelInfo {
            id: "haiku".into(),
            name: "Haiku 5.5".into(),
            description: "Fastest".into(),
            efforts: vec![],
            default_effort: None,
            is_default: false,
        },
    ];
    (line, listed)
}

#[test]
fn the_model_list_comes_from_the_sidecar() {
    let (line, listed) = sidecar_models();
    let s = setup(then_wait(vec![line]));
    let session = s.spawn(None);

    assert_eq!(block_on(s.core.models(session).unwrap()).unwrap(), listed);
}

#[test]
fn asking_for_the_models_fails_once_the_sidecar_exits_without_listing_them() {
    let s = setup(json!([]));
    let session = s.spawn(None);

    let models = block_on(s.core.models(session).unwrap());

    assert!(
        matches!(models, Err(CoreError::SessionNotRunning(id)) if id == session),
        "{models:?}"
    );
}

#[test]
fn set_model_reaches_the_sidecar_and_applies_from_the_next_turn_leaving_the_running_one_alone() {
    // The fake fills {{model}} and {{effort}} with what the turn runs with.
    let says_model = send(json!({"type": "text_delta", "text": "{{model}} {{effort}}"}));
    let mut steps = vec![
        json!({"wait_message": "user_message"}),
        send(json!({"type": "turn_start"})),
        approval_request("r1"),
        json!({"wait_message": "answer"}),
        says_model.clone(),
        send(json!({"type": "turn_end"})),
    ];
    steps.extend(turn(&[]));
    steps.insert(steps.len() - 1, says_model);
    let s = setup(then_wait(steps));
    let session = s.spawn(Some("one"));
    wait_requests(&s.core, session, 1);
    let (_, mut frames) = Frames::subscribe(&s.core, session);

    s.core.set_model(session, "haiku", Some("low")).unwrap();
    s.core.answer(session, id("r1"), Reply::AllowOnce).unwrap();
    frames.until_turn_end();
    s.core.send_message(session, "two").unwrap();
    frames.until_turn_end();

    let said: Vec<AgentMessage> = frames
        .messages()
        .into_iter()
        .filter(|m| matches!(m, AgentMessage::TextDelta { .. }))
        .collect();
    assert_eq!(said, [delta("default default"), delta("haiku low")]);
    assert_eq!(
        s.wait_sent("set_model", 1),
        [json!({"type": "set_model", "model": "haiku", "effort": "low"})]
    );
}

#[test]
fn set_model_without_an_effort_leaves_the_effort_out() {
    let s = setup(then_wait(vec![]));
    let session = s.spawn(None);

    s.core.set_model(session, "haiku", None).unwrap();

    assert_eq!(
        s.wait_sent("set_model", 1),
        [json!({"type": "set_model", "model": "haiku"})]
    );
}

fn commands_line(names: &[&str]) -> Value {
    let commands: Vec<Value> = names
        .iter()
        .map(|name| json!({"name": name, "description": format!("Runs {name}"), "argument_hint": "<arg>"}))
        .collect();
    send(json!({"type": "commands", "commands": commands}))
}

fn commands(names: &[&str]) -> AgentMessage {
    AgentMessage::Commands {
        commands: names
            .iter()
            .map(|name| SlashCommand {
                name: (*name).into(),
                description: format!("Runs {name}"),
                argument_hint: "<arg>".into(),
                invocation: format!("/{name}"),
            })
            .collect(),
    }
}

/// The messages of `messages` that are a command list.
fn command_lists(messages: &[AgentMessage]) -> Vec<AgentMessage> {
    messages
        .iter()
        .filter(|m| matches!(m, AgentMessage::Commands { .. }))
        .cloned()
        .collect()
}

#[test]
fn the_command_list_reaches_the_panel_a_later_one_replaces_it_and_a_late_panel_gets_the_latest() {
    let mut steps = turn(&["ok"]);
    steps.insert(2, commands_line(&["review", "init"]));
    steps.push(json!({"wait_message": "user_message"}));
    steps.push(commands_line(&["review"]));
    let s = setup(then_wait(steps));
    let session = s.spawn(None);
    let (history, mut frames) = Frames::subscribe(&s.core, session);
    assert_eq!(history, []);

    s.core.send_message(session, "one").unwrap();
    frames.until_turn_end();

    assert_eq!(
        frames.messages(),
        [
            user("one"),
            started(),
            commands(&["review", "init"]),
            delta("ok"),
            ended()
        ]
    );
    // A panel that attaches later gets the list after the conversation, which never holds it.
    let (late, _) = Frames::subscribe(&s.core, session);
    assert_eq!(
        late,
        [
            user("one"),
            started(),
            delta("ok"),
            ended(),
            commands(&["review", "init"])
        ]
    );

    let (_, mut frames) = Frames::subscribe(&s.core, session);
    s.core.send_message(session, "two").unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while command_lists(&frames.messages()).is_empty() {
        frames.next_frame(deadline, "the second list");
    }
    assert_eq!(command_lists(&frames.messages()), [commands(&["review"])]);
    let (late, _) = Frames::subscribe(&s.core, session);
    assert_eq!(command_lists(&late), [commands(&["review"])]);
    assert_eq!(late.last(), Some(&commands(&["review"])));
}

#[test]
fn a_command_list_alone_sets_no_state_and_starts_no_turn_for_a_waiting_message() {
    let mut s = setup(then_wait(vec![
        json!({"wait_message": "user_message"}),
        commands_line(&["review"]),
    ]));
    let session = s.spawn(Some("go"));
    let (history, mut frames) = Frames::subscribe(&s.core, session);
    let deadline = Instant::now() + TIMEOUT;
    while command_lists(&[history.clone(), frames.messages()].concat()).is_empty() {
        frames.next_frame(deadline, "the list");
    }

    s.no_state_for(Duration::from_millis(100));
    // The message still waits for its turn, so the sidecar's exit makes it not delivered.
    s.core.kill(session).unwrap();
    while !frames.messages().contains(&not_delivered("go")) {
        frames.next_frame(deadline, "the not delivered message");
    }
}

fn usage_line(windows: Value) -> Value {
    json!({"send": {"type": "usage", "reading": "windows", "windows": windows}})
}

fn two_windows() -> Value {
    json!([
        {"name": "5-hour", "percent_used": 42, "resets_at_unix_ms": 1_763_560_921_500_u64},
        {"name": "Weekly", "percent_used": 18, "resets_at_unix_ms": 1_763_992_800_000_u64},
    ])
}

fn two_windows_read() -> PlanLimits {
    PlanLimits::Windows(vec![
        UsageWindow {
            name: "5-hour".into(),
            percent_used: 42,
            resets_at_unix_ms: Some(1_763_560_921_500),
        },
        UsageWindow {
            name: "Weekly".into(),
            percent_used: 18,
            resets_at_unix_ms: Some(1_763_992_800_000),
        },
    ])
}

#[test]
fn a_usage_reading_with_two_windows_arrives_with_percent_and_reset_time() {
    let mut s = setup(json!([
        usage_line(two_windows()),
        {"wait_message": "never"}
    ]));
    let before = now_unix_ms();
    s.spawn(None);

    let reading = s.next_usage_reading().expect("a reading");

    assert_eq!(reading.limits, two_windows_read());
    assert!(!reading.latest_read_failed);
    assert!((before..=now_unix_ms()).contains(&reading.read_at_unix_ms));
    assert_eq!(s.core.usage_reading(Agent::Claude), Some(reading));
    assert_eq!(s.core.usage_reading(Agent::Codex), None);
}

#[test]
fn an_account_without_plan_limits_reads_as_no_plan_limits() {
    let mut s = setup(json!([
        {"send": {"type": "usage", "reading": "no_plan_limits"}},
        {"wait_message": "never"}
    ]));
    s.spawn(None);

    let reading = s.next_usage_reading().expect("a reading");

    assert_eq!(reading.limits, PlanLimits::NoPlanLimits);
    assert!(!reading.latest_read_failed);
}

#[test]
fn a_failed_read_keeps_the_last_good_reading_with_its_time_and_never_fails_a_turn() {
    let mut steps = vec![usage_line(two_windows())];
    steps.extend(turn(&["ok"]));
    steps.push(json!({"send": {"type": "usage", "reading": "failed"}}));
    steps.extend(turn(&["ok again"]));
    let mut s = setup(then_wait(steps));
    let session = s.spawn(Some("go"));
    let good = s.next_usage_reading().expect("a reading");
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);

    let after_failure = s.next_usage_reading().expect("the last good reading");

    assert_eq!(
        after_failure,
        UsageReading {
            latest_read_failed: true,
            ..good
        }
    );
    assert_eq!(s.core.usage_reading(Agent::Claude), Some(after_failure));
    // The session takes its next turn as before.
    s.core.send_message(session, "more").unwrap();
    assert_eq!(s.next_state().1, AgentState::Working);
    assert_eq!(s.next_state().1, AgentState::Done);
}

#[test]
fn a_failed_first_read_gives_no_reading() {
    let mut s = setup(json!([
        {"send": {"type": "usage", "reading": "failed"}},
        {"send": {"type": "account", "account": null}},
        {"wait_message": "never"}
    ]));
    s.spawn(None);

    // The account comes after the failed read, so the read sent nothing before it.
    loop {
        match s.next_event() {
            CoreEvent::UsageReading { reading, .. } => panic!("unexpected {reading:?}"),
            CoreEvent::Account { .. } => break,
            _ => {}
        }
    }
    assert_eq!(s.core.usage_reading(Agent::Claude), None);
}

#[test]
fn an_agent_that_cannot_read_usage_turns_its_reading_off() {
    let mut s = setup(json!([
        usage_line(two_windows()),
        {"send": {"type": "usage", "reading": "unsupported"}},
        {"wait_message": "never"}
    ]));
    s.spawn(None);
    assert!(s.next_usage_reading().is_some());

    assert_eq!(s.next_usage_reading(), None);
    assert_eq!(s.core.usage_reading(Agent::Claude), None);
}

#[test]
fn a_session_reports_its_tokens_and_context_use_after_each_turn() {
    let mut steps = turn(&["ok"]);
    steps.push(send(
        json!({"type": "context", "session_total_tokens": 120_000,
        "context_window": {"used_tokens": 84_000, "size_tokens": 200_000}}),
    ));
    steps.extend(turn(&["ok again"]));
    // The agent could not say how full the window is.
    steps.push(send(
        json!({"type": "context", "session_total_tokens": 150_000, "context_window": null}),
    ));
    let mut s = setup(then_wait(steps));
    let session = s.spawn(Some("go"));
    assert_eq!(s.core.context_reading(session), None);

    let first = ContextReading {
        session_total_tokens: 120_000,
        context_window: Some(ContextWindow {
            used_tokens: 84_000,
            size_tokens: 200_000,
        }),
    };
    assert_eq!(s.next_context_reading(), (session, first.clone()));
    assert_eq!(s.core.context_reading(session), Some(first));

    s.core.send_message(session, "more").unwrap();
    let second = ContextReading {
        session_total_tokens: 150_000,
        context_window: None,
    };
    assert_eq!(s.next_context_reading(), (session, second.clone()));
    assert_eq!(s.core.context_reading(session), Some(second));
}
