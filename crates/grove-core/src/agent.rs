//! Agent sessions: launching them through their adapter, and their state from hooks.

mod claude;
mod codex;
pub(crate) mod heuristic;
mod records;

pub use records::AgentSessionInfo;
use records::title_of;

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::future::poll_fn;
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::net as std_net;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::task::Poll;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{broadcast, oneshot};
use tokio::time;

use crate::chat::{Chat, ModelChoice};
use crate::panel_request::{self, Request, SessionToken};
use crate::restore::AgentRecord;
use crate::session::{GROVE_SESSION_ID, GROVE_SESSION_TOKEN, GROVE_SOCKET, Kind, ProcessExit};
use crate::state::SaveRequests;
use crate::{
    Core, CoreConfig, CoreError, CoreEvent, CoreInner, LaunchArgs, PanelSpec, Result, SessionId,
    SessionKind, SpawnSpec, Warning,
};

/// An agent CLI grove runs. Only Claude Code and Codex until after M7 (ADR 0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// The name grove-hook puts in its messages, and profiles use.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }
}

/// How an agent session runs. GLOSSARY.md has the meanings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Chat,
    Terminal,
}

/// A new agent session.
#[derive(Debug, Clone)]
pub struct AgentSpec {
    pub agent: Agent,
    pub mode: Mode,
    /// The first prompt. In terminal mode it goes in argv, never typed into the PTY. In chat
    /// mode it is the first message.
    pub prompt: Option<String>,
    /// The terminal size in cells. Chat mode has no terminal and ignores it.
    pub cols: u16,
    pub rows: u16,
    /// The agent panel to start it in: an index into the worktree's `chat` as `workspace`
    /// returns it. None, or a panel that already has a session, puts it in a new shown panel
    /// after the last one.
    pub panel: Option<usize>,
}

/// Who is logged in to an agent, as it reports it. grove never sees the credentials.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AccountInfo {
    pub email: Option<String>,
    pub organization: Option<String>,
    /// The subscription plan, such as "max". None with an API key.
    pub plan: Option<String>,
    /// Where the API key came from, such as "ANTHROPIC_API_KEY". None with a subscription login.
    pub api_key_source: Option<String>,
}

/// What an agent session is doing. GLOSSARY.md has the meanings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Working,
    /// The agent waits on the user.
    NeedsInput {
        kind: InputKind,
    },
    /// The turn or the process ended on an error.
    Failed,
    /// The turn ended and the user has not focused the session since.
    Done,
    /// Nothing is waiting on the user.
    Idle,
    /// The process ended cleanly or grove stopped it.
    Exited,
}

/// What a `NeedsInput` session waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    Approval,
    Question,
}

/// Where an agent state came from. In chat mode it is always Protocol (ADR 0014). In terminal
/// mode Hook beats Heuristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateSource {
    Protocol,
    Hook,
    Heuristic,
}

/// An agent session waiting on the user, as the attention queue lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionEntry {
    pub session: SessionId,
    pub worktree: PathBuf,
    pub state: AgentState,
    /// When the session entered `state`, by the wall clock. On macOS an `Instant` stops while
    /// the Mac sleeps, so an age shown from one would leave that time out.
    pub since: SystemTime,
}

/// What an agent's screen shows, for the status heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    /// A dialog that waits on the user's approval, such as Claude's folder trust dialog.
    Dialog,
    /// The input prompt. `error`: the last turn above it ended on an error.
    Prompt {
        error: bool,
    },
    Other,
}

/// What an adapter gets to launch one session.
pub(crate) struct LaunchCtx<'a> {
    pub session: SessionId,
    /// The agent's own session id for a new conversation, which its hooks carry. Codex picks
    /// its own and ignores this one. With `resume`, the conversation to go on with.
    pub resume_id: &'a str,
    pub resume: bool,
    pub prompt: Option<&'a str>,
    /// The session's saved model and effort. None runs the agent's default.
    pub model: Option<&'a ModelChoice>,
    /// The repo's extra args for this agent. They go last.
    pub launch_args: &'a [String],
    /// The session's own folder under the home folder.
    pub dir: &'a Path,
    /// The token of the process this launch starts, and the core socket. Its MCP server needs
    /// both.
    pub token: &'a str,
    pub socket: &'a Path,
    pub cols: u16,
    pub rows: u16,
}

/// What a chat-mode agent session runs, on pipes.
pub(crate) struct ChatSpec {
    pub command: PathBuf,
    pub args: Vec<OsString>,
    /// The file its stderr goes to, for debugging. The core never reads it.
    pub stderr: PathBuf,
}

/// What starts an agent session's process: a new conversation, or one going on with
/// `resume_id`'s.
struct AgentStart<'a> {
    agent: Agent,
    mode: Mode,
    resume_id: &'a str,
    resume: bool,
    prompt: Option<&'a str>,
    model: Option<ModelChoice>,
    cols: u16,
    rows: u16,
}

/// Per-agent code: the launch command with its integration flags, and hook parsing.
pub(crate) trait AgentAdapter {
    /// Writes the session's files into `ctx.dir` and returns what to run in terminal mode.
    fn launch(&self, ctx: &LaunchCtx) -> Result<SpawnSpec>;
    /// Like `launch`, for chat mode: the transport to start, with the same integration. It
    /// ignores the prompt. Codex keeps the launch args' `-c` overrides, and Claude ignores them.
    fn launch_chat(&self, ctx: &LaunchCtx) -> Result<ChatSpec>;
    /// The first of a repo's launch args that would replace one of grove's own flags or
    /// overrides. They come after grove's, so the agent would take theirs.
    fn clashing_arg<'a>(&self, launch_args: &'a [String]) -> Option<&'a String>;
    fn parse_event(&self, event: &HookEvent) -> Option<AgentState>;
    /// Reads the session's screen, one line per row, as the headless parser has it.
    fn read_screen(&self, screen: &str) -> Screen;
}

/// A hook payload, with the fields grove reads.
#[derive(Debug, Deserialize)]
pub(crate) struct HookEvent {
    pub hook_event_name: String,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub reason: Option<String>,
}

/// The status table both agents share. `question_tool` is the tool the agent asks the user a
/// question with. Only Claude fires `StopFailure`, and only Codex fires `Interrupt`.
fn hook_state(event: &HookEvent, question_tool: &str) -> Option<AgentState> {
    let asks = event.tool_name.as_deref() == Some(question_tool);
    match event.hook_event_name.as_str() {
        // Claude fires PermissionRequest for its question tool too.
        "PreToolUse" | "PermissionRequest" if asks => Some(AgentState::NeedsInput {
            kind: InputKind::Question,
        }),
        "PermissionRequest" => Some(AgentState::NeedsInput {
            kind: InputKind::Approval,
        }),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(AgentState::Working),
        "Stop" => Some(AgentState::Done),
        "StopFailure" => Some(AgentState::Failed),
        "Interrupt" => Some(AgentState::Idle),
        // /clear and /resume in Claude's TUI end one conversation, and the process goes on
        // with the next.
        "SessionEnd" if matches!(event.reason.as_deref(), Some("clear" | "resume")) => None,
        "SessionEnd" => Some(AgentState::Exited),
        _ => None,
    }
}

/// What grove-hook writes to the socket, one per connection.
#[derive(Debug, Deserialize)]
struct HookMessage {
    session: String,
    agent: String,
    event: HookEvent,
}

/// The agent sessions the core started, and the state their hooks reported.
pub(crate) struct Agents {
    warnings: Arc<crate::warnings::Warnings>,
    home: PathBuf,
    socket: PathBuf,
    claude: claude::ClaudeCode,
    codex: codex::Codex,
    timers: heuristic::Timers,
    live: Mutex<HashMap<SessionId, Tracked>>,
    // Worktree path to its most recently focused agent session.
    last_focused: Mutex<HashMap<PathBuf, SessionId>>,
    // Each change to what a restart brings back asks for a save.
    saves: SaveRequests,
    max_live_subagents_per_parent: usize,
}

struct Tracked {
    agent: Agent,
    worktree: PathBuf,
    mode: Mode,
    // The terminal size it was spawned with, for terminal mode after a switch.
    size: (u16, u16),
    // Some for a session in chat mode.
    chat: Option<Arc<Chat>>,
    // `set_mode` runs. Chat calls are refused until it is done.
    switching: bool,
    // Told when the process `set_mode` ended has exited and its hooks are applied.
    exited: Option<oneshot::Sender<()>>,
    // None until a Codex session's first SessionStart.
    resume_id: Option<String>,
    // None until `set_model`. The agent's default runs until then.
    model: Option<ModelChoice>,
    // The token of its running process, which its panel requests must carry. None once that
    // process has exited.
    token: Option<SessionToken>,
    // None until the first hook or heuristic.
    state: Option<(AgentState, StateSource)>,
    // When the state last changed or a hook event set it. Heuristics act only on what came
    // after, so a hook always beats them.
    since: Instant,
    // When the state last changed. Unlike `since`, a hook that repeats the state or a new
    // source for it leaves this alone, so a session keeps its place in the attention queue.
    entered: Instant,
    // The same moment by the wall clock, for the age the UI shows. It can go back when the
    // clock is set, so the queue orders by `entered`.
    entered_wall_clock: SystemTime,
    // A hook from this conversation arrived, so hooks run.
    hooks_seen: bool,
    // The output the last check for a settled screen was for.
    settled: Option<Instant>,
    // The start of the quiet spell the last stuck check was for.
    checked: Option<Instant>,
    // Its process ended, so its state is final.
    ended: bool,
    // The agent session that spawned it as a subagent.
    parent: Option<SessionId>,
    // None until its first user message.
    title: Option<String>,
    // When it started, to the millisecond, as the state file keeps it.
    created: SystemTime,
}

impl Tracked {
    fn new(
        agent: Agent,
        worktree: &Path,
        mode: Mode,
        size: (u16, u16),
        resume_id: Option<String>,
    ) -> Tracked {
        Tracked {
            agent,
            worktree: worktree.to_path_buf(),
            mode,
            size,
            chat: None,
            switching: false,
            exited: None,
            resume_id,
            model: None,
            token: None,
            state: None,
            since: Instant::now(),
            entered: Instant::now(),
            entered_wall_clock: SystemTime::now(),
            hooks_seen: false,
            settled: None,
            checked: None,
            ended: false,
            parent: None,
            title: None,
            created: now_to_the_millisecond(),
        }
    }

    // The session as the state file kept it.
    fn of_record(worktree: &Path, record: AgentRecord) -> Tracked {
        let size = (record.cols, record.rows);
        let mut tracked = Tracked::new(record.agent, worktree, record.mode, size, record.resume_id);
        tracked.model = record.model;
        tracked.parent = record.parent;
        tracked.title = record.title;
        if let Some(created) = record.created_unix_ms {
            tracked.created = UNIX_EPOCH + Duration::from_millis(created);
        }
        tracked.ended = record.ended;
        tracked
    }

    fn record(&self, id: SessionId) -> AgentRecord {
        let created = self.created.duration_since(UNIX_EPOCH).ok();
        AgentRecord {
            id,
            agent: self.agent,
            mode: self.mode,
            resume_id: self.resume_id.clone(),
            model: self.model.clone(),
            cols: self.size.0,
            rows: self.size.1,
            parent: self.parent,
            title: self.title.clone(),
            created_unix_ms: created.and_then(|since| u64::try_from(since.as_millis()).ok()),
            ended: self.ended,
        }
    }
}

// The time now, cut to the millisecond the state file keeps, so a session's created time reads
// the same before and after a restart.
fn now_to_the_millisecond() -> SystemTime {
    let since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let millis = u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX);
    UNIX_EPOCH + Duration::from_millis(millis)
}

impl Agents {
    /// Marks the session as running in `mode` with `token` from now on, before its process
    /// starts, so its first hook, message or panel request finds it. Its state stays until the
    /// new process sets one.
    fn begin(
        &self,
        session: SessionId,
        mode: Mode,
        chat: Option<Arc<Chat>>,
        token: Option<SessionToken>,
    ) {
        if let Some(tracked) = lock(&self.live).get_mut(&session) {
            tracked.mode = mode;
            tracked.chat = chat;
            tracked.token = token;
            tracked.since = Instant::now();
            tracked.hooks_seen = false;
            tracked.settled = None;
            tracked.checked = None;
            tracked.ended = false;
        }
        self.saves.request();
    }

    /// Every agent session with its worktree, as the state file keeps it, ended or not. One
    /// switching mode counts as running.
    pub(crate) fn records(&self) -> Vec<(PathBuf, AgentRecord)> {
        lock(&self.live)
            .iter()
            .map(|(&id, tracked)| (tracked.worktree.clone(), tracked.record(id)))
            .collect()
    }

    /// The agent sessions in `worktree` whose process has not ended, as `sessions` lists them.
    pub(crate) fn running_in(&self, worktree: &Path) -> Vec<(SessionId, SessionKind)> {
        lock(&self.live)
            .iter()
            .filter(|(_, t)| !t.ended && t.worktree == worktree)
            .map(|(&id, t)| {
                let kind = SessionKind::Agent {
                    agent: t.agent,
                    mode: t.mode,
                    parent: t.parent,
                };
                (id, kind)
            })
            .collect()
    }

    pub(crate) fn new(
        cfg: &CoreConfig,
        saves: SaveRequests,
        warnings: Arc<crate::warnings::Warnings>,
    ) -> Self {
        Self {
            warnings,
            home: cfg.home.clone(),
            socket: cfg.socket.clone(),
            claude: claude::ClaudeCode {
                command: cfg.claude_command.clone(),
                sidecar: cfg.claude_sidecar_command.clone(),
                hook_relay: cfg.hook_relay.clone(),
                mcp_server: cfg.mcp_server.clone(),
            },
            codex: codex::Codex {
                command: cfg.codex_command.clone(),
                hook_relay: cfg.hook_relay.clone(),
                mcp_server: cfg.mcp_server.clone(),
            },
            timers: heuristic::Timers::new(cfg),
            live: Mutex::default(),
            last_focused: Mutex::default(),
            saves,
            max_live_subagents_per_parent: cfg.max_live_subagents_per_parent,
        }
    }

    fn adapter(&self, agent: Agent) -> &dyn AgentAdapter {
        match agent {
            Agent::Claude => &self.claude,
            Agent::Codex => &self.codex,
        }
    }

    /// Refuses launch args that would replace one of grove's own flags or overrides.
    pub(crate) fn check_launch_args(&self, args: &LaunchArgs) -> Result<()> {
        for (agent, list) in [(Agent::Claude, &args.claude), (Agent::Codex, &args.codex)] {
            let list = list.as_deref().unwrap_or_default();
            if let Some(arg) = self.adapter(agent).clashing_arg(list) {
                return Err(CoreError::InvalidLaunchArgs {
                    agent,
                    arg: arg.clone(),
                });
            }
        }
        Ok(())
    }

    fn on_hook(&self, msg: HookMessage, events: &broadcast::Sender<CoreEvent>) {
        let Ok(id) = msg.session.parse() else {
            return;
        };
        let session = SessionId(id);
        let mut live = lock(&self.live);
        let Some(tracked) = live.get_mut(&session) else {
            return;
        };
        if tracked.ended || msg.agent != tracked.agent.name() {
            return;
        }
        let Some(resume_id) = msg.event.session_id.clone() else {
            return;
        };
        // Every SessionStart carries the conversation from now on, as after /clear.
        if msg.event.hook_event_name == "SessionStart" {
            if tracked.resume_id.as_ref() != Some(&resume_id) {
                tracked.resume_id = Some(resume_id);
                self.saves.request();
            }
        } else if tracked.resume_id.as_ref() != Some(&resume_id) || tracked.mode == Mode::Chat {
            // Only the current conversation counts. Chat state comes from the protocol.
            return;
        }
        if !tracked.hooks_seen {
            tracked.hooks_seen = true;
            self.warnings.clear_hooks(session);
        }
        if msg.event.hook_event_name == "SessionStart" {
            return;
        }
        if let Some(state) = self.adapter(tracked.agent).parse_event(&msg.event) {
            tracked.since = Instant::now();
            tracked.set(session, state, StateSource::Hook, events);
        }
    }

    /// The agent session a panel request comes from, when `token` is the token of that
    /// session's running process. None for anything else, an ended session included.
    fn authenticate(&self, session: &str, token: &str) -> Option<SessionId> {
        let named = session;
        let session = SessionId(named.parse().ok()?);
        // Only the form the id prints in, so "+3" or "03" names no session.
        if session.to_string() != named {
            return None;
        }
        let live = lock(&self.live);
        let tracked = live.get(&session).filter(|tracked| !tracked.ended)?;
        tracked
            .token
            .as_ref()
            .is_some_and(|own| own.matches(token))
            .then_some(session)
    }

    // Exit 0 or grove's kill is Exited, anything else Failed. A quit with no SessionEnd, such
    // as a double Ctrl-C, still exits 0. The exit is as certain as a hook, so in terminal mode
    // it takes the Hook source, and no later hook changes it. In chat mode it is Protocol.
    fn on_exit(&self, exit: &ProcessExit, events: &broadcast::Sender<CoreEvent>) {
        let mut live = lock(&self.live);
        // Every session that ends changes what a restart brings back, shells and run sessions too.
        // Asked under the lock, so the save reads an agent session after it is marked ended.
        self.saves.request();
        let Some(tracked) = live.get_mut(&exit.session) else {
            return;
        };
        // The token goes with the process that had it.
        tracked.token = None;
        self.warnings.clear_hooks(exit.session);
        // `set_mode` ended it, and the session goes on in the other mode.
        if let Some(exited) = tracked.exited.take() {
            let _ = exited.send(());
            return;
        }
        tracked.ended = true;
        let state = if exit.killed || exit.code == Some(0) {
            AgentState::Exited
        } else {
            AgentState::Failed
        };
        tracked.set(exit.session, state, tracked.source(), events);
        // After its state, so a list read on this event has both.
        let _ = events.send(CoreEvent::AgentSessionsChanged);
    }

    /// A state a chat-mode session's protocol reported, for as long as its process runs.
    pub(crate) fn on_protocol(
        &self,
        session: SessionId,
        state: AgentState,
        events: &broadcast::Sender<CoreEvent>,
    ) {
        let mut live = lock(&self.live);
        let Some(tracked) = live.get_mut(&session).filter(|t| !t.ended) else {
            return;
        };
        tracked.since = Instant::now();
        tracked.set(session, state, StateSource::Protocol, events);
    }

    /// The thread a Codex session in chat mode started or resumed, which resumes it later.
    pub(crate) fn on_thread(&self, session: SessionId, thread: String) {
        if let Some(tracked) = lock(&self.live).get_mut(&session) {
            tracked.resume_id = Some(thread);
        }
        self.saves.request();
    }

    /// Saves `choice` with the session, for every launch from now on.
    pub(crate) fn choose_model(&self, session: SessionId, choice: ModelChoice) {
        if let Some(tracked) = lock(&self.live).get_mut(&session) {
            tracked.model = Some(choice);
        }
        self.saves.request();
    }

    pub(crate) fn model_choice(&self, session: SessionId) -> Result<Option<ModelChoice>> {
        lock(&self.live)
            .get(&session)
            .map(|tracked| tracked.model.clone())
            .ok_or(CoreError::NotAnAgentSession(session))
    }

    /// The chat of a session in chat mode.
    pub(crate) fn chat(&self, session: SessionId) -> Result<Arc<Chat>> {
        let live = lock(&self.live);
        let tracked = live.get(&session);
        if tracked.is_some_and(|tracked| tracked.switching) {
            return Err(CoreError::SwitchingMode(session));
        }
        tracked
            .and_then(|tracked| tracked.chat.clone())
            .ok_or(CoreError::NotInChatMode(session))
    }
}

impl Tracked {
    /// Where the state comes from when the agent's own signals set it: in chat mode the
    /// protocol, in terminal mode hooks (ADR 0014).
    fn source(&self) -> StateSource {
        match self.mode {
            Mode::Chat => StateSource::Protocol,
            Mode::Terminal => StateSource::Hook,
        }
    }

    fn set(
        &mut self,
        session: SessionId,
        state: AgentState,
        source: StateSource,
        events: &broadcast::Sender<CoreEvent>,
    ) {
        if self.state != Some((state, source)) {
            if self.state.map(|(old, _)| old) != Some(state) {
                self.entered = Instant::now();
                self.entered_wall_clock = SystemTime::now();
            }
            self.state = Some((state, source));
            self.since = Instant::now();
            // Having no receivers is fine.
            let _ = events.send(CoreEvent::AgentState {
                session,
                state,
                source,
            });
        }
    }
}

impl Core {
    /// Starts an agent session in a tracked worktree, in `spec.mode`. Its hooks and MCP config
    /// are passed at launch from files under the home folder in both modes (ADR 0010).
    /// It goes in agent panel `spec.panel` and the workspace is saved, so a reload never finds
    /// it without a panel. A panel past the last one is refused and starts nothing.
    pub fn spawn_agent(&self, worktree: &Path, spec: AgentSpec) -> Result<SessionId> {
        let (agent, panel) = (spec.agent, spec.panel);
        let ended = self.check_agent_panel(worktree, panel)?;
        let session = self.spawn_agent_under(worktree, spec, None)?;
        self.place_agent(worktree, session, agent, panel, &ended);
        Ok(session)
    }

    /// `spawn_agent` without a panel, for a subagent of `parent` when there is one.
    fn spawn_agent_under(
        &self,
        worktree: &Path,
        spec: AgentSpec,
        parent: Option<SessionId>,
    ) -> Result<SessionId> {
        self.check_tracked(worktree)?;
        let session = self.sessions.new_id();
        let resume_id = new_uuid()?;
        // Codex picks its own id. Its first SessionStart reports it, or in chat mode the thread
        // it starts.
        let known = (spec.agent == Agent::Claude).then(|| resume_id.clone());
        let mut tracked = Tracked::new(
            spec.agent,
            worktree,
            spec.mode,
            (spec.cols, spec.rows),
            known,
        );
        tracked.parent = parent;
        tracked.title = spec.prompt.as_deref().and_then(title_of);
        {
            let mut live = lock(&self.agents.live);
            // A parent deleted since it asked spawns nothing, so no record names a parent
            // that is gone.
            if let Some(parent) = parent.filter(|parent| !live.contains_key(parent)) {
                return Err(CoreError::NotAnAgentSession(parent));
            }
            // Counted under the lock that inserts it, so two requests never both take the
            // last place under the limit.
            if let Some(parent) = parent {
                self.agents.check_live_subagents(&live, parent)?;
            }
            // Tracked before it starts, so its first hook or message finds it.
            live.insert(session, tracked);
        }
        let start = AgentStart {
            agent: spec.agent,
            mode: spec.mode,
            resume_id: &resume_id,
            resume: false,
            prompt: spec.prompt.as_deref(),
            model: None,
            cols: spec.cols,
            rows: spec.rows,
        };
        self.start_agent(session, worktree, start)
            .inspect_err(|_| self.untrack(session))?;
        Ok(session)
    }

    /// Brings back the agent session of `record` after a restart. One that had ended comes back
    /// ended and `Exited`, with no process. Any other starts again in its mode, on its saved
    /// model: it resumes its conversation with the same integration as its first launch, or
    /// starts a new conversation when no id was captured.
    pub(crate) fn restore_agent(&self, worktree: &Path, record: AgentRecord) -> Result<()> {
        let (session, mode, ended) = (record.id, record.mode, record.ended);
        let mut tracked = Tracked::of_record(worktree, record);
        if ended {
            // Whatever it ended on, it waits on nobody now, so it stays out of the attention
            // queue.
            tracked.state = Some((AgentState::Exited, tracked.source()));
        }
        lock(&self.agents.live).insert(session, tracked);
        if ended {
            return Ok(());
        }
        self.resume_in(session, mode)
            .inspect_err(|_| self.untrack(session))?;
        Ok(())
    }

    /// Starts a subagent for agent session `parent`: a new agent session in chat mode, in
    /// `worktree` or else the parent's own, at the parent's size (ADR 0006, 0013). Another worktree
    /// of the parent's repo nests under the parent's, as `nest_worktree` allows. Sends
    /// `PanelRequested` with the parent. Returns the new session and its worktree.
    pub(crate) fn spawn_subagent(
        &self,
        parent: SessionId,
        agent: Agent,
        prompt: String,
        worktree: Option<PathBuf>,
    ) -> Result<(SessionId, PathBuf)> {
        let live = lock(&self.agents.live);
        // A parent that ended since it asked spawns nothing.
        let (parent_worktree, (cols, rows)) = live
            .get(&parent)
            .filter(|tracked| !tracked.ended)
            .map(|tracked| (tracked.worktree.clone(), tracked.size))
            .ok_or(CoreError::NotAnAgentSession(parent))?;
        if subagent_depth(&live, parent) >= MAX_SUBAGENT_DEPTH {
            return Err(CoreError::SubagentTooDeep {
                parent,
                limit: MAX_SUBAGENT_DEPTH,
            });
        }
        // Refused here before anything changes. The check that decides is the one under the
        // lock that inserts the new session.
        self.agents.check_live_subagents(&live, parent)?;
        drop(live);
        let worktree = worktree.unwrap_or_else(|| parent_worktree.clone());
        // Nested before the spawn, so the list is right by the new session's first state.
        let nested = worktree != parent_worktree && self.nest_worktree(&worktree, &parent_worktree);
        let spec = AgentSpec {
            agent,
            mode: Mode::Chat,
            prompt: Some(prompt),
            cols,
            rows,
            panel: None,
        };
        let session = self
            .spawn_agent_under(&worktree, spec, Some(parent))
            .inspect_err(|_| {
                if nested {
                    self.unnest_worktree(&worktree);
                }
            })?;
        if nested {
            self.saves.request();
        }
        // Having no receivers is fine.
        let _ = self.events.send(CoreEvent::PanelRequested {
            parent: Some(parent),
            panel: PanelSpec::Agent {
                session,
                worktree: worktree.clone(),
            },
        });
        Ok((session, worktree))
    }

    // Forgets an agent session whose process never started.
    fn untrack(&self, session: SessionId) {
        lock(&self.agents.live).remove(&session);
        self.agents.saves.request();
    }

    /// Switches an agent session to `mode`: ends its process and resumes the same conversation
    /// in the other mode under the same session id, with the same integration (ADR 0013). In
    /// chat mode it returns once the conversation so far is read back from the agent's own
    /// record. A Codex session with no conversation yet starts a new one. Refused while the
    /// session is `Working` or `NeedsInput`, or while a message sent to it has no turn started
    /// yet, and changes nothing then. A session already in `mode` stays as it is.
    pub async fn set_mode(&self, session: SessionId, mode: Mode) -> Result<()> {
        let chat = {
            let mut live = lock(&self.agents.live);
            let tracked = live
                .get_mut(&session)
                .ok_or(CoreError::NotAnAgentSession(session))?;
            if tracked.switching {
                return Err(CoreError::SwitchingMode(session));
            }
            if tracked.mode == mode {
                return Ok(());
            }
            if let Some((state @ (AgentState::Working | AgentState::NeedsInput { .. }), _)) =
                tracked.state
            {
                return Err(CoreError::AgentBusy { session, state });
            }
            // New sends are refused from here on.
            tracked.switching = true;
            tracked.chat.clone()
        };
        // A send that got past its own check before this either queued its message already, and
        // the switch stops, or finds sends ended. Outside the agents lock, since the chat's lock
        // is always taken first.
        if let Some(chat) = chat
            && !chat.end_sends()
        {
            if let Some(tracked) = lock(&self.agents.live).get_mut(&session) {
                tracked.switching = false;
            }
            return Err(CoreError::MessagePending(session));
        }
        // Under the same lock as `on_exit`, so the exit of a running process is still to come
        // and finds this.
        let exited = lock(&self.agents.live)
            .get_mut(&session)
            .filter(|tracked| !tracked.ended)
            .map(|tracked| {
                let (exited, on_exit) = oneshot::channel();
                tracked.exited = Some(exited);
                on_exit
            });
        // Ends the switch however this returns, even when the caller drops it.
        let mut switch = Switch {
            agents: &self.agents,
            events: &self.events,
            session,
            started: false,
        };
        if let Some(on_exit) = exited {
            // Fails only when the process has just exited, and its exit still comes.
            let _ = self.kill(session);
            // Its exit is applied and its id is free again, so the new process can take it.
            let _ = on_exit.await;
        }
        let history = self.resume_in(session, mode)?;
        switch.started = true;
        if let Some(chat) = history {
            // A history that never comes, or that the core cannot read, must not leave the
            // session switching for good. The chat goes on without it.
            let _ = time::timeout(HISTORY_WAIT, chat.history_read()).await;
        }
        Ok(())
    }

    // Starts the session's conversation in `mode`. Returns the chat whose history is still to
    // come when it resumes one in chat mode.
    fn resume_in(&self, session: SessionId, mode: Mode) -> Result<Option<Arc<Chat>>> {
        let (agent, worktree, resume_id, model, (cols, rows)) = {
            let live = lock(&self.agents.live);
            let t = live
                .get(&session)
                .ok_or(CoreError::NotAnAgentSession(session))?;
            let model = t.model.clone();
            (
                t.agent,
                t.worktree.clone(),
                t.resume_id.clone(),
                model,
                t.size,
            )
        };
        let (id, resume) = match resume_id {
            Some(id) => (id, true),
            None => (new_uuid()?, false),
        };
        let start = AgentStart {
            agent,
            mode,
            resume_id: &id,
            resume,
            prompt: None,
            model,
            cols,
            rows,
        };
        let chat = self.start_agent(session, &worktree, start)?;
        Ok(chat.filter(|_| resume))
    }

    // Starts the agent session's process as `start` says, and returns its chat in chat mode.
    fn start_agent(
        &self,
        session: SessionId,
        worktree: &Path,
        start: AgentStart,
    ) -> Result<Option<Arc<Chat>>> {
        let agents = &self.agents;
        let dir = agents.session_folder(session);
        fs::create_dir_all(&dir).map_err(CoreError::io("create the session folder", &dir))?;
        let launch_args = self.launch_args(worktree, start.agent);
        // Each process gets a new token, a restored or switched one too, so no earlier process
        // of the session can make panel requests.
        let token = SessionToken::create(&dir)?;
        let ctx = LaunchCtx {
            session,
            resume_id: start.resume_id,
            resume: start.resume,
            prompt: start.prompt,
            model: start.model.as_ref(),
            launch_args: &launch_args,
            dir: &dir,
            token: token.as_str(),
            socket: &agents.socket,
            cols: start.cols,
            rows: start.rows,
        };
        let adapter = agents.adapter(start.agent);
        let env = [
            (GROVE_SESSION_ID, session.to_string().into()),
            (GROVE_SOCKET, agents.socket.clone().into()),
            (GROVE_SESSION_TOKEN, token.as_str().into()),
        ];
        let chat = match start.mode {
            Mode::Terminal => {
                let spec = adapter.launch(&ctx)?;
                let check_token = (start.agent == Agent::Codex).then(|| token.as_str().to_owned());
                agents.begin(session, Mode::Terminal, None, Some(token));
                self.start_session(session, worktree, spec, &env, Kind::Agent)?;
                if let Some(token) = check_token {
                    self.check_codex_hooks(session, worktree, launch_args, token);
                }
                None
            }
            Mode::Chat => {
                let spec = adapter.launch_chat(&ctx)?;
                let thread = (start.agent == Agent::Codex && start.resume)
                    .then(|| start.resume_id.to_owned());
                let chat = Arc::new(Chat::new(start.agent, thread, start.model.clone()));
                agents.begin(session, Mode::Chat, Some(chat.clone()), Some(token));
                self.start_chat(session, worktree, spec, &env, chat.clone(), start.prompt)?;
                Some(chat)
            }
        };
        // Deleted while its process started, so nothing else would end that process.
        if !lock(&agents.live).contains_key(&session) {
            let _ = self.kill(session);
            return Err(CoreError::NotAnAgentSession(session));
        }
        // A new session is listed, or a listed one runs again or in another mode. Having no
        // receivers is fine.
        let _ = self.events.send(CoreEvent::AgentSessionsChanged);
        Ok(chat)
    }

    // Runs the check on a thread of its own, so the launch does not wait for it. A check that
    // fails or times out leaves the hooks unknown and sends no warning. Either way, states come
    // from heuristics until a hook arrives.
    fn check_codex_hooks(
        &self,
        session: SessionId,
        worktree: &Path,
        launch_args: Vec<String>,
        token: String,
    ) {
        let agents = self.agents.clone();
        let env = self.session_env.clone();
        let worktree = worktree.to_path_buf();
        // A thread that cannot start leaves the hooks unknown too.
        let _ = thread::Builder::new()
            .name("grove-codex-hooks".into())
            .spawn(move || {
                if agents
                    .codex
                    .hooks_need_approval(session, &worktree, &env, &launch_args)
                {
                    // A hook, exit or relaunch can win the race with hooks/list.
                    let live = lock(&agents.live);
                    if live.get(&session).is_some_and(|tracked| {
                        !tracked.hooks_seen
                            && tracked
                                .token
                                .as_ref()
                                .is_some_and(|current| current.matches(&token))
                    }) {
                        agents
                            .warnings
                            .raise(Warning::CodexHooksNotTrusted { session, worktree });
                    }
                }
            });
    }

    /// The UI calls this when a session's panel gets focus. `Done` becomes `Idle`, keeping its
    /// source, and an agent session becomes its worktree's most recently focused one.
    pub fn focus(&self, session: SessionId) {
        let mut live = lock(&self.agents.live);
        let Some(tracked) = live.get_mut(&session) else {
            return;
        };
        lock(&self.agents.last_focused).insert(tracked.worktree.clone(), session);
        if let Some((AgentState::Done, source)) = tracked.state {
            tracked.set(session, AgentState::Idle, source, &self.events);
        }
    }

    /// The agent's own session id grove last recorded for `session`, which resumes its
    /// conversation. None for a session that is not an agent session, or a Codex session
    /// before its first prompt.
    pub fn resume_id(&self, session: SessionId) -> Option<String> {
        lock(&self.agents.live)
            .get(&session)
            .and_then(|tracked| tracked.resume_id.clone())
    }

    /// The agent session in `worktree` that last got focus.
    pub fn last_focused_agent(&self, worktree: &Path) -> Option<SessionId> {
        lock(&self.agents.last_focused).get(worktree).copied()
    }

    /// Picks the agent session a review batch for `worktree` goes to, `to` or else the
    /// worktree's most recently focused agent session, with its chat in chat mode. It must be an
    /// agent session in `worktree` that is `Idle` or `Done`, not switching mode, and in terminal
    /// mode have bracketed paste on.
    pub(crate) fn batch_target(
        &self,
        worktree: &Path,
        to: Option<SessionId>,
    ) -> Result<(SessionId, Option<Arc<Chat>>)> {
        let no_target = || CoreError::NoAgentSession(worktree.to_path_buf());
        let session = to
            .or_else(|| self.last_focused_agent(worktree))
            .ok_or_else(no_target)?;
        let (state, chat, switching) = lock(&self.agents.live)
            .get(&session)
            .filter(|tracked| tracked.worktree == worktree)
            .map(|tracked| {
                let state = tracked.state.map(|(state, _)| state);
                (state, tracked.chat.clone(), tracked.switching)
            })
            .ok_or_else(no_target)?;
        // Its process is ending, so the batch would go to the wrong one, or nowhere.
        if switching {
            return Err(CoreError::SwitchingMode(session));
        }
        if !matches!(state, Some(AgentState::Idle | AgentState::Done)) {
            return Err(CoreError::AgentNotReady { session, state });
        }
        // Read after the agents lock is released, so the two locks never nest.
        if chat.is_none() && !self.bracketed_paste(session)? {
            return Err(CoreError::BracketedPasteOff(session));
        }
        Ok((session, chat))
    }

    /// The agent sessions waiting on the user: approvals, then questions, then `Failed`, then
    /// `Done`, each oldest first by when it entered that state. The UI calls this again on
    /// every `AgentState` event.
    pub fn attention_queue(&self) -> Vec<AttentionEntry> {
        // Removing a worktree drops the records of its sessions, so none waits here for a
        // worktree that is gone.
        let waiting = lock(&self.agents.live)
            .iter()
            .filter_map(|(&session, tracked)| {
                let (state, _) = tracked.state?;
                let entry = AttentionEntry {
                    session,
                    worktree: tracked.worktree.clone(),
                    state,
                    since: tracked.entered_wall_clock,
                };
                Some((attention_rank(state)?, tracked.entered, entry))
            })
            .collect();
        attention_order(waiting)
    }
}

/// Sorts waiting sessions by their place in the attention queue, then by when each entered its
/// state. Ties fall back to the session id, so the order is stable.
fn attention_order(mut waiting: Vec<(u8, Instant, AttentionEntry)>) -> Vec<AttentionEntry> {
    waiting.sort_by_key(|(rank, entered, entry)| (*rank, *entered, entry.session.0));
    waiting.into_iter().map(|(_, _, entry)| entry).collect()
}

/// A state's place in the attention queue. None for a state that does not wait on the user.
fn attention_rank(state: AgentState) -> Option<u8> {
    match state {
        AgentState::NeedsInput {
            kind: InputKind::Approval,
        } => Some(0),
        AgentState::NeedsInput {
            kind: InputKind::Question,
        } => Some(1),
        AgentState::Failed => Some(2),
        AgentState::Done => Some(3),
        AgentState::Working | AgentState::Idle | AgentState::Exited => None,
    }
}

/// How long one connection gets to send its whole message and, for a panel request, take its
/// reply. grove-hook connects and writes within its own 200 ms budget and then exits, which
/// closes the connection. 500 ms leaves room for a loaded machine. Hooks that arrive meanwhile
/// wait in the socket's buffer, so a client that never closes delays them by at most this much
/// and never blocks an agent.
const READ_DEADLINE: Duration = Duration::from_millis(500);

// How long `set_mode` waits for a resumed chat's history.
const HISTORY_WAIT: Duration = Duration::from_secs(10);

// A running `set_mode` or `resume_agent_session`. Dropping it ends the switch.
struct Switch<'a> {
    agents: &'a Agents,
    events: &'a broadcast::Sender<CoreEvent>,
    session: SessionId,
    // The new process runs.
    started: bool,
}

impl Drop for Switch<'_> {
    fn drop(&mut self) {
        let mut live = lock(&self.agents.live);
        let Some(tracked) = live.get_mut(&self.session) else {
            return;
        };
        tracked.switching = false;
        // Its process is gone and none took its place. An exit still to come ends it as any
        // exit does.
        if !self.started && tracked.exited.take().is_none() {
            tracked.ended = true;
            tracked.set(
                self.session,
                AgentState::Exited,
                tracked.source(),
                self.events,
            );
            // `begin` may have had it saved as running.
            self.agents.saves.request();
            let _ = self.events.send(CoreEvent::AgentSessionsChanged);
        }
    }
}

/// The core socket, bound twice over one file: tokio's half waits for connections, and the
/// std half checks for queued ones without waiting.
pub(crate) struct HookSocket {
    listener: UnixListener,
    queued: std_net::UnixListener,
}

impl HookSocket {
    /// Must run inside the core's runtime.
    pub(crate) fn bind(path: &Path) -> Result<Self> {
        let socket = || -> io::Result<Self> {
            let queued = std_net::UnixListener::bind(path)?;
            // Both halves share this flag.
            queued.set_nonblocking(true)?;
            let listener = UnixListener::from_std(queued.try_clone()?)?;
            Ok(Self { listener, queued })
        };
        socket().map_err(CoreError::io("bind the socket", path))
    }
}

/// How deep subagents nest below a session the user started. Each level is a real agent
/// process, so an agent that keeps spawning stops here.
const MAX_SUBAGENT_DEPTH: usize = 3;

// How many subagent levels `session` sits below a session with no parent: 0 for one the user
// started. It counts no further than MAX_SUBAGENT_DEPTH, so a chain that loops still ends.
fn subagent_depth(live: &HashMap<SessionId, Tracked>, session: SessionId) -> usize {
    let mut depth = 0;
    let mut above = live.get(&session).and_then(|tracked| tracked.parent);
    while let Some(parent) = above {
        if depth == MAX_SUBAGENT_DEPTH {
            break;
        }
        depth += 1;
        above = live.get(&parent).and_then(|tracked| tracked.parent);
    }
    depth
}

impl Agents {
    // Refuses when `parent` has the most live subagents one session may have: those it started
    // itself that have not ended.
    fn check_live_subagents(
        &self,
        live: &HashMap<SessionId, Tracked>,
        parent: SessionId,
    ) -> Result<()> {
        let limit = self.max_live_subagents_per_parent;
        let live_subagents = live
            .values()
            .filter(|tracked| tracked.parent == Some(parent) && !tracked.ended)
            .count();
        if live_subagents >= limit {
            return Err(CoreError::TooManyLiveSubagents { parent, limit });
        }
        Ok(())
    }

    /// The worktree of agent session `session`, while its process has not ended.
    pub(crate) fn running_worktree(&self, session: SessionId) -> Option<PathBuf> {
        lock(&self.live)
            .get(&session)
            .filter(|tracked| !tracked.ended)
            .map(|tracked| tracked.worktree.clone())
    }

    /// The worktree of agent session `session`, its process ended or not.
    pub(crate) fn worktree_of(&self, session: SessionId) -> Option<PathBuf> {
        lock(&self.live)
            .get(&session)
            .map(|tracked| tracked.worktree.clone())
    }
}

/// A panel request that needs the whole core, handed from the socket task to the panel
/// worker with the connection its reply goes to.
pub(crate) enum PanelJob {
    /// A request from `session`, which the socket task authenticated.
    Run {
        session: SessionId,
        request: Request,
        reply_to: std_net::UnixStream,
    },
    /// The core is stopping.
    Stop,
}

/// The thread that runs `PanelJob`s, so the socket task never waits on one and hooks keep
/// their order. It holds the core only while a job runs. Dropping it stops the thread and
/// waits for it, so the core's last handle is always the one the user drops.
pub(crate) struct PanelWorker {
    jobs: mpsc::Sender<PanelJob>,
    // Set once the core starts stopping, so jobs still queued start nothing.
    stopping: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl PanelWorker {
    /// Starts the thread on `queue`, the receiving end of `jobs`.
    pub(crate) fn start(
        jobs: mpsc::Sender<PanelJob>,
        queue: mpsc::Receiver<PanelJob>,
        core: &Arc<CoreInner>,
    ) -> Result<Self> {
        let core = Arc::downgrade(core);
        let stopping = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stopping);
        let thread = thread::Builder::new()
            .name("grove-panel-requests".into())
            .spawn(move || {
                while let Ok(PanelJob::Run {
                    session,
                    request,
                    mut reply_to,
                }) = queue.recv()
                {
                    // The flag first, so a job that holds the core has passed it.
                    let running = if stopped.load(Ordering::SeqCst) {
                        None
                    } else {
                        core.upgrade()
                    };
                    let reply = match running {
                        Some(inner) => {
                            // A handle with no worker, so dropping it ends nothing. The user's
                            // handle outlives it: dropping that waits for this thread first.
                            let core = Core {
                                _panel_worker: None,
                                inner,
                            };
                            core.answer_panel_request(session, request)
                        }
                        None => panel_request::error("grove is stopping"),
                    };
                    // A client that left or stalls loses only its reply.
                    let _ = reply_to
                        .set_nonblocking(false)
                        .and_then(|()| reply_to.set_write_timeout(Some(READ_DEADLINE)))
                        .and_then(|()| reply_to.write_all(format!("{reply}\n").as_bytes()));
                }
            })
            .map_err(CoreError::pty("start the panel request thread"))?;
        Ok(Self {
            jobs,
            stopping,
            thread: Some(thread),
        })
    }
}

impl Drop for PanelWorker {
    fn drop(&mut self) {
        // A job already running finishes. Those queued behind it are refused.
        self.stopping.store(true, Ordering::SeqCst);
        let _ = self.jobs.send(PanelJob::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Reads hook messages and panel requests from the core socket and applies session exits. One
/// at a time, so the hooks of one agent, which it runs one after another, apply in order. Panel
/// requests that need the whole core go to `panel_jobs`.
pub(crate) async fn serve(
    socket: HookSocket,
    mut exits: UnboundedReceiver<ProcessExit>,
    agents: Arc<Agents>,
    events: broadcast::Sender<CoreEvent>,
    panel_jobs: mpsc::Sender<PanelJob>,
) {
    loop {
        let next = poll_fn(|cx| match socket.listener.poll_accept(cx) {
            Poll::Ready(accepted) => Poll::Ready(Ok(accepted.map(|(stream, _)| stream))),
            Poll::Pending => exits.poll_recv(cx).map(Err),
        })
        .await;
        match next {
            Ok(Ok(stream)) => read_connection(stream, &agents, &events, &panel_jobs).await,
            Ok(Err(_)) => {}
            Err(Some(exit)) => {
                // An agent runs its hooks before it exits, so they are all connected by now,
                // though tokio may not have seen them yet. They go first.
                while let Ok((stream, _)) = socket.queued.accept() {
                    if let Ok(stream) = stream
                        .set_nonblocking(true)
                        .and_then(|()| UnixStream::from_std(stream))
                    {
                        read_connection(stream, &agents, &events, &panel_jobs).await;
                    }
                }
                agents.on_exit(&exit, &events);
            }
            // The core is stopping.
            Err(None) => return,
        }
    }
}

// Reads one message and applies a hook or replies to a panel request, all within the deadline.
// A request for the panel worker goes there with its connection, and the worker replies.
async fn read_connection(
    stream: UnixStream,
    agents: &Agents,
    events: &broadcast::Sender<CoreEvent>,
    panel_jobs: &mpsc::Sender<PanelJob>,
) {
    let _ = time::timeout(READ_DEADLINE, async move {
        let message = match read_message(&stream).await {
            Ok(message) => message,
            Err(Unread::TooLarge) => {
                // So a client that waits on its reply learns why it gets none.
                let refused = panel_request::error("request too large");
                let _ = write_all(&stream, format!("{refused}\n").as_bytes()).await;
                return Some(());
            }
            Err(Unread::Broken) => return None,
        };
        if panel_request::is_panel_request(&message) {
            let read = panel_request::read(message, |session, token| {
                agents.authenticate(session, token)
            });
            let reply = match read {
                Ok((session, Request::SessionId)) => panel_request::session_id_reply(session),
                Ok((session, request)) => {
                    // Fails only while the core stops. The client then gets no reply.
                    if let Ok(reply_to) = stream.into_std() {
                        let _ = panel_jobs.send(PanelJob::Run {
                            session,
                            request,
                            reply_to,
                        });
                    }
                    return Some(());
                }
                Err(refused) => refused,
            };
            // A client that left without its reply loses only that.
            let _ = write_all(&stream, format!("{reply}\n").as_bytes()).await;
        } else if let Ok(hook) = serde_json::from_value(message) {
            agents.on_hook(hook, events);
        }
        Some(())
    })
    .await;
}

async fn write_all(stream: &UnixStream, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        stream.writable().await?;
        match stream.try_write(bytes) {
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// grove-hook keeps only a few short fields, and a panel request is as small, so anything
/// bigger is from neither.
const MAX_MESSAGE: usize = 64 * 1024;

/// Why a connection gave no message.
enum Unread {
    TooLarge,
    /// It failed, or what it sent is not JSON.
    Broken,
}

async fn read_message(stream: &UnixStream) -> std::result::Result<Value, Unread> {
    let mut message = Vec::new();
    let mut buf = [0; 4096];
    loop {
        stream.readable().await.map_err(|_| Unread::Broken)?;
        match stream.try_read(&mut buf) {
            Ok(0) => break,
            Ok(n) => message.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::WouldBlock => continue,
            Err(_) => return Err(Unread::Broken),
        }
        if message.len() > MAX_MESSAGE {
            return Err(Unread::TooLarge);
        }
    }
    serde_json::from_slice(&message).map_err(|_| Unread::Broken)
}

/// Copies grove-hook to where agent hooks run it, when the copy there differs. Through a
/// temp file and a rename, so a hook running now never sees half a binary.
pub(crate) fn install_hook_relay(source: &Path, dest: &Path) -> Result<()> {
    let relay = fs::read(source).map_err(CoreError::io("read the hook relay", source))?;
    if fs::read(dest).is_ok_and(|installed| installed == relay) {
        return Ok(());
    }
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir).map_err(CoreError::io("create the folder", dir))?;
    }
    let temp = dest.with_extension("tmp");
    fs::write(&temp, &relay).map_err(CoreError::io("write", &temp))?;
    crate::set_mode(&temp, 0o755)?;
    fs::rename(&temp, dest).map_err(CoreError::io("install the hook relay at", dest))?;
    warm_up(dest);
    Ok(())
}

// macOS checks a new binary on its first run, which took about 200 ms here and over the 1 s
// hook timeout under load. Pay it now, not in an agent's first hook. With no arguments
// grove-hook exits 0 at once. The core holds the home lock meanwhile, so a relay that runs
// past WARM_UP_LIMIT is killed and reaped. A failure costs only that delay, so it is ignored.
// Linux has no such check, and running a file just written there can fail with ETXTBSY while
// a child another thread forked still holds the write fd.
#[cfg(target_os = "macos")]
fn warm_up(relay: &Path) {
    use std::process::{Command, Stdio};
    use std::time::Instant;

    // The first run went past 1 s under load. Past this, the first hook pays the rest.
    const WARM_UP_LIMIT: Duration = Duration::from_secs(2);
    let Ok(mut child) = Command::new(relay)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let deadline = Instant::now() + WARM_UP_LIMIT;
    while let Ok(None) = child.try_wait() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(target_os = "macos"))]
fn warm_up(_relay: &Path) {}

// Hooks run through the shell.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', r"'\''"))
}

/// `N` random bytes from the system's source.
pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let urandom = Path::new("/dev/urandom");
    let mut bytes = [0u8; N];
    File::open(urandom)
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(CoreError::io("read", urandom))?;
    Ok(bytes)
}

/// A random version 4 UUID.
fn new_uuid() -> Result<String> {
    let mut b: [u8; 16] = random_bytes()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_stops_working_when_its_process_exits_for_a_mode_switch() {
        let home = tempfile::tempdir().unwrap();
        let agents = Agents::new(
            &CoreConfig::new(home.path().to_path_buf()),
            SaveRequests::default(),
            Arc::new(crate::warnings::Warnings::new(broadcast::channel(1).0)),
        );
        let session = SessionId(3);
        let token = SessionToken::create(home.path()).unwrap();
        let presented = token.as_str().to_owned();
        let mut tracked = Tracked::new(Agent::Claude, home.path(), Mode::Terminal, (80, 24), None);
        tracked.token = Some(token);
        // `set_mode` is ending the process to start the session again in the other mode, so
        // the session itself does not end.
        let (exited, _on_exit) = oneshot::channel();
        tracked.exited = Some(exited);
        lock(&agents.live).insert(session, tracked);
        assert_eq!(agents.authenticate("3", &presented), Some(session));
        let (events, _) = broadcast::channel(8);

        let exit = ProcessExit {
            session,
            code: None,
            killed: true,
        };
        agents.on_exit(&exit, &events);

        assert_eq!(agents.authenticate("3", &presented), None);
    }

    #[test]
    fn a_session_that_ends_during_a_start_does_not_shift_the_new_sessions_panel() {
        let home = tempfile::tempdir().unwrap();
        let repo = home.path().join("repo");
        let git = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(git.success());
        let (core, _events) = Core::start(CoreConfig::new(home.path().join("home"))).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(core.add_repo(&repo)).unwrap();
        let worktree = runtime.block_on(core.list_worktrees())[0].path.clone();
        let (ending, started) = (SessionId(50), SessionId(51));
        let tracked = Tracked::new(Agent::Claude, &worktree, Mode::Terminal, (80, 24), None);
        lock(&core.agents.live).insert(ending, tracked);
        let panel = |session| crate::AgentPanel {
            agent: Agent::Claude,
            session,
            shown: true,
        };
        let mut workspace = core.workspace(&worktree).unwrap();
        workspace.chat = vec![panel(Some(ending)), panel(None), panel(None)];
        runtime
            .block_on(core.set_workspace(&worktree, workspace, None))
            .unwrap();

        // As `spawn_agent` does, with `ending` ending while the new process starts.
        let ended = core.check_agent_panel(&worktree, Some(1)).unwrap();
        if let Some(tracked) = lock(&core.agents.live).get_mut(&ending) {
            tracked.ended = true;
        }
        core.place_agent(&worktree, started, Agent::Claude, Some(1), &ended);

        let saved = core
            .workspaces()
            .get(&worktree)
            .unwrap()
            .workspace
            .chat
            .clone();
        assert_eq!(
            saved,
            [panel(Some(ending)), panel(Some(started)), panel(None)]
        );
    }

    #[test]
    fn a_parent_that_has_ended_spawns_no_subagent() {
        let home = tempfile::tempdir().unwrap();
        let (core, mut events) = Core::start(CoreConfig::new(home.path().to_path_buf())).unwrap();
        let parent = SessionId(3);
        let mut tracked = Tracked::new(Agent::Claude, home.path(), Mode::Terminal, (80, 24), None);
        tracked.ended = true;
        lock(&core.agents.live).insert(parent, tracked);

        let spawned = core.spawn_subagent(parent, Agent::Codex, "hi".into(), None);

        assert!(
            matches!(spawned, Err(CoreError::NotAnAgentSession(s)) if s == parent),
            "{spawned:?}"
        );
        assert!(events.try_recv().is_err());
        assert_eq!(lock(&core.agents.live).len(), 1);
    }

    // Tracks each of `chain` as an agent session, with the parent it names.
    fn track_chain(core: &Core, home: &Path, chain: &[(u64, Option<u64>)]) {
        for &(id, parent) in chain {
            let mut tracked = Tracked::new(Agent::Claude, home, Mode::Chat, (80, 24), None);
            tracked.parent = parent.map(SessionId);
            lock(&core.agents.live).insert(SessionId(id), tracked);
        }
    }

    #[test]
    fn a_subagent_three_levels_deep_spawns_no_subagent() {
        let home = tempfile::tempdir().unwrap();
        let (core, mut events) = Core::start(CoreConfig::new(home.path().to_path_buf())).unwrap();
        track_chain(
            &core,
            home.path(),
            &[(0, None), (1, Some(0)), (2, Some(1)), (3, Some(2))],
        );

        let spawned = core.spawn_subagent(SessionId(3), Agent::Codex, "hi".into(), None);

        assert!(
            matches!(
                spawned,
                Err(CoreError::SubagentTooDeep {
                    parent: SessionId(3),
                    limit: 3
                })
            ),
            "{spawned:?}"
        );
        assert!(events.try_recv().is_err());
        assert_eq!(lock(&core.agents.live).len(), 4);
    }

    #[test]
    fn a_parent_with_eight_live_subagents_spawns_no_ninth() {
        let home = tempfile::tempdir().unwrap();
        let (core, mut events) = Core::start(CoreConfig::new(home.path().to_path_buf())).unwrap();
        let mut sessions = vec![(0, None)];
        sessions.extend((1..=8).map(|subagent| (subagent, Some(0))));
        track_chain(&core, home.path(), &sessions);

        let spawned = core.spawn_subagent(SessionId(0), Agent::Codex, "hi".into(), None);

        assert!(
            matches!(
                spawned,
                Err(CoreError::TooManyLiveSubagents {
                    parent: SessionId(0),
                    limit: 8
                })
            ),
            "{spawned:?}"
        );
        assert!(events.try_recv().is_err());
        assert_eq!(lock(&core.agents.live).len(), 9);
    }

    #[test]
    fn two_requests_for_the_last_place_under_the_limit_start_one_subagent() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        let git = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(git.success());
        // Stands in for the Claude sidecar, and runs until it is killed.
        let sidecar = dir.path().join("claude-sidecar");
        fs::write(&sidecar, "#!/bin/sh\nexec sleep 600\n").unwrap();
        fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = CoreConfig::new(dir.path().join("home"));
        cfg.claude_sidecar_command = sidecar;
        cfg.max_live_subagents_per_parent = 1;
        let (core, _events) = Core::start(cfg).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(core.add_repo(&repo)).unwrap();
        let worktree = runtime.block_on(core.list_worktrees())[0].path.clone();
        let parent = SessionId(50);
        let tracked = Tracked::new(Agent::Claude, &worktree, Mode::Chat, (80, 24), None);
        lock(&core.agents.live).insert(parent, tracked);

        // Both wait for the agents, which the test holds, then go on side by side.
        let agents = lock(&core.agents.live);
        let spawned = thread::scope(|scope| {
            let spawn = || core.spawn_subagent(parent, Agent::Claude, "hi".into(), None);
            let (a, b) = (scope.spawn(spawn), scope.spawn(spawn));
            thread::sleep(Duration::from_millis(100));
            drop(agents);
            [a.join().unwrap(), b.join().unwrap()]
        });

        let refused = |spawned: &Result<_>| {
            matches!(
                spawned,
                Err(CoreError::TooManyLiveSubagents { limit: 1, .. })
            )
        };
        assert_eq!(
            spawned.iter().filter(|s| s.is_ok()).count(),
            1,
            "{spawned:?}"
        );
        assert_eq!(
            spawned.iter().filter(|s| refused(s)).count(),
            1,
            "{spawned:?}"
        );
        assert_eq!(lock(&core.agents.live).len(), 2);
    }

    #[test]
    fn a_parent_chain_that_loops_is_refused_instead_of_walked_forever() {
        let home = tempfile::tempdir().unwrap();
        let (core, _events) = Core::start(CoreConfig::new(home.path().to_path_buf())).unwrap();
        track_chain(&core, home.path(), &[(1, Some(2)), (2, Some(1))]);

        let spawned = core.spawn_subagent(SessionId(1), Agent::Codex, "hi".into(), None);

        assert!(
            matches!(spawned, Err(CoreError::SubagentTooDeep { .. })),
            "{spawned:?}"
        );
    }

    #[test]
    fn a_spawn_queued_behind_a_running_one_does_not_start_once_the_core_is_stopping() {
        let home = tempfile::tempdir().unwrap();
        let (core, _events) = Core::start(CoreConfig::new(home.path().to_path_buf())).unwrap();
        let worker = core._panel_worker.as_ref().unwrap();
        let spawn = |parent, reply_to| PanelJob::Run {
            session: SessionId(parent),
            request: Request::SubagentSpawn {
                agent: Agent::Codex,
                prompt: "hi".into(),
                worktree: None,
            },
            reply_to,
        };
        let (mut a_client, a_reply_to) = std_net::UnixStream::pair().unwrap();
        let (mut b_client, b_reply_to) = std_net::UnixStream::pair().unwrap();
        assert_eq!(Arc::strong_count(&core.inner), 1);

        // Job A runs and waits for the agents, which the test holds. B queues behind it.
        let agents = lock(&core.agents.live);
        worker.jobs.send(spawn(1, a_reply_to)).unwrap();
        worker.jobs.send(spawn(2, b_reply_to)).unwrap();
        // The worker holds the core once A has passed the stop flag.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Arc::strong_count(&core.inner) < 2 {
            assert!(Instant::now() < deadline, "the worker never took job A");
            thread::yield_now();
        }
        // The core starts stopping while A runs, as dropping it does first.
        worker.stopping.store(true, Ordering::SeqCst);
        drop(agents);
        let mut a_reply = String::new();
        a_client.read_to_string(&mut a_reply).unwrap();
        let mut b_reply = String::new();
        b_client.read_to_string(&mut b_reply).unwrap();
        drop(core);

        // A finished with the core's own answer for an unknown parent. B never started.
        assert_eq!(
            a_reply,
            "{\"error\":\"session 1 is not an agent session\"}\n"
        );
        assert_eq!(b_reply, "{\"error\":\"grove is stopping\"}\n");
    }

    #[test]
    fn the_queue_keeps_entry_order_when_the_wall_clock_goes_back() {
        let approval = AgentState::NeedsInput {
            kind: InputKind::Approval,
        };
        let entry = |id, since| AttentionEntry {
            session: SessionId(id),
            worktree: PathBuf::new(),
            state: approval,
            since,
        };
        let first = Instant::now();
        let now = SystemTime::now();
        // The clock was set back a minute between the two, so the later one has the earlier
        // wall time. Neither the wall time nor the session id gives this order.
        let waiting = vec![
            (
                0,
                first + Duration::from_secs(1),
                entry(1, now - Duration::from_secs(60)),
            ),
            (0, first, entry(2, now)),
        ];

        let order: Vec<_> = attention_order(waiting)
            .into_iter()
            .map(|entry| entry.session)
            .collect();

        assert_eq!(order, [SessionId(2), SessionId(1)]);
    }
}
