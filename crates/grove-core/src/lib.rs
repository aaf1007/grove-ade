//! The grove core: worktrees, sessions, diff, status and events. It has no UI code.

mod agent;
mod chat;
mod comments;
mod config;
mod diff;
mod editor;
mod error;
mod files;
mod notify;
mod panel;
mod panel_request;
mod profile;
mod remove;
mod restore;
mod run;
mod session;
mod setup;
mod shell_env;
mod snapshot;
mod state;
mod terminal;
mod watch;
mod workspace;
mod worktree;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, Permissions, TryLockError};
use std::io;
use std::ops::Deref;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::runtime::Runtime;
use tokio::sync::broadcast;

pub use agent::{
    AccountInfo, Agent, AgentSessionInfo, AgentSpec, AgentState, AttentionEntry, InputKind, Mode,
    StateSource,
};
pub use chat::{
    AgentMessage, MessageSink, ModelChoice, ModelInfo, Question, QuestionOption, Reply, RequestId,
    TurnOutcome,
};
pub use comments::{BatchComment, CommentId, CommentState, NewReviewComment, ReviewComment};
pub use config::CoreConfig;
pub use diff::{
    DiffBase, DiffLine, FileDiff, FileStatus, FileVersions, Hunk, LineChange, LineKind,
};
pub use editor::Editor;
pub use error::{CoreError, Result};
pub use panel_request::PanelSpec;
pub use profile::{
    AgentDefaults, AgentStateKind, Layout, Notify, PanelKind, Profile, ProfileSettings,
    ProfileText, Review, ReviewTarget, Suggestion,
};
pub use remove::{BranchOutcome, DeleteCheck, RemoveOpts};
pub use restore::{SessionInfo, SessionKind};
pub use run::{LaunchArgs, RepoSettings, RunSession, RunSpec};
pub use session::{OutputFrame, OutputSink, SessionId, SpawnSpec};
pub use setup::{Setup, SetupState};
pub use snapshot::Snapshot;
pub use workspace::{AgentPanel, Workspace};
pub use worktree::{AddedWorktree, NewWorktree, WorktreeInfo};

const EVENT_CAPACITY: usize = 256;

/// Small state events for the UI. The core never sends PTY output as an event.
/// Later tickets add the events they send.
#[derive(Debug, Clone)]
pub enum CoreEvent {
    /// The session's process ended. `code` is None when a signal ended it.
    SessionExited {
        session: SessionId,
        code: Option<i32>,
    },
    /// An agent session's state changed.
    AgentState {
        session: SessionId,
        state: AgentState,
        source: StateSource,
    },
    /// An agent session in `worktree` entered `state`, and its repo's profile settings list it
    /// under `notify`, NeedsInput and Failed when they list nothing. Sent once per entry, after
    /// the `AgentState` for it. The shell shows a notification while grove is not focused.
    Notify {
        session: SessionId,
        worktree: PathBuf,
        state: AgentState,
    },
    /// What `agent_sessions` lists changed: an agent session started, got its title, switched
    /// mode, ended, was resumed or was deleted. The UI calls `agent_sessions` again.
    AgentSessionsChanged,
    /// A file, the index, HEAD or a ref of the worktree changed, so its diff may have too.
    DiffChanged { worktree: PathBuf },
    /// The worktree's review comments changed: one was added, sent or resolved, or a change to
    /// the worktree moved one or flagged or unflagged it as possibly addressed.
    CommentsChanged { worktree: PathBuf },
    /// A profile changed: through `set_profile`, or in its file on disk. `by` is the agent
    /// session that changed it through `set_profile`, None for the user and for edits on disk.
    ProfilesChanged { by: Option<SessionId> },
    /// The settings of the repo whose main worktree is at `repo` changed through
    /// `set_repo_settings`, which `set_repo_overrides_text` also calls. `by` is the agent session
    /// that changed them, None for the user.
    RepoSettingsChanged {
        repo: PathBuf,
        by: Option<SessionId>,
    },
    /// The workspace of agent session `session` changed through `set_workspace` or
    /// `undo_workspace`, or one of its agent panels came or went: a subagent started below it,
    /// `resume_agent_session` showed a panel again, or a subagent was deleted or its worktree
    /// removed. `by` is the agent session that changed it, None for the user and for the rest.
    /// A new session's own workspace sends none: it starts with the session.
    WorkspaceChanged {
        session: SessionId,
        by: Option<SessionId>,
    },
    /// A worktree's setup script started or ended.
    Setup { worktree: PathBuf, setup: Setup },
    /// Something failed that the core worked around. The UI shows it to the user.
    Warning(Warning),
    /// Who is logged in to an agent, as its chat-mode transport reported it. None when nobody
    /// is.
    Account {
        agent: Agent,
        account: Option<AccountInfo>,
    },
    /// An agent session asked for a panel through grove-mcp, and grove started what it shows.
    /// `parent` is the agent session that asked.
    PanelRequested {
        parent: Option<SessionId>,
        panel: PanelSpec,
    },
    /// A log or command panel closed: the agent session that opened it closed it, or its
    /// worktree was removed. Not sent when grove quits.
    /// It can come for a panel that never got `PanelRequested`, when a worktree removal
    /// overlaps the open, so ignore a close for a panel not shown.
    PanelClosed { panel: SessionId },
}

/// A problem the core worked around, so it keeps running with less than it should.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// The login shell's environment could not be read at start, so sessions get the minimal
    /// environment and may not find `claude` or `codex`.
    LoginShellEnv { reason: String },
    /// The user has not approved grove's hooks in Codex, or they changed since. Codex runs none
    /// of them, so the session's state comes from heuristics. The user approves them in
    /// Codex's `/hooks`.
    CodexHooksNotTrusted {
        session: SessionId,
        worktree: PathBuf,
    },
    /// A session that was running when grove quit could not start again, so it is gone. Its
    /// panel is gone with it.
    SessionNotRestored { worktree: PathBuf, reason: String },
    /// A worktree's watcher could not start, as when its folder is gone or the watch limit is
    /// reached. The worktree is still tracked, but its diff does not update live until
    /// `Core::retry_watcher` starts the watcher.
    WatcherNotStarted { worktree: PathBuf, reason: String },
}

/// A running core. It owns its tokio runtime (ADR 0002). Dropping it stops it.
pub struct Core {
    // Dropped first: it ends the thread that runs panel requests, so `inner` drops here.
    _panel_worker: Option<agent::PanelWorker>,
    inner: Arc<CoreInner>,
}

impl Deref for Core {
    type Target = CoreInner;

    fn deref(&self) -> &CoreInner {
        &self.inner
    }
}

// Public only inside a private module, so `Core` can deref to it while nothing outside the
// crate can name it.
mod core_inner {
    use super::*;

    /// What a `Core` holds. Shared only with the panel request thread, for one request at a
    /// time.
    pub struct CoreInner {
        pub(crate) socket: PathBuf,
        // Each call that changes it saves the state file.
        pub(crate) worktrees: Arc<Mutex<Vec<WorktreeInfo>>>,
        pub(crate) state_file: Arc<state::StateFile>,
        pub(crate) repo_settings: Arc<Mutex<run::Settings>>,
        pub(crate) profiles: Arc<profile::Profiles>,
        // Rereads the profiles when their folder changes.
        pub(crate) _profiles_watch: watch::Watch,
        // One per tracked worktree. Removing one stops it.
        pub(crate) watches: Arc<Mutex<HashMap<PathBuf, watch::Watch>>>,
        // Worktrees a `remove_worktree` call is ending, where no session may start. One call at a
        // time covers each.
        pub(crate) removing: Arc<Mutex<HashSet<PathBuf>>>,
        pub(crate) setups: Arc<setup::Setups>,
        // New worktrees go in `<worktrees_root>/<repo>/<name>`. A real path, as git reports paths.
        pub(crate) worktrees_root: PathBuf,
        pub(crate) sessions: session::Sessions,
        // Every session starts with exactly this, plus its own variables.
        pub(crate) session_env: shell_env::Env,
        // The login shell, or /bin/sh without one. Run sessions run their command with it.
        pub(crate) run_shell: PathBuf,
        pub(crate) agents: Arc<agent::Agents>,
        pub(crate) comments: Arc<comments::Comments>,
        // Stopped run sessions: those that exited, and those a restart brought back.
        pub(crate) stopped_runs: Arc<Mutex<restore::StoppedRuns>>,
        // The log and command panels agent sessions opened, until closed.
        pub(crate) panels: Arc<Mutex<panel::Panels>>,
        // Asks the background saver to save, for changes no call waits on, such as a session
        // ending.
        pub(crate) saves: state::SaveRequests,
        // Held by `spawn_shell` from its look for a running shell until a new one is listed, so two
        // calls at once open one shell.
        pub(crate) opening_shell: Mutex<()>,
        pub(crate) events: broadcast::Sender<CoreEvent>,
        pub(crate) runtime: Runtime,
        // Rust drops fields in order. The lock stays last so it is released after the rest.
        pub(crate) _lock: File,
    }
}

use core_inner::CoreInner;

impl Core {
    pub fn start(cfg: CoreConfig) -> Result<(Core, broadcast::Receiver<CoreEvent>)> {
        fs::create_dir_all(&cfg.home)
            .map_err(CoreError::io("create the home folder", &cfg.home))?;
        set_mode(&cfg.home, 0o700)?;
        let worktrees_root = fs::canonicalize(&cfg.home)
            .map_err(CoreError::io("resolve the home folder", &cfg.home))?
            .join("worktrees");

        let lock_path = cfg.home.join("grove.lock");
        let lock = File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(CoreError::io("open the lock", &lock_path))?;
        lock.try_lock().map_err(|e| match e {
            TryLockError::WouldBlock => CoreError::AlreadyRunning(cfg.home.clone()),
            TryLockError::Error(source) => CoreError::io("take the lock", &lock_path)(source),
        })?;
        // Read under the lock and before start changes anything, so a bad file fails start early.
        let restored = state::read(&cfg.home)?;

        if let Some(relay) = &cfg.hook_relay_source {
            agent::install_hook_relay(relay, &cfg.hook_relay)?;
        }

        // We hold the lock, so a socket file left here is from a crashed grove.
        if fs::symlink_metadata(&cfg.socket).is_ok_and(|m| m.file_type().is_socket()) {
            fs::remove_file(&cfg.socket)
                .map_err(CoreError::io("remove the stale socket", &cfg.socket))?;
        }

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(CoreError::Runtime)?;
        let socket = {
            let _guard = runtime.enter();
            agent::HookSocket::bind(&cfg.socket)?
        };

        // Synchronous: start runs on a plain thread, and spawn must not wait on a tokio worker.
        let (session_env, warning) =
            shell_env::for_sessions(cfg.login_shell.as_deref(), cfg.login_shell_timeout);
        let (events, rx) = broadcast::channel(EVENT_CAPACITY);
        if let Some(warning) = warning {
            // `rx` already exists, so it gets this.
            let _ = events.send(CoreEvent::Warning(warning));
        }
        let saves = state::SaveRequests::default();
        let agents = Arc::new(agent::Agents::new(&cfg, saves.clone()));
        let profiles = Arc::new(profile::Profiles::open(&cfg.home.join("profiles"))?);
        // Watched before the first read, so no change is missed between the two.
        let profiles_watch = watch::Watch::folder(profiles.dir(), {
            let (profiles, agents, events) = (profiles.clone(), agents.clone(), events.clone());
            move || {
                if profiles.reread(&agents) {
                    let _ = events.send(CoreEvent::ProfilesChanged { by: None });
                }
            }
        })?;
        profiles.reread(&agents);
        let (sessions, exits) = session::Sessions::new(&cfg);
        let (panel_jobs, panel_queue) = std::sync::mpsc::channel();
        runtime.spawn(agent::serve(
            socket,
            exits,
            agents.clone(),
            events.clone(),
            panel_jobs.clone(),
        ));
        runtime.spawn(agent::heuristic::run(
            agents.clone(),
            sessions.probe(),
            events.clone(),
        ));
        // Watch the restored worktrees as `add_repo` does. start runs on a plain thread, so it may
        // block here.
        let (watches, failed) = watch_all(&restored.worktrees, &events);
        for warning in failed {
            // As with the login shell's warning, `rx` gets it.
            let _ = events.send(CoreEvent::Warning(warning));
        }
        let worktrees = Arc::new(Mutex::new(restored.worktrees));
        let comments = Arc::new(comments::Comments::new(
            events.clone(),
            saves.clone(),
            restored.comments,
        ));
        runtime.spawn(comments::follow(
            comments.clone(),
            Arc::clone(&worktrees),
            events.subscribe(),
        ));
        let repo_settings = Arc::new(Mutex::new(restored.settings));
        runtime.spawn(notify::follow(
            notify::Sources {
                agents: agents.clone(),
                worktrees: Arc::clone(&worktrees),
                repo_settings: Arc::clone(&repo_settings),
                profiles: profiles.clone(),
            },
            events.clone(),
            events.subscribe(),
        ));
        let stopped_runs = Arc::default();
        let state_file = state::StateFile::new(&cfg.home, {
            let (agents, probe, stopped, comments) = (
                agents.clone(),
                sessions.probe(),
                Arc::clone(&stopped_runs),
                comments.clone(),
            );
            Box::new(move || state::SessionsAndComments {
                sessions: restore::to_relaunch(&agents, &probe, &stopped),
                comments: comments.kept(),
            })
        });
        // From here on, an early return drops the core, which removes the socket and saves the
        // state.
        let inner = Arc::new(CoreInner {
            socket: cfg.socket,
            worktrees,
            state_file: Arc::new(state_file),
            repo_settings,
            profiles,
            _profiles_watch: profiles_watch,
            watches: Arc::new(Mutex::new(watches)),
            removing: Arc::default(),
            setups: Arc::default(),
            worktrees_root,
            sessions,
            session_env,
            run_shell: cfg.login_shell.unwrap_or_else(|| "/bin/sh".into()),
            agents,
            comments,
            stopped_runs,
            panels: Arc::default(),
            saves,
            opening_shell: Mutex::default(),
            events,
            runtime,
            _lock: lock,
        });
        let panel_worker = agent::PanelWorker::start(panel_jobs, panel_queue, &inner)?;
        let core = Core {
            _panel_worker: Some(panel_worker),
            inner,
        };
        let back = core.restore(restored.sessions);
        core.agents.keep_panels_of(&back);
        core.runtime.spawn(state::save_when_asked(
            core.saves.clone(),
            core.state_file.clone(),
            core.worktrees.clone(),
            core.repo_settings.clone(),
        ));
        // So the state file has what came back.
        core.saves.request();
        set_mode(&core.socket, 0o600)?;
        Ok((core, rx))
    }

    /// Adds the repo that `path` is in and adopts every worktree in its `git worktree list`.
    pub async fn add_repo(&self, path: &Path) -> Result<()> {
        let dir = path.to_path_buf();
        let events = self.events.clone();
        let (found, (watches, failed)) = self
            .runtime
            .spawn_blocking(move || {
                let found = worktree::list(&dir)?;
                let watches = watch_all(&found, &events);
                Ok::<_, CoreError>((found, watches))
            })
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("run git in", path)(io::Error::other(e)))??;
        {
            let mut tracked = self.tracked();
            // git reports the same main worktree path from anywhere in the repo.
            if let Some(main) = found.first()
                && tracked.iter().any(|w| w.path == main.path)
            {
                let err = CoreError::RepoAlreadyAdded(main.path.clone());
                // Dropping a watch joins its thread, so do it off this tokio worker.
                self.runtime.spawn_blocking(move || drop(watches));
                return Err(err);
            }
            self.watches().extend(watches);
            tracked.extend(found);
        }
        // Once tracked, so a retry finds the worktree.
        for warning in failed {
            let _ = self.events.send(CoreEvent::Warning(warning));
        }
        self.save_state().await
    }

    /// Every tracked worktree, repo by repo in the order they were added, main worktree first.
    pub async fn list_worktrees(&self) -> Vec<WorktreeInfo> {
        self.tracked().clone()
    }

    fn tracked(&self) -> MutexGuard<'_, Vec<WorktreeInfo>> {
        self.worktrees
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn watches(&self) -> MutexGuard<'_, HashMap<PathBuf, watch::Watch>> {
        self.watches.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn settings(&self) -> MutexGuard<'_, run::Settings> {
        self.repo_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

// Starts a watcher for each of `worktrees`. A worktree whose watcher fails, as when its folder is
// gone, is still tracked, just without live diff updates, and gets a warning the caller sends
// once it tracks the worktree. It blocks while each watcher starts.
fn watch_all(
    worktrees: &[WorktreeInfo],
    events: &broadcast::Sender<CoreEvent>,
) -> (HashMap<PathBuf, watch::Watch>, Vec<Warning>) {
    let mut watches = HashMap::new();
    let mut failed = Vec::new();
    for w in worktrees {
        match watch::Watch::start(&w.path, events.clone()) {
            Ok(watch) => {
                watches.insert(w.path.clone(), watch);
            }
            Err(e) => failed.push(watch::not_started(&w.path, &e)),
        }
    }
    (watches, failed)
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, Permissions::from_mode(mode))
        .map_err(CoreError::io("set permissions on", path))
}

impl Drop for CoreInner {
    // Runs before the fields drop, so the sessions are saved before they end, and the socket is
    // gone before the lock is released.
    fn drop(&mut self) {
        // Nothing is left to report a failure to. The last save that worked stays.
        let _ = self.state_file.close(&self.worktrees, &self.repo_settings);
        let _ = fs::remove_file(&self.socket);
        panel::kill_left_on_quit(&self.panels, &self.sessions);
    }
}
