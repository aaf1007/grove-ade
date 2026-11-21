use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use grove_core::{
    Core, CoreConfig, CoreError, CoreEvent, OutputFrame, SessionId, Snapshot, SpawnSpec,
};
use tempfile::TempDir;
use tokio::sync::broadcast::{self, error::TryRecvError};
use tokio::sync::mpsc::{self, UnboundedReceiver};

const TIMEOUT: Duration = Duration::from_secs(10);

// Output watermarks a few reads reach.
const HIGH: usize = 16 * 1024;
const LOW: usize = 4 * 1024;
// A read started before the attach has room for the reader's whole buffer, so the first pause
// can come this much past the high watermark.
const READ: usize = 64 * 1024;
// Long enough that a session still being read sends a frame.
const QUIET: Duration = Duration::from_millis(300);
// How long a session read at full speed may take to print 3 MB and exit. Alone it takes under a
// second, and 14 s while a build keeps every core busy. A paused session never exits, so this
// only sets how long a failure takes.
const PRINTED_3_MB: Duration = Duration::from_secs(60);

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

// A core tracking one fresh repo. Returns the repo's main worktree path as the core lists it.
fn core_with_repo(dir: &Path) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf) {
    core_with_repo_config(dir, |_| {})
}

fn small_watermarks(cfg: &mut CoreConfig) {
    cfg.output_high_watermark = HIGH;
    cfg.output_low_watermark = LOW;
}

fn core_with_repo_config(
    dir: &Path,
    configure: impl FnOnce(&mut CoreConfig),
) -> (Core, broadcast::Receiver<CoreEvent>, PathBuf) {
    let repo = dir.join("repo");
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap();
    assert!(status.success());
    let mut cfg = CoreConfig::new(dir.join("home"));
    configure(&mut cfg);
    let (core, events) = Core::start(cfg).unwrap();
    block_on(core.add_repo(&repo)).unwrap();
    let worktree = block_on(core.list_worktrees())[0].path.clone();
    (core, events, worktree)
}

// dash reads no rc file unless $ENV names one, so the user's shell config stays out of tests.
// It has no line editor, so a line typed before it is ready is echoed once. bash's readline
// echoes it again. macOS and Debian both ship /bin/dash.
fn sh(cols: u16, rows: u16) -> SpawnSpec {
    SpawnSpec {
        command: "/bin/dash".into(),
        args: vec![],
        cols,
        rows,
    }
}

// Runs `script` in dash. Output before attach is in the snapshot, not the frames, so a script
// that prints waits for a line first.
fn script(script: &str) -> SpawnSpec {
    SpawnSpec {
        command: "/bin/dash".into(),
        args: vec!["-c".into(), script.into()],
        cols: 80,
        rows: 24,
    }
}

/// Waits for a line on stdin, so a test can attach or resize first. Then writes the PTY size to
/// `<out>.size`, sends `queries`, and writes the replies to `out` as hex. od stops once no byte
/// has come for half a second.
fn query(out: &Path, queries: &str) -> SpawnSpec {
    let script = format!(
        "read go; stty size > '{out}.size'; stty -echo -icanon min 0 time 5; \\
         printf '{queries}'; od -An -v -tx1 > '{out}.tmp'; mv '{out}.tmp' '{out}'",
        out = out.display(),
    );
    SpawnSpec {
        command: "/bin/dash".into(),
        args: vec!["-c".into(), script.into()],
        cols: 80,
        rows: 24,
    }
}

/// The replies `query` wrote, once it finished.
fn replies(out: &Path) -> String {
    assert!(eventually(|| out.exists()), "no replies file");
    let hex = fs::read_to_string(out).unwrap();
    let bytes: Vec<u8> = hex
        .split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect();
    String::from_utf8(bytes).unwrap()
}

fn attach(core: &Core, session: SessionId) -> Output {
    let (sink, rx) = mpsc::unbounded_channel();
    let snapshot = core.attach(session, sink).unwrap();
    Output {
        snapshot,
        rx,
        frames: Vec::new(),
        seen: Vec::new(),
    }
}

// What a session sent to its sink so far, after the snapshot the attach returned.
struct Output {
    snapshot: Snapshot,
    rx: UnboundedReceiver<OutputFrame>,
    // Each frame with when the test got it.
    frames: Vec<(OutputFrame, Instant)>,
    // Every frame's bytes in order.
    seen: Vec<u8>,
}

impl Output {
    // Takes the frames that have come.
    fn take(&mut self) {
        while let Ok(frame) = self.rx.try_recv() {
            self.seen.extend(&frame.bytes);
            self.frames.push((frame, Instant::now()));
        }
    }

    // Waits until `done` holds for the output so far.
    fn wait_until(&mut self, mut done: impl FnMut(&Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.take();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "not in {} bytes of output ending {:?}",
                self.total(),
                self.tail()
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    // Waits until no frame has come for `quiet`.
    fn wait_quiet(&mut self, quiet: Duration) {
        let deadline = Instant::now() + TIMEOUT;
        let mut last = Instant::now();
        loop {
            let before = self.frames.len();
            self.take();
            if self.frames.len() > before {
                last = Instant::now();
            }
            if last.elapsed() >= quiet {
                return;
            }
            assert!(Instant::now() < deadline, "the output never stopped");
            thread::sleep(Duration::from_millis(10));
        }
    }

    // Whether the core dropped the sink, with every frame it sent taken.
    fn closed(&self) -> bool {
        self.rx.is_closed() && self.rx.is_empty()
    }

    fn wait_closed(&mut self) {
        self.wait_until(Self::closed);
    }

    // Acks each frame as it comes, as a panel does, until `done` holds. For output nothing has
    // acked yet.
    fn ack_until(&mut self, core: &Core, session: SessionId, mut done: impl FnMut(&Self) -> bool) {
        let mut acked = 0;
        self.wait_until(|o| {
            core.ack(session, o.total() - acked);
            acked = o.total();
            done(o)
        });
    }

    fn total(&self) -> usize {
        self.seen.len()
    }

    // The last few bytes, as text.
    fn tail(&self) -> String {
        let start = self.total().saturating_sub(40);
        String::from_utf8_lossy(&self.seen[start..]).into_owned()
    }

    // Waits until `found` picks something out of the output after the terminal's echo of the
    // typed line. Nothing before that echo can be the command's output. The prompt can come
    // before the echo or, when the line was typed before the shell was ready, after it.
    fn wait_after<T>(&mut self, typed: &str, found: impl Fn(&str) -> Option<T>) -> T {
        let echo = format!("{typed}\r\n");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.take();
            let text = String::from_utf8_lossy(&self.seen);
            if let Some(start) = text.find(&echo)
                && let Some(value) = found(&text[start + echo.len()..])
            {
                return value;
            }
            assert!(Instant::now() < deadline, "not in the output: {text:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    // Waits until the typed line's output includes `line`, perhaps after a prompt.
    fn wait_for_line(&mut self, typed: &str, line: &str) {
        let line = format!("{line}\r\n");
        self.wait_after(typed, |rest| rest.contains(&line).then_some(()));
    }
}

// Starts `sleep 1000` as a background job of the session's shell.
fn start_job(core: &Core, session: SessionId, output: &mut Output) -> Job {
    let typed = "sleep 1000 & echo job=$!";
    core.write(session, format!("{typed}\r").as_bytes())
        .unwrap();
    let pid = output.wait_after(typed, |rest| {
        // Whole lines only: the last one may still be arriving.
        let (lines, _) = rest.rsplit_once("\r\n")?;
        lines
            .split("\r\n")
            .find_map(|line| line.split_once("job=")?.1.parse().ok())
    });
    Job(pid)
}

// A `sleep` the test started through a shell session. Dropping it kills it if it still runs.
struct Job(u32);

impl Job {
    fn is_running(&self) -> bool {
        // ps lists a killed process with state Z until something reaps it.
        let out = Command::new("ps")
            .args(["-o", "stat=,comm=", "-p", &self.0.to_string()])
            .output()
            .unwrap();
        let ps = String::from_utf8_lossy(&out.stdout);
        let ps = ps.trim();
        !ps.is_empty() && !ps.starts_with('Z') && ps.ends_with("sleep")
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        if self.is_running() {
            let _ = Command::new("kill")
                .args(["-KILL", &self.0.to_string()])
                .status();
        }
    }
}

// Polls `done` until it holds or the test times out.
fn eventually(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
    true
}

// Waits for `session` to exit and returns its exit code. Other events are skipped: the
// worktree's watcher can send `DiffChanged` at any time.
fn wait_exit(events: &mut broadcast::Receiver<CoreEvent>, session: SessionId) -> Option<i32> {
    wait_exit_within(events, session, TIMEOUT)
}

fn wait_exit_within(
    events: &mut broadcast::Receiver<CoreEvent>,
    session: SessionId,
    within: Duration,
) -> Option<i32> {
    let deadline = Instant::now() + within;
    loop {
        match events.try_recv() {
            Ok(CoreEvent::SessionExited { session: s, code }) if s == session => return code,
            Ok(_) => {}
            Err(TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "the session never exited");
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("{e}"),
        }
    }
}

// Whether `session` has exited by now. Other events are skipped.
fn has_exited(events: &mut broadcast::Receiver<CoreEvent>, session: SessionId) -> bool {
    loop {
        match events.try_recv() {
            Ok(CoreEvent::SessionExited { session: s, .. }) if s == session => return true,
            Ok(_) => {}
            Err(TryRecvError::Empty) => return false,
            Err(e) => panic!("{e}"),
        }
    }
}

#[test]
fn a_shell_session_starts_in_its_worktree() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();
    let mut output = attach(&core, session);

    core.write(session, b"pwd\r").unwrap();

    output.wait_for_line("pwd", worktree.to_str().unwrap());
}

#[test]
fn spawning_in_a_folder_grove_does_not_track_fails() {
    let dir = short_temp_dir();
    let (core, _events, _worktree) = core_with_repo(dir.path());
    let folder = dir.path().join("plain");
    fs::create_dir(&folder).unwrap();

    let spawned = core.spawn(&folder, sh(80, 24));

    assert!(
        matches!(&spawned, Err(CoreError::UnknownWorktree(path)) if *path == folder),
        "{spawned:?}"
    );
}

#[test]
fn writing_echo_hi_puts_hi_on_the_attached_sink() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();
    let mut output = attach(&core, session);

    core.write(session, b"echo hi\r").unwrap();

    output.wait_for_line("echo hi", "hi");
}

#[test]
fn a_shell_that_exits_by_itself_sends_its_exit_code_while_a_job_still_holds_the_pty() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = core_with_repo(dir.path());
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();
    let mut output = attach(&core, session);
    // The job keeps the PTY open after the shell exits, so its output never reaches end of file.
    let job = start_job(&core, session, &mut output);

    core.write(session, b"exit 3\r").unwrap();

    assert_eq!(wait_exit(&mut events, session), Some(3));
    assert!(job.is_running());
}

#[test]
fn kill_ends_the_shell_and_its_background_job() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = core_with_repo(dir.path());
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();
    let mut output = attach(&core, session);
    // Job control puts the job in its own process group.
    let job = start_job(&core, session, &mut output);

    core.kill(session).unwrap();

    assert_eq!(wait_exit(&mut events, session), None);
    assert!(eventually(|| !job.is_running()));
}

#[test]
fn after_resize_stty_size_reports_the_new_size() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();
    let mut output = attach(&core, session);
    core.write(session, b"stty size\r").unwrap();
    output.wait_for_line("stty size", "24 80");

    core.resize(session, 100, 40).unwrap();
    core.write(session, b"stty size\r").unwrap();

    output.wait_for_line("stty size", "40 100");
}

#[test]
fn write_returns_at_once_while_the_program_reads_nothing() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let core = Arc::new(core);
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();
    core.write(session, b"sleep 1000\r").unwrap();
    // Once about 1 KB of whole lines waits unread, a write to the PTY blocks.
    let lines = b"aaaaaaa\n".repeat(128 * 1024);

    let (done, written) = std::sync::mpsc::channel();
    thread::spawn({
        let core = core.clone();
        move || done.send(core.write(session, &lines).is_ok())
    });

    assert_eq!(written.recv_timeout(TIMEOUT), Ok(true));
}

#[test]
fn dropping_the_core_ends_its_sessions_and_their_jobs() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();
    let mut output = attach(&core, session);
    let job = start_job(&core, session, &mut output);

    drop(core);

    assert!(eventually(|| !job.is_running()));
}

#[test]
fn a_program_with_no_panel_gets_one_reply_to_each_query() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let out = dir.path().join("replies");
    // DA1, DA2, then the cursor position after moving to row 5, column 10.
    let session = core
        .spawn(&worktree, query(&out, r"\033[c\033[>c\033[5;10H\033[6n"))
        .unwrap();

    core.write(session, b"\r").unwrap();

    assert_eq!(replies(&out), "\x1b[?1;2c\x1b[>0;276;0c\x1b[5;10R");
}

#[test]
fn a_program_with_a_panel_attached_gets_one_reply_to_each_query() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let out = dir.path().join("replies");
    let session = core
        .spawn(&worktree, query(&out, r"\033[c\033[>c\033[5;10H\033[6n"))
        .unwrap();
    let _output = attach(&core, session);

    core.write(session, b"\r").unwrap();

    assert_eq!(replies(&out), "\x1b[?1;2c\x1b[>0;276;0c\x1b[5;10R");
}

#[test]
fn after_resize_the_parser_and_the_pty_have_the_same_size() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let out = dir.path().join("replies");
    // The text area size in cells.
    let session = core.spawn(&worktree, query(&out, r"\033[18t")).unwrap();

    core.resize(session, 100, 40).unwrap();
    core.write(session, b"\r").unwrap();

    assert_eq!(replies(&out), "\x1b[8;40;100t");
    let size = fs::read_to_string(dir.path().join("replies.size")).unwrap();
    assert_eq!(size.trim(), "40 100");
}

#[test]
fn a_zero_size_resize_gives_the_parser_and_the_pty_the_same_size() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let out = dir.path().join("replies");
    let session = core.spawn(&worktree, query(&out, r"\033[18t")).unwrap();

    core.resize(session, 0, 0).unwrap();
    core.write(session, b"\r").unwrap();

    assert_eq!(replies(&out), "\x1b[8;1;2t");
    let size = fs::read_to_string(dir.path().join("replies.size")).unwrap();
    assert_eq!(size.trim(), "1 2");
}

#[test]
fn a_burst_of_small_writes_arrives_as_fewer_frames_a_flush_window_apart() {
    let window = Duration::from_millis(400);
    let dir = short_temp_dir();
    let (core, _events, worktree) =
        core_with_repo_config(dir.path(), |cfg| cfg.output_flush_window = window);
    // The reader gets each write on its own, as it waits through the sleep after it.
    let session = core
        .spawn(
            &worktree,
            script("read _; i=0; while [ $i -lt 20 ]; do printf x; sleep 0.05; i=$((i+1)); done"),
        )
        .unwrap();
    let mut output = attach(&core, session);

    core.write(session, b"\r").unwrap();

    output.wait_until(|o| o.seen.ends_with(&[b'x'; 20]));
    // The terminal echoes the typed line first.
    assert_eq!(output.seen, [b"\r\n".as_slice(), &[b'x'; 20]].concat());
    let seqs: Vec<u64> = output.frames.iter().map(|(f, _)| f.seq).collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
    // The echo and the 20 writes.
    assert!(seqs.len() < 21, "{seqs:?}");
    // Half the window, since the test can get a frame late.
    for pair in output.frames.windows(2) {
        let gap = pair[1].1 - pair[0].1;
        assert!(gap >= window / 2, "frames {seqs:?} came {gap:?} apart");
    }
}

#[test]
fn output_a_flush_window_after_the_last_frame_goes_out_at_once() {
    let window = Duration::from_secs(1);
    let dir = short_temp_dir();
    let (core, _events, worktree) =
        core_with_repo_config(dir.path(), |cfg| cfg.output_flush_window = window);
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();
    let mut output = attach(&core, session);
    core.write(session, b"a").unwrap();
    output.wait_until(|o| o.seen.contains(&b'a'));
    // The prompt can come after the echo.
    output.wait_quiet(window + Duration::from_millis(200));

    let typed = Instant::now();
    core.write(session, b"b").unwrap();

    let before = output.frames.len();
    output.wait_until(|o| o.frames.len() > before);
    let (frame, got) = &output.frames[before];
    assert_eq!(frame.bytes, b"b");
    // Waiting for the window would take all of it.
    let waited = *got - typed;
    assert!(waited < window / 2, "the echo took {waited:?}");
}

#[test]
fn with_no_acks_a_fast_printer_blocks_at_the_high_watermark_until_acks_take_it_below_the_low() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = core_with_repo_config(dir.path(), small_watermarks);
    // No output processing: when a write blocks between the CR and LF that a newline becomes,
    // macOS sends the CR again.
    let session = core
        .spawn(
            &worktree,
            script("read _; stty -opost; yes | head -c 300000"),
        )
        .unwrap();
    let mut output = attach(&core, session);

    core.write(session, b"\r").unwrap();

    output.wait_until(|o| o.total() >= HIGH);
    output.wait_quiet(QUIET);
    let paused = output.total();
    assert!(paused < HIGH + READ, "{paused} bytes unacked");
    // Unread, it would have printed all of it and ended.
    assert!(!has_exited(&mut events, session));

    core.ack(session, paused - LOW);
    output.wait_quiet(QUIET);
    assert_eq!(output.total(), paused, "resumed at the low watermark");

    core.ack(session, 1);
    // It reads until the high watermark again.
    output.wait_until(|o| o.total() > paused);
    output.wait_quiet(QUIET);
    assert_eq!(output.total(), paused + HIGH - (LOW - 1));

    // Acked as it comes, it prints the rest and ends, with nothing lost.
    let mut acked = paused - LOW + 1;
    output.wait_until(|o| {
        core.ack(session, o.total() - acked);
        acked = o.total();
        o.total() >= 2 + 300_000
    });
    assert_eq!(wait_exit(&mut events, session), Some(0));
    let printed = [b"\r\n".as_slice(), &b"y\n".repeat(150_000)].concat();
    // Too long to print.
    assert!(
        output.seen == printed,
        "{} bytes, ending {:?}",
        output.total(),
        output.tail()
    );
}

#[test]
fn a_detached_session_is_read_at_full_speed() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = core_with_repo_config(dir.path(), small_watermarks);
    let session = core
        .spawn(&worktree, script("read _; yes | head -c 3000000"))
        .unwrap();
    let mut output = attach(&core, session);
    core.write(session, b"\r").unwrap();
    output.wait_until(|o| o.total() >= HIGH);

    core.detach(session);

    output.wait_closed();
    assert!(output.total() < HIGH + READ, "{} bytes", output.total());
    // With no acks it printed far more than the high watermark.
    assert_eq!(
        wait_exit_within(&mut events, session, PRINTED_3_MB),
        Some(0)
    );
}

#[test]
fn a_paused_session_whose_panel_goes_without_a_detach_is_read_at_full_speed() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = core_with_repo_config(dir.path(), small_watermarks);
    let session = core
        .spawn(&worktree, script("read _; yes | head -c 3000000"))
        .unwrap();
    let mut output = attach(&core, session);
    core.write(session, b"\r").unwrap();
    output.wait_until(|o| o.total() >= HIGH);
    // Every frame is out, so no send finds the receiver gone.
    output.wait_quiet(QUIET);

    drop(output);

    assert_eq!(
        wait_exit_within(&mut events, session, PRINTED_3_MB),
        Some(0)
    );
}

// macOS hangs up the terminal when the session's process exits, so the job's writes fail there.
#[cfg(target_os = "linux")]
#[test]
fn a_job_still_printing_after_its_session_exits_stays_paced() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = core_with_repo_config(dir.path(), small_watermarks);
    // The shell exits at once. The job ignores the hangup that brings, and keeps printing. The
    // shell sets the trap, so the job has it before the shell can exit.
    let session = core
        .spawn(
            &worktree,
            script("read _; stty -opost; trap '' HUP; yes | head -c 300000 & exit 0"),
        )
        .unwrap();
    let mut output = attach(&core, session);

    core.write(session, b"\r").unwrap();

    assert_eq!(wait_exit(&mut events, session), Some(0));
    output.wait_until(|o| o.total() >= HIGH);
    output.wait_quiet(QUIET);
    assert!(output.total() < HIGH + READ, "{} bytes", output.total());
    // Acks still reach it, so it prints the rest, and its reader stops, which drops the sink.
    output.ack_until(&core, session, Output::closed);
    assert_eq!(output.total(), 2 + 300_000);
}

#[test]
fn killing_a_paused_session_ends_it_and_reports_its_exit() {
    let dir = short_temp_dir();
    let (core, mut events, worktree) = core_with_repo_config(dir.path(), small_watermarks);
    let session = core.spawn(&worktree, script("read _; exec yes")).unwrap();
    let mut output = attach(&core, session);
    core.write(session, b"\r").unwrap();
    output.wait_until(|o| o.total() >= HIGH);

    core.kill(session).unwrap();

    assert_eq!(wait_exit(&mut events, session), None);
    // Acks still reach it, so its reader reads to the end and stops, which drops the sink.
    output.ack_until(&core, session, Output::closed);
    // Its output has ended, so these do nothing.
    core.ack(session, 1);
    core.detach(session);
}

#[test]
fn bytes_reach_the_sink_unchanged() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    // Invalid UTF-8, then a euro sign cut after its second byte. Its last byte waits for a line,
    // so a later read gets it.
    let session = core
        .spawn(
            &worktree,
            script(r"read _; stty -echo; printf 'a\377\376\342\202'; read _; printf '\254b'"),
        )
        .unwrap();
    let mut output = attach(&core, session);
    core.write(session, b"\r").unwrap();
    output.wait_until(|o| o.seen.ends_with(b"\xe2\x82"));

    core.write(session, b"\r").unwrap();

    output.wait_until(|o| o.seen.ends_with(b"b"));
    // The terminal echoes the first line.
    assert_eq!(output.seen, b"\r\na\xff\xfe\xe2\x82\xacb");
}

#[test]
fn a_new_attach_replaces_the_sink_starts_at_zero_unacked_and_numbers_frames_on() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo_config(dir.path(), small_watermarks);
    let session = core.spawn(&worktree, script("read _; exec yes")).unwrap();
    let mut first = attach(&core, session);
    core.write(session, b"\r").unwrap();
    first.wait_until(|o| o.total() >= HIGH);
    first.wait_quiet(QUIET);
    let (last, _) = first.frames.last().unwrap();
    let last = last.seq;

    let mut second = attach(&core, session);

    first.wait_closed();
    second.wait_until(|o| o.total() >= HIGH);
    second.wait_quiet(QUIET);
    // The reader was paused when the new sink came, so it read no more than that from zero.
    assert_eq!(second.total(), HIGH);
    let seqs: Vec<u64> = second.frames.iter().map(|(f, _)| f.seq).collect();
    assert_eq!(
        seqs,
        (last + 1..last + 1 + seqs.len() as u64).collect::<Vec<_>>()
    );
}

#[test]
fn detaching_and_attaching_again_many_times_keeps_working() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo(dir.path());
    let session = core.spawn(&worktree, sh(80, 24)).unwrap();

    for i in 0..100 {
        let mut output = attach(&core, session);
        let snapshot = String::from_utf8_lossy(&output.snapshot.bytes).into_owned();
        if i > 0 {
            // The last attach's output is on the screen.
            assert!(
                snapshot.contains(&format!("attach{}", i - 1)),
                "{snapshot:?}"
            );
        }
        let typed = format!("echo attach{i}");
        core.write(session, format!("{typed}\r").as_bytes())
            .unwrap();
        output.wait_for_line(&typed, &format!("attach{i}"));
        assert_eq!(output.frames[0].0.seq, output.snapshot.seq + 1);
        core.detach(session);
    }
}

#[test]
fn an_ack_for_more_than_is_unacked_counts_down_to_zero() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo_config(dir.path(), small_watermarks);
    let session = core.spawn(&worktree, script("read _; exec yes")).unwrap();
    let mut output = attach(&core, session);
    core.write(session, b"\r").unwrap();
    output.wait_until(|o| o.total() >= HIGH);
    output.wait_quiet(QUIET);
    let paused = output.total();

    core.ack(session, usize::MAX);

    // From zero it reads up to the high watermark again, and no further.
    output.wait_until(|o| o.total() > paused);
    output.wait_quiet(QUIET);
    assert_eq!(output.total(), paused + HIGH);
}

#[test]
fn zero_watermarks_still_let_acked_output_through() {
    let dir = short_temp_dir();
    let (core, _events, worktree) = core_with_repo_config(dir.path(), |cfg| {
        cfg.output_high_watermark = 0;
        cfg.output_low_watermark = 0;
    });
    // It keeps running, so only pacing decides what comes through.
    let session = core
        .spawn(&worktree, script("read _; printf hello; read _"))
        .unwrap();
    let mut output = attach(&core, session);

    core.write(session, b"\r").unwrap();

    output.ack_until(&core, session, |o| o.seen.ends_with(b"hello"));
}
