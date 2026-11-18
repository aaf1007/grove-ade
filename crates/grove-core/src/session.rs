use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::future;
use std::io::{self, ErrorKind, Read, Write};
use std::mem;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{Notify, broadcast, oneshot};
use tokio::time;

use crate::terminal::{MIN_COLS, MIN_ROWS, Terminal};
use crate::{Core, CoreConfig, CoreError, CoreEvent, Result, Snapshot};

/// A session the core started. Ids are never reused while the core runs, and a session a
/// restart brings back keeps its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub(crate) u64);

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Ties an agent session's hooks and MCP servers to it.
pub(crate) const GROVE_SESSION_ID: &str = "GROVE_SESSION_ID";
/// The secret of an agent session's running process, which its panel requests carry with its
/// GROVE_SESSION_ID.
pub(crate) const GROVE_SESSION_TOKEN: &str = "GROVE_SESSION_TOKEN";
/// Where grove-hook finds the core socket.
pub(crate) const GROVE_SOCKET: &str = "GROVE_SOCKET";

/// What a new session runs, and its terminal size in cells.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub command: PathBuf,
    pub args: Vec<OsString>,
    pub cols: u16,
    pub rows: u16,
}

/// A batch of an attached session's output. A session numbers its frames from 1 in the order
/// they go out, with no gaps, across all its attaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFrame {
    pub seq: u64,
    pub bytes: Vec<u8>,
}

/// Receives an attached session's output, frame by frame. Its panel acks the bytes it has drawn.
pub type OutputSink = UnboundedSender<OutputFrame>;

type Live = Mutex<HashMap<SessionId, Session>>;
type Outputs = Mutex<HashMap<SessionId, Arc<Output>>>;
type Kept = Mutex<HashMap<SessionId, KeptSession>>;

/// How a session's process ended, for the agent state.
pub(crate) struct ProcessExit {
    pub session: SessionId,
    /// None when a signal ended it.
    pub code: Option<i32>,
    /// Grove's `kill` ended it.
    pub killed: bool,
}

/// When a session last had input and output, for the status heuristics.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Activity {
    /// The last output the reader parsed.
    pub output: Option<Instant>,
    /// The last `Core::write`. Replies to terminal queries do not count.
    pub input: Option<Instant>,
    /// The first output after that write.
    pub reply: Option<Instant>,
}

/// What the status heuristics see of a session at one moment.
pub(crate) struct Observed {
    pub activity: Activity,
    /// The session's own process holds the terminal, not a program it started, so the screen
    /// is its own.
    pub in_front: bool,
}

/// A handle on the live sessions that a core task can keep.
#[derive(Clone)]
pub(crate) struct Probe(Arc<Live>);

impl Probe {
    pub(crate) fn observe(&self, session: SessionId) -> Option<Observed> {
        let live = lock(&self.0);
        let s = live.get(&session)?;
        let pty = s.pty.as_ref()?;
        Some(Observed {
            activity: *lock(&pty.activity),
            in_front: pty.master.process_group_leader() == Some(s.leader),
        })
    }

    /// The session's screen as its headless parser has it, one line per row, and the time of
    /// the last output in it.
    pub(crate) fn screen(&self, session: SessionId) -> Option<(String, Option<Instant>)> {
        let live = lock(&self.0);
        let pty = live.get(&session)?.pty.as_ref()?;
        let terminal = lock(&pty.terminal);
        Some((terminal.screen_text(), lock(&pty.activity).output))
    }

    /// Every running shell and run session, with its worktree.
    pub(crate) fn shells_and_runs(&self) -> Vec<(SessionId, PathBuf, Kind)> {
        lock(&self.0)
            .iter()
            .filter(|(_, s)| matches!(s.kind, Kind::Shell | Kind::Run { .. }))
            .map(|(&id, s)| (id, s.worktree.clone(), s.kind.clone()))
            .collect()
    }
}

pub(crate) struct Sessions {
    next_id: AtomicU64,
    // A session's wait thread removes it when its process exits.
    live: Arc<Live>,
    // A session's reader removes its output when it stops. On Linux a process the session
    // started can keep printing after the session's own process exits, and its panel still
    // acks and detaches it.
    outputs: Arc<Outputs>,
    // Setup and run sessions. Each one's exit code goes to a waiter, and its output stays
    // attachable after it exits, until `release`.
    kept: Arc<Kept>,
    exits: UnboundedSender<ProcessExit>,
    pacing: Pacing,
}

struct KeptSession {
    // Taken when the process exits.
    on_exit: Option<oneshot::Sender<Option<i32>>>,
    // Its terminal and output, from when the process exits.
    ended: Option<(Arc<Mutex<Terminal>>, Arc<Output>)>,
    // Its process, which leads a process session of its own. Kept after it exits only while
    // something it left still runs in that session, which keeps the kernel from handing the
    // pid out again.
    leader: Option<libc::pid_t>,
    // Told once the reader has parsed the last of its output.
    on_output_end: Option<oneshot::Sender<()>>,
}

/// When an attached session's output goes out and when its reader pauses, from the core's
/// config.
#[derive(Clone, Copy)]
struct Pacing {
    flush_window: Duration,
    high_watermark: usize,
    low_watermark: usize,
}

struct Session {
    // The session's process. It leads its own session (setsid), so its pid is also the
    // session id of everything it starts.
    leader: libc::pid_t,
    worktree: PathBuf,
    kind: Kind,
    // To the writer thread, which writes to the PTY or the process's stdin.
    input: mpsc::Sender<Vec<u8>>,
    // None for a session on pipes, such as a chat-mode agent session.
    pty: Option<Pty>,
    killed: bool,
}

struct Pty {
    master: Box<dyn MasterPty + Send>,
    terminal: Arc<Mutex<Terminal>>,
    output: Arc<Output>,
    activity: Arc<Mutex<Activity>>,
}

/// What a session runs, so a restart can bring it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A command the caller chose, which a restart does not bring back.
    Command,
    Shell,
    /// `port` is where it declared it serves, for the port router.
    Run {
        name: String,
        port: Option<u16>,
    },
    /// An agent session. `Agents` keeps what a restart needs for it.
    Agent,
}

impl Session {
    fn pty(&self, session: SessionId) -> Result<&Pty> {
        self.pty.as_ref().ok_or(CoreError::NoTerminal(session))
    }
}

/// A session's output on its way to the attached sink. The reader thread fills the batch and
/// the flush task sends it.
#[derive(Default)]
struct Output {
    state: Mutex<OutputState>,
    // Wakes the flush task when output comes for an empty batch.
    arrived: Notify,
    // Wakes a paused reader.
    acked: Condvar,
}

#[derive(Default)]
struct OutputState {
    sink: Option<OutputSink>,
    // Read for the sink and not in a frame yet.
    batch: Vec<u8>,
    // Read for the sink and not acked yet, the batch included.
    unacked: usize,
    // The last frame's number.
    seq: u64,
    // The reader stopped, so the flush task sends the last frame and ends.
    closed: bool,
}

impl OutputState {
    // Whether acks decide when the reader reads.
    fn paced(&self) -> bool {
        self.sink.is_some()
    }

    fn set_sink(&mut self, sink: Option<OutputSink>) {
        self.sink = sink;
        // Both were for the earlier sink.
        self.batch.clear();
        self.unacked = 0;
    }
}

/// What a flush did.
pub(crate) enum Flushed {
    Frame,
    Nothing,
    // The reader stopped and nothing is left to send.
    Closed,
}

impl Core {
    /// Starts `spec` in a new PTY, in a tracked worktree. A restart does not bring it back.
    pub fn spawn(&self, worktree: &Path, spec: SpawnSpec) -> Result<SessionId> {
        self.check_tracked(worktree)?;
        self.start_session(self.sessions.new_id(), worktree, spec, &[], Kind::Command)
    }

    /// The shell session of a tracked worktree: the one running there, at the size it has, or
    /// the user's login shell started now at `cols` by `rows`. A restart brings it back as a
    /// fresh shell under the same id.
    pub fn spawn_shell(&self, worktree: &Path, cols: u16, rows: u16) -> Result<SessionId> {
        self.check_tracked(worktree)?;
        let _opening = lock(&self.opening_shell);
        let mut running = self.sessions.probe().shells_and_runs().into_iter();
        if let Some((session, ..)) = running.find(|(_, w, k)| *k == Kind::Shell && w == worktree) {
            return Ok(session);
        }
        let session = self.start_shell(self.sessions.new_id(), worktree, cols, rows)?;
        self.saves.request();
        Ok(session)
    }

    pub(crate) fn start_shell(
        &self,
        id: SessionId,
        worktree: &Path,
        cols: u16,
        rows: u16,
    ) -> Result<SessionId> {
        // A login shell, as a terminal app starts one.
        let spec = SpawnSpec {
            command: self.run_shell.clone(),
            args: vec!["-l".into()],
            cols,
            rows,
        };
        self.start_session(id, worktree, spec, &[], Kind::Shell)
    }

    /// Starts `spec` as session `id`, with the session environment plus `env`. Only agent
    /// sessions pass `env`. Nothing from grove's own environment gets through.
    pub(crate) fn start_session(
        &self,
        id: SessionId,
        worktree: &Path,
        spec: SpawnSpec,
        env: &[(&str, OsString)],
        kind: Kind,
    ) -> Result<SessionId> {
        let pair = native_pty_system()
            .openpty(size(spec.cols, spec.rows))
            .map_err(CoreError::pty("open a PTY"))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(CoreError::pty("read the PTY"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(CoreError::pty("write to the PTY"))?;

        let output = Arc::new(Output::default());
        let activity = Arc::default();
        let (input, input_rx) = mpsc::channel();
        // Replies to terminal queries go to the writer thread, in order with other input.
        let terminal = Arc::new(Mutex::new(Terminal::new(
            spec.cols,
            spec.rows,
            input.clone(),
        )));
        let pacing = self.sessions.pacing;
        let outputs = self.sessions.outputs.clone();
        lock(&outputs).insert(id, output.clone());
        let reader_started = start_thread("grove-pty-reader", {
            let terminal = terminal.clone();
            let output = output.clone();
            let activity = Arc::clone(&activity);
            let outputs = outputs.clone();
            let kept = self.sessions.kept.clone();
            move || {
                read_output(reader, &terminal, &output, pacing, &activity);
                output.close();
                // After a mode switch a new process may hold the id, with its own output.
                let mut outputs = lock(&outputs);
                if outputs.get(&id).is_some_and(|o| Arc::ptr_eq(o, &output)) {
                    outputs.remove(&id);
                }
                drop(outputs);
                let on_output_end = lock(&kept)
                    .get_mut(&id)
                    .and_then(|kept| kept.on_output_end.take());
                if let Some(on_output_end) = on_output_end {
                    // The waiter may be gone, as when the core stops.
                    let _ = on_output_end.send(());
                }
            }
        });
        if reader_started.is_err() {
            lock(&outputs).remove(&id);
        }
        reader_started?;
        // Started after the reader, which ends it.
        self.runtime
            .spawn(flush_batches(output.clone(), pacing.flush_window));
        start_thread("grove-pty-writer", move || write_input(writer, input_rx))?;
        // The wait thread gets the child once it runs, so nothing can fail after it starts.
        let (child_tx, child_rx) = mpsc::channel::<Box<dyn Child + Send + Sync>>();
        start_thread("grove-pty-wait", {
            let live = self.sessions.live.clone();
            let kept = self.sessions.kept.clone();
            let events = self.events.clone();
            let exits = self.sessions.exits.clone();
            move || {
                if let Ok(mut child) = child_rx.recv() {
                    // Exit comes from here, never from the reader: a process the session
                    // started can keep the PTY open after the session's own process exits.
                    let code = child
                        .wait()
                        .ok()
                        .filter(|status| status.signal().is_none())
                        .and_then(|status| i32::try_from(status.exit_code()).ok());
                    report_exit(id, code, &live, &kept, &events, &exits);
                }
            }
        })?;

        let mut cmd = CommandBuilder::new(&spec.command);
        cmd.args(&spec.args);
        cmd.cwd(worktree);
        cmd.env_clear();
        for (key, value) in &self.session_env {
            cmd.env(key, value);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        // Held from the check to the insert, so a removal either lists this session or refuses it.
        let mut live = self.sessions.live();
        self.check_startable(worktree)?;
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(CoreError::pty("start the session's command"))?;
        // std's Child always has an id, and it fits.
        let leader = child
            .process_id()
            .and_then(|pid| libc::pid_t::try_from(pid).ok())
            .unwrap_or_default();
        // Under the live lock, so before its exit can be reported.
        if let Some(kept) = lock(&self.sessions.kept).get_mut(&id) {
            kept.leader = Some(leader);
        }
        live.insert(
            id,
            Session {
                leader,
                worktree: worktree.to_path_buf(),
                kind,
                input,
                pty: Some(Pty {
                    master: pair.master,
                    terminal,
                    output,
                    activity,
                }),
                killed: false,
            },
        );
        // Only fails if the wait thread is gone, and it waits for this.
        let _ = child_tx.send(child);
        Ok(id)
    }

    /// Starts `cmd` as session `id` with no terminal, its stdin and stdout on pipes, and returns
    /// its stdout. What `input` queued, before or after, goes to its stdin. It gets the session
    /// environment plus `env`, as `start_session` gives. `before_exit` runs after the process
    /// exits and before the exit is reported.
    pub(crate) fn start_piped(
        &self,
        id: SessionId,
        worktree: &Path,
        mut cmd: Command,
        env: &[(&str, OsString)],
        (input, input_rx): (mpsc::Sender<Vec<u8>>, mpsc::Receiver<Vec<u8>>),
        before_exit: impl FnOnce() + Send + 'static,
    ) -> Result<ChildStdout> {
        let (stdin_tx, stdin_rx) = mpsc::channel::<ChildStdin>();
        start_thread("grove-pipe-writer", move || {
            if let Ok(stdin) = stdin_rx.recv() {
                write_input(stdin, input_rx);
            }
        })?;
        // As in `start_session`, nothing can fail once the wait thread has the child.
        let (child_tx, child_rx) = mpsc::channel::<std::process::Child>();
        start_thread("grove-pipe-wait", {
            let live = self.sessions.live.clone();
            let kept = self.sessions.kept.clone();
            let events = self.events.clone();
            let exits = self.sessions.exits.clone();
            move || {
                if let Ok(mut child) = child_rx.recv() {
                    let code = child.wait().ok().and_then(|status| status.code());
                    before_exit();
                    report_exit(id, code, &live, &kept, &events, &exits);
                }
            }
        })?;

        cmd.current_dir(worktree)
            .env_clear()
            .envs(&self.session_env)
            .envs(env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // Its own session, as a PTY session's process leads one, so `kill` ends everything it
        // started.
        lead_own_session(&mut cmd);
        let program = PathBuf::from(cmd.get_program());
        // Held from the check to the insert, so a removal either lists this session or refuses it.
        let mut live = self.sessions.live();
        self.check_startable(worktree)?;
        let mut child = cmd
            .spawn()
            .map_err(CoreError::io("start the session's command", &program))?;
        // Both are piped, so std always sets both.
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CoreError::io("pipe the stdio of", &program)(
                io::ErrorKind::BrokenPipe.into(),
            ));
        };
        live.insert(
            id,
            Session {
                // std's Child id always fits.
                leader: libc::pid_t::try_from(child.id()).unwrap_or_default(),
                worktree: worktree.to_path_buf(),
                kind: Kind::Agent,
                input,
                pty: None,
                killed: false,
            },
        );
        // Neither fails: both threads wait for these.
        let _ = stdin_tx.send(stdin);
        let _ = child_tx.send(child);
        Ok(stdout)
    }

    /// A sender to the session's writer thread.
    pub(crate) fn input(&self, session: SessionId) -> Result<mpsc::Sender<Vec<u8>>> {
        let live = self.sessions.live();
        let s = live
            .get(&session)
            .ok_or(CoreError::SessionNotRunning(session))?;
        Ok(s.input.clone())
    }

    /// Queues `bytes` for the session's writer thread, so a program that is not reading never
    /// blocks the caller.
    pub fn write(&self, session: SessionId, bytes: &[u8]) -> Result<()> {
        let live = self.sessions.live();
        let s = live
            .get(&session)
            .ok_or(CoreError::SessionNotRunning(session))?;
        let mut activity = lock(&s.pty(session)?.activity);
        // The writer thread stops only when the PTY is gone.
        s.input
            .send(bytes.to_vec())
            .map_err(|_| CoreError::SessionNotRunning(session))?;
        activity.input = Some(Instant::now());
        activity.reply = None;
        Ok(())
    }

    /// Resizes the session's PTY, which sends SIGWINCH to its foreground process, and its
    /// headless parser with it.
    pub fn resize(&self, session: SessionId, cols: u16, rows: u16) -> Result<()> {
        let live = self.sessions.live();
        let s = live
            .get(&session)
            .ok_or(CoreError::SessionNotRunning(session))?;
        let pty = s.pty(session)?;
        // Holding the parser's lock means output drawn for the new size is parsed at it.
        let mut terminal = lock(&pty.terminal);
        pty.master
            .resize(size(cols, rows))
            .map_err(CoreError::pty("resize the PTY"))?;
        terminal.resize(cols, rows);
        Ok(())
    }

    /// The port a running run session declared. None for any other session, and once it exits.
    pub fn port(&self, session: SessionId) -> Option<u16> {
        match self.sessions.live().get(&session)?.kind {
            Kind::Run { port, .. } => port,
            _ => None,
        }
    }

    /// Whether the session's program has bracketed paste on, which a review batch needs in terminal
    /// mode. Fails with `NoTerminal` for an agent session in chat mode, and `SessionNotRunning` for a
    /// session that is not running.
    pub fn bracketed_paste(&self, session: SessionId) -> Result<bool> {
        self.read_terminal(session, Terminal::bracketed_paste)
    }

    /// Runs `read` on the session's parser.
    fn read_terminal<T>(&self, session: SessionId, read: impl FnOnce(&Terminal) -> T) -> Result<T> {
        let live = self.sessions.live();
        let s = live
            .get(&session)
            .ok_or(CoreError::SessionNotRunning(session))?;
        Ok(read(&lock(&s.pty(session)?.terminal)))
    }

    /// Ends the session's process and every process it started that is still in its session,
    /// whatever their process group. `SessionExited` follows from the wait thread. An agent
    /// session ends as `Exited`, not `Failed`.
    pub fn kill(&self, session: SessionId) -> Result<()> {
        let leader = {
            let mut live = self.sessions.live();
            let s = live
                .get_mut(&session)
                .ok_or(CoreError::SessionNotRunning(session))?;
            s.killed = true;
            s.leader
        };
        // Listing every process reads /proc on Linux, so it stays off the caller's thread.
        self.runtime.spawn_blocking(move || kill_session(leader));
        Ok(())
    }

    /// Sends the session's output to `sink` in frames from now on, in place of any earlier sink,
    /// with no bytes unacked, and returns the session's terminal up to the first of them. The
    /// session's reader pauses while too many bytes are unacked. A setup session stays
    /// attachable after it exits, until its worktree is removed. Attaching then returns its
    /// final screen, and no frames follow.
    pub fn attach(&self, session: SessionId, sink: OutputSink) -> Result<Snapshot> {
        let running = {
            let live = self.sessions.live();
            match live.get(&session) {
                Some(s) => {
                    let pty = s.pty(session)?;
                    Some((pty.terminal.clone(), pty.output.clone()))
                }
                None => None,
            }
        };
        // A kept session that exited returns its final screen, and no frames follow.
        let (terminal, output) = running
            .or_else(|| self.sessions.ended(session))
            .ok_or(CoreError::SessionNotRunning(session))?;
        // The reader parses each chunk and adds it to the batch under the parser's lock. Held
        // until the new sink is in place, it puts each chunk in the snapshot or in a frame to
        // the new sink, never both or neither.
        let mut terminal = lock(&terminal);
        let bytes = terminal.snapshot();
        let (cols, rows) = terminal.size();
        let seq = output.set_sink(Some(sink));
        Ok(Snapshot {
            seq,
            cols,
            rows,
            bytes,
        })
    }

    /// Stops sending the session's output to its sink, so the session is read at full speed and
    /// feeds only its headless parser. Works until the session's output ends, which can be after
    /// its process exits, and does nothing after.
    pub fn detach(&self, session: SessionId) {
        if let Some(output) = self.sessions.output(session) {
            output.set_sink(None);
        }
    }

    /// Counts `bytes` of the session's output as drawn by its panel. A paused reader resumes once
    /// the unacked bytes are below the low watermark. Works until the session's output ends,
    /// which can be after its process exits, and does nothing after.
    pub fn ack(&self, session: SessionId, bytes: usize) {
        if let Some(output) = self.sessions.output(session) {
            output.ack(bytes);
        }
    }
}

impl Sessions {
    /// The receiver gets every session's exit.
    pub(crate) fn new(cfg: &CoreConfig) -> (Self, UnboundedReceiver<ProcessExit>) {
        let (exits, rx) = unbounded_channel();
        // At 0 a paused reader could never resume. A low watermark above the high one could
        // resume it with no room to read.
        let high_watermark = cfg.output_high_watermark.max(1);
        let sessions = Self {
            next_id: AtomicU64::default(),
            live: Arc::default(),
            outputs: Arc::default(),
            kept: Arc::default(),
            exits,
            pacing: Pacing {
                flush_window: cfg.output_flush_window,
                high_watermark,
                low_watermark: cfg.output_low_watermark.clamp(1, high_watermark),
            },
        };
        (sessions, rx)
    }

    pub(crate) fn new_id(&self) -> SessionId {
        SessionId(self.next_id.fetch_add(1, Ordering::Relaxed))
    }

    /// Makes `new_id` hand out only ids above `session`, one a restart brings back.
    pub(crate) fn reserve(&self, session: SessionId) {
        self.next_id.fetch_max(session.0 + 1, Ordering::Relaxed);
    }

    /// Keeps session `id`, which is still to start: the returned receiver gets its exit code,
    /// None for a signal, and its output stays attachable after it exits, until `release`.
    pub(crate) fn keep(&self, id: SessionId) -> oneshot::Receiver<Option<i32>> {
        let (on_exit, exited) = oneshot::channel();
        let kept = KeptSession {
            on_exit: Some(on_exit),
            ended: None,
            leader: None,
            on_output_end: None,
        };
        lock(&self.kept).insert(id, kept);
        exited
    }

    /// Stops keeping session `id`, so the core drops its output once it has exited.
    pub(crate) fn release(&self, id: SessionId) {
        lock(&self.kept).remove(&id);
    }

    /// Stops keeping session `id`, as `release` does, and returns the process session to kill
    /// for it, whether its own process still runs or not. None when nothing of it can be left.
    ///
    /// One case still gets it wrong: what it left all exits on its own, the kernel hands the pid
    /// to a new process that starts a session of its own, and that process exits while others
    /// stay in its session. Killing by the pid then kills those.
    pub(crate) fn release_with_leader(&self, id: SessionId) -> Option<libc::pid_t> {
        let kept = lock(&self.kept).remove(&id)?;
        let leader = kept.leader?;
        // Once its process has exited and been reaped, a process with its pid is another's.
        let exited = kept.on_exit.is_none();
        (!(exited && process_exists(leader))).then_some(leader)
    }

    /// For kept session `id`, which is still to start: the receiver hears once its reader has
    /// parsed the last of its output, at the end of the terminal. macOS ends the terminal a few
    /// milliseconds after the process exits, and what it left there writes nowhere from then
    /// on. Linux keeps the terminal open until every process holding it has exited. The sender
    /// drops unheard if the session is released first.
    pub(crate) fn output_ended(&self, id: SessionId) -> oneshot::Receiver<()> {
        let (on_output_end, ended) = oneshot::channel();
        if let Some(kept) = lock(&self.kept).get_mut(&id) {
            kept.on_output_end = Some(on_output_end);
        }
        ended
    }

    #[cfg(test)]
    pub(crate) fn kept_count(&self) -> usize {
        lock(&self.kept).len()
    }

    /// The session's headless parser: a running session's, or else the final one of a kept
    /// session that exited.
    pub(crate) fn terminal(&self, session: SessionId) -> Option<Arc<Mutex<Terminal>>> {
        if let Some(s) = self.live().get(&session) {
            return s.pty.as_ref().map(|pty| pty.terminal.clone());
        }
        // An exit stores the kept output under the live lock, so a session gone from there
        // has it by now.
        self.ended(session).map(|(terminal, _)| terminal)
    }

    fn ended(&self, session: SessionId) -> Option<(Arc<Mutex<Terminal>>, Arc<Output>)> {
        lock(&self.kept).get(&session)?.ended.clone()
    }

    /// How long output waits after a frame, for chat-mode messages too.
    pub(crate) fn flush_window(&self) -> Duration {
        self.pacing.flush_window
    }

    pub(crate) fn probe(&self) -> Probe {
        Probe(self.live.clone())
    }

    /// The running sessions in any of `worktrees`.
    pub(crate) fn in_worktrees(&self, worktrees: &[PathBuf]) -> Vec<SessionId> {
        self.live()
            .iter()
            .filter(|(_, s)| worktrees.contains(&s.worktree))
            .map(|(&id, _)| id)
            .collect()
    }

    /// Stops keeping a session, as `release` does, for a task that cannot borrow `self`.
    pub(crate) fn releaser(&self) -> impl Fn(SessionId) + Send + 'static {
        let kept = self.kept.clone();
        move |session| {
            lock(&kept).remove(&session);
        }
    }

    /// Whether a session still runs, for a task that cannot borrow `self`.
    pub(crate) fn is_running(&self) -> impl Fn(SessionId) -> bool + Send + 'static {
        let live = self.live.clone();
        move |session| lock(&live).contains_key(&session)
    }

    fn live(&self) -> MutexGuard<'_, HashMap<SessionId, Session>> {
        lock(&self.live)
    }

    fn output(&self, session: SessionId) -> Option<Arc<Output>> {
        lock(&self.outputs).get(&session).cloned()
    }
}

// A stopped core ends every session it still runs. Closing the PTYs is not enough: the hangup
// reaches the shell, and its background jobs keep running.
impl Drop for Sessions {
    fn drop(&mut self) {
        for session in self.live().values() {
            kill_session(session.leader);
        }
        // No panel acks anymore, so no reader may wait for one.
        for output in lock(&self.outputs).values() {
            output.set_sink(None);
        }
    }
}

// The PTY gets the same smallest size as the headless parser, so both report one size.
fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(MIN_ROWS),
        cols: cols.max(MIN_COLS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn start_thread(name: &str, f: impl FnOnce() + Send + 'static) -> Result<()> {
    thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map(drop)
        .map_err(CoreError::pty("start a session thread"))
}

impl Output {
    /// Sends the output from now on to `sink`, in place of any earlier sink. None detaches.
    /// Returns the last frame's number, so the next frame is the one after it.
    fn set_sink(&self, sink: Option<OutputSink>) -> u64 {
        let seq = {
            let mut s = lock(&self.state);
            s.set_sink(sink);
            s.seq
        };
        self.acked.notify_one();
        // The flush task watches this sink's receiver instead.
        self.arrived.notify_one();
        seq
    }

    /// A sender of the sink, for the flush task to watch its receiver.
    fn sink(&self) -> Option<OutputSink> {
        lock(&self.state).sink.clone()
    }

    /// Detaches the sink if its receiver is gone.
    fn detach_if_closed(&self) {
        let mut s = lock(&self.state);
        if s.sink.as_ref().is_some_and(UnboundedSender::is_closed) {
            s.set_sink(None);
            drop(s);
            self.acked.notify_one();
        }
    }

    fn ack(&self, bytes: usize) {
        let mut s = lock(&self.state);
        s.unacked = s.unacked.saturating_sub(bytes);
        drop(s);
        self.acked.notify_one();
    }

    /// Waits while the sink is too far behind, then returns how much the reader may read: up to
    /// the high watermark while acks pace it, `buf` otherwise.
    fn room(&self, pacing: Pacing, buf: usize) -> usize {
        let mut s = lock(&self.state);
        if s.paced() && s.unacked >= pacing.high_watermark {
            s = self
                .acked
                .wait_while(s, |s| s.paced() && s.unacked >= pacing.low_watermark)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if s.paced() {
            (pacing.high_watermark - s.unacked).min(buf)
        } else {
            buf
        }
    }

    /// Adds `bytes` to the batch, if a sink is attached.
    fn push(&self, bytes: &[u8]) {
        let mut s = lock(&self.state);
        if s.sink.is_none() {
            return;
        }
        let first = s.batch.is_empty();
        s.batch.extend_from_slice(bytes);
        s.unacked += bytes.len();
        drop(s);
        if first {
            self.arrived.notify_one();
        }
    }

    /// Sends the batch as the next frame.
    fn flush(&self) -> Flushed {
        let mut guard = lock(&self.state);
        let s = &mut *guard;
        let mut flushed = Flushed::Nothing;
        if let Some(sink) = &s.sink
            && !s.batch.is_empty()
        {
            s.seq += 1;
            let frame = OutputFrame {
                seq: s.seq,
                bytes: mem::take(&mut s.batch),
            };
            flushed = Flushed::Frame;
            if sink.send(frame).is_err() {
                // The receiver is gone, so the sink is detached.
                s.set_sink(None);
                self.acked.notify_one();
            }
        }
        if s.closed { Flushed::Closed } else { flushed }
    }

    /// Tells the flush task the reader has stopped.
    fn close(&self) {
        lock(&self.state).closed = true;
        self.arrived.notify_one();
    }
}

/// A session's batch on its way to a sink: PTY output, or a chat-mode session's messages.
pub(crate) trait Batch {
    /// Waits until something comes for an empty batch, or the sink changes.
    async fn wait(&self);
    /// Sends the batch as the next frame.
    fn flush(&self) -> Flushed;
}

impl Batch for Output {
    async fn wait(&self) {
        wait_for_output(self).await;
    }

    fn flush(&self) -> Flushed {
        Output::flush(self)
    }
}

/// Sends the session's batch to its sink in frames. What comes a flush window or more after the
/// last frame goes out at once. What comes sooner waits until a window after that frame, and
/// goes out with everything that came meanwhile. Ends once the reader has stopped.
pub(crate) async fn flush_batches(batch: Arc<impl Batch>, window: Duration) {
    let mut last_frame = None;
    loop {
        // No timer runs here, so an idle session wakes nothing.
        batch.wait().await;
        if let Some(last) = last_frame {
            time::sleep_until(last + window).await;
        }
        match batch.flush() {
            Flushed::Frame => last_frame = Some(time::Instant::now()),
            Flushed::Nothing => {}
            Flushed::Closed => return,
        }
    }
}

/// Waits until output comes or the sink changes. A sink whose receiver closes meanwhile is
/// detached here: a paused reader adds nothing to send, so no failed send would find out.
async fn wait_for_output(output: &Output) {
    loop {
        // A new sink wakes this, so the clone never keeps a replaced sink open for long.
        let sink = output.sink();
        let mut arrived = pin!(output.arrived.notified());
        let mut closed = pin!(async {
            match &sink {
                Some(sink) => sink.closed().await,
                None => future::pending().await,
            }
        });
        let receiver_closed = future::poll_fn(|cx| {
            if arrived.as_mut().poll(cx).is_ready() {
                return Poll::Ready(false);
            }
            closed.as_mut().poll(cx).map(|()| true)
        })
        .await;
        if !receiver_closed {
            return;
        }
        output.detach_if_closed();
    }
}

/// Runs until the last process holding the PTY closes it. Every chunk goes to the headless
/// parser first, attached or not.
fn read_output(
    mut reader: Box<dyn Read + Send>,
    terminal: &Mutex<Terminal>,
    output: &Output,
    pacing: Pacing,
    activity: &Mutex<Activity>,
) {
    let mut buf = [0; 64 * 1024];
    loop {
        let room = output.room(pacing, buf.len());
        let n = match reader.read(&mut buf[..room]) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        // The output time changes with the screen, under one lock, so a screen read with it
        // is the screen at that time.
        let mut t = lock(terminal);
        t.advance(&buf[..n]);
        let mut a = lock(activity);
        let now = Instant::now();
        a.output = Some(now);
        if a.input.is_some() && a.reply.is_none() {
            a.reply = Some(now);
        }
        drop(a);
        // Still under the parser's lock, so whoever holds it sees each chunk both parsed and
        // pushed, or neither.
        output.push(&buf[..n]);
        drop(t);
    }
}

// Runs until the session is dropped or the PTY or pipe is gone.
pub(crate) fn write_input(mut writer: impl Write, input: mpsc::Receiver<Vec<u8>>) {
    for bytes in input {
        if writer
            .write_all(&bytes)
            .and_then(|()| writer.flush())
            .is_err()
        {
            return;
        }
    }
}

/// Ends the session once its process has exited with `code`, None for a signal.
fn report_exit(
    id: SessionId,
    code: Option<i32>,
    live: &Live,
    kept: &Kept,
    events: &broadcast::Sender<CoreEvent>,
    exits: &UnboundedSender<ProcessExit>,
) {
    // A kept session that left nothing running forgets its leader, whose pid is now free. Looked
    // up before the locks below, as listing every process reads /proc on Linux. Nothing can
    // join the process session once it is empty.
    let leader = lock(kept).get(&id).and_then(|k| k.leader);
    let leader_left = leader.filter(|&leader| session_has_processes(leader));
    // Held until the event is sent, so whoever lists the running sessions after this also gets
    // the event, as removing a worktree relies on. A kept session's output is stored under the
    // same lock, so an attach that finds the session gone also finds its output.
    let mut running = lock(live);
    let session = running.remove(&id);
    let killed = session.as_ref().is_some_and(|s| s.killed);
    // The rest of the session closes here, PTY included.
    let ended = session
        .and_then(|s| s.pty)
        .map(|pty| (pty.terminal, pty.output));
    // Having no receivers is fine.
    let _ = events.send(CoreEvent::SessionExited { session: id, code });
    if let Some(kept) = lock(kept).get_mut(&id) {
        kept.ended = ended;
        kept.leader = leader_left;
        if let Some(on_exit) = kept.on_exit.take() {
            // The waiter may be gone, as when the core stops.
            let _ = on_exit.send(code);
        }
    }
    drop(running);
    // Gone only when the core is stopping.
    let _ = exits.send(ProcessExit {
        session: id,
        code,
        killed,
    });
}

// Makes `cmd`'s process lead a new session, so `kill_session` with its pid ends everything it
// starts.
pub(crate) fn lead_own_session(cmd: &mut Command) {
    // setsid is async-signal-safe, so it may run between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

// Job control gives each background job of a shell its own process group, so killing the
// shell's group would miss them. They all stay in the shell's session, so kill that instead.
pub(crate) fn kill_session(sid: libc::pid_t) {
    // Sessions 0 and 1 belong to the kernel and init.
    if sid <= 1 {
        return;
    }
    // A process can fork between the listing and its signal, so list again until no new
    // process shows up.
    let mut killed = HashSet::new();
    loop {
        // To kill(), pid 0 means this process's own group and 1 is init.
        let found: Vec<_> = processes()
            .into_iter()
            .filter(|&pid| pid > 1 && unsafe { libc::getsid(pid) } == sid && killed.insert(pid))
            .collect();
        if found.is_empty() {
            return;
        }
        for pid in found {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
}

fn session_has_processes(sid: libc::pid_t) -> bool {
    processes()
        .into_iter()
        .any(|pid| pid > 1 && unsafe { libc::getsid(pid) } == sid)
}

// A process we may not signal still exists.
fn process_exists(pid: libc::pid_t) -> bool {
    let signalled = unsafe { libc::kill(pid, 0) } == 0;
    signalled || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(target_os = "macos")]
fn processes() -> Vec<libc::pid_t> {
    // With no buffer it returns the number of processes, plus some room.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::pid_t> = vec![0; count];
    let size = libc::c_int::try_from(size_of_val(pids.as_slice())).unwrap_or(libc::c_int::MAX);
    let found = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), size) };
    pids.truncate(usize::try_from(found).unwrap_or(0));
    pids
}

#[cfg(target_os = "linux")]
fn processes() -> Vec<libc::pid_t> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use tempfile::TempDir;

    use super::*;
    use crate::CoreConfig;

    /// A core tracking one fresh repo, running `script` in dash in its main worktree at 80x24.
    fn start(script: &str) -> (TempDir, Core, SessionId) {
        let (dir, core, worktree) = core_in_repo();
        let session = start_in(&core, &worktree, core.sessions.new_id(), script);
        (dir, core, session)
    }

    /// A core tracking one fresh repo, and its main worktree.
    fn core_in_repo() -> (TempDir, Core, PathBuf) {
        // macOS caps a socket path at 104 bytes, so the home folder stays under /tmp.
        let dir = tempfile::Builder::new()
            .prefix("grove")
            .tempdir_in("/tmp")
            .unwrap();
        let (core, _events) = Core::start(CoreConfig::new(dir.path().join("home"))).unwrap();
        // A session starts only in a tracked worktree.
        let repo = dir.path().join("repo");
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap();
        assert!(init.success());
        let worktree = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                core.add_repo(&repo).await.unwrap();
                core.list_worktrees().await[0].path.clone()
            });
        (dir, core, worktree)
    }

    /// Runs `script` in dash as session `id` in `worktree`, at 80x24.
    fn start_in(core: &Core, worktree: &Path, id: SessionId, script: &str) -> SessionId {
        let spec = SpawnSpec {
            command: "/bin/dash".into(),
            args: vec!["-c".into(), script.into()],
            cols: 80,
            rows: 24,
        };
        core.start_session(id, worktree, spec, &[], Kind::Command)
            .unwrap()
    }

    /// Waits until the session's screen shows `text`.
    fn wait_for_screen(core: &Core, session: SessionId, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let screen = || core.sessions.probe().screen(session).unwrap().0;
        while !screen().contains(text) {
            assert!(Instant::now() < deadline, "{text:?} never showed");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_kept_session_that_left_nothing_running_has_nothing_to_kill() {
        let (_dir, core, worktree) = core_in_repo();
        let id = core.sessions.new_id();
        let exited = core.sessions.keep(id);
        start_in(&core, &worktree, id, "true");
        exited.blocking_recv().unwrap();

        // Its pid is free for the kernel to hand out again.
        assert_eq!(core.sessions.release_with_leader(id), None);
    }

    #[test]
    fn a_kept_session_whose_pid_another_process_now_has_has_nothing_to_kill() {
        let (_dir, core, worktree) = core_in_repo();
        let id = core.sessions.new_id();
        let exited = core.sessions.keep(id);
        start_in(&core, &worktree, id, "true");
        exited.blocking_recv().unwrap();
        // This test's own pid stands for the kernel handing the leader's out again.
        let other = libc::pid_t::try_from(std::process::id()).unwrap();
        lock(&core.sessions.kept).get_mut(&id).unwrap().leader = Some(other);

        assert_eq!(core.sessions.release_with_leader(id), None);
    }

    /// A panel that wrote `snapshot`.
    fn panel_from(snapshot: &Snapshot) -> Terminal {
        let mut panel = Terminal::new(snapshot.cols, snapshot.rows, mpsc::channel().0);
        panel.advance(&snapshot.bytes);
        panel
    }

    #[test]
    fn the_core_reads_the_screen_and_bracketed_paste_from_the_parser() {
        let (_dir, core, session) = start(r"printf 'hello\033[?2004h'; sleep 1000");

        let deadline = Instant::now() + Duration::from_secs(10);
        while !core.bracketed_paste(session).unwrap() {
            assert!(Instant::now() < deadline, "bracketed paste never came on");
            thread::sleep(Duration::from_millis(10));
        }

        let (screen, _) = core.sessions.probe().screen(session).unwrap();
        assert!(screen.starts_with("hello\n"));
    }

    #[test]
    fn attaching_to_a_session_that_printed_2000_lines_returns_the_screen_and_the_last_1000() {
        let (_dir, core, session) =
            start(r#"i=1; while [ $i -le 2000 ]; do echo "line $i"; i=$((i+1)); done; sleep 1000"#);
        wait_for_screen(&core, session, "line 2000");
        let (sink, _frames) = unbounded_channel();

        let snapshot = core.attach(session, sink).unwrap();

        assert_eq!((snapshot.seq, snapshot.cols, snapshot.rows), (0, 80, 24));
        let panel = panel_from(&snapshot);
        assert_eq!(panel.scrollback_len(), 1000);
        assert_eq!(panel.line_text(-1000), "line 978");
        assert_eq!(panel.line_text(0), "line 1978");
        assert_eq!(panel.line_text(22), "line 2000");
    }

    /// Prints numbered lines without end, a few bytes per write, so reads end inside escape
    /// sequences and UTF-8 characters: colors, links, a line feed inside a CSI, combining
    /// characters, and a synchronized update every 50 lines.
    const STREAM: &str = r"i=0; while :; do i=$((i+1))
        printf '\033[38;2;%d;7;200m%07d\033[0m caf\303' $((i % 256)) $i
        printf '\251 \346\274\242\345\255\227 \033]8;;https://e.x/%d\033' $i
        printf '\\link\033]8;;\033\\ \033[1'
        printf '\n2m\033[0m e\314'
        printf '\201\r\n'
        if [ $((i % 50)) -eq 0 ]; then
            printf '\033[?2026h\0337\033[1;70H\033[7m%07d\033[0m\0338\033[?2026l' $i
        fi
    done";

    #[test]
    fn under_steady_output_each_attach_hands_over_from_its_snapshot_to_frames_without_a_seam() {
        let (_dir, core, session) = start(STREAM);
        let (terminal, output) = {
            let live = core.sessions.live();
            let pty = live[&session].pty.as_ref().unwrap();
            (pty.terminal.clone(), pty.output.clone())
        };
        // A small xorshift, so runs repeat without a dependency.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut below = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut cut_snapshots = 0;

        for attach in 0..200 {
            thread::sleep(Duration::from_millis(below(5)));
            let (sink, mut frames) = unbounded_channel();
            let snapshot = core.attach(session, sink).unwrap();
            // A panel for the snapshot and the frames after it.
            let mut panel = panel_from(&snapshot);
            if panel.mid_sequence() {
                cut_snapshots += 1;
            }
            let mut next = snapshot.seq + 1;
            let mut take = |frame: OutputFrame, panel: &mut Terminal| {
                assert_eq!(frame.seq, next, "attach {attach}");
                next += 1;
                panel.advance(&frame.bytes);
                frame.bytes.len()
            };
            let until = Instant::now() + Duration::from_millis(below(10));
            while Instant::now() < until {
                while let Ok(frame) = frames.try_recv() {
                    let drawn = take(frame, &mut panel);
                    core.ack(session, drawn);
                }
                thread::sleep(Duration::from_millis(1));
            }

            // With both locks held, the reader and the flush task stand still. The frames sent
            // and the batch not sent yet are all the output since the snapshot.
            let live = lock(&terminal);
            let state = lock(&output.state);
            while let Ok(frame) = frames.try_recv() {
                take(frame, &mut panel);
            }
            panel.advance(&state.batch);
            assert_eq!(
                live.differences(&panel),
                Vec::<String>::new(),
                "attach {attach}"
            );
            drop(state);
            drop(live);
            core.detach(session);
        }

        // Most reads end inside a sequence, so many snapshots do too.
        assert!(
            cut_snapshots >= 20,
            "{cut_snapshots} of 200 snapshots cut a sequence"
        );
    }
}
