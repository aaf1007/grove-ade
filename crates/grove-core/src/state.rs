use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::iter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

use crate::comments::Kept;
use crate::restore::Relaunch;
use crate::run::Settings;
use crate::{
    Agent, AgentPanel, CommentId, CommentState, Core, CoreError, DiffBase, PanelKind,
    ProfileSettings, RepoSettings, Result, ReviewComment, RunSession, SessionId, Workspace,
    WorktreeInfo,
};

// The schema version this grove writes. It reads this one and every older one. Version 2 added
// each repo's run sessions and launch args. Version 3 replaced them with the repo's profile and
// overrides, which hold them. Version 4 added each worktree's workspace. Version 5 added each
// worktree's sessions to bring back and its review comments, and the session in each agent
// panel. Version 6 added the parent of each agent session that is a subagent. Version 7 added
// each agent session's model and effort. Version 8 added the worktree each worktree nests
// under. Version 9 keeps each agent session until the user deletes it, with its title, its
// created time and whether its process had ended. Version 10 moved the workspace from each
// worktree to each of its agent sessions. Version 11 added each workspace's diff base.
const VERSION: u32 = 11;

/// `state.json` in the home folder: the repos and worktrees grove tracks, each repo's settings,
/// and each worktree's sessions and review comments, kept across restarts. An agent session's
/// workspace is kept with it.
pub(crate) struct StateFile {
    home: PathBuf,
    path: PathBuf,
    // Each save writes this, then renames it over `path`.
    temp: PathBuf,
    // Held for a whole save, so saves never interleave. True once the core is stopping and has
    // saved for the last time, so the sessions it then ends stay saved.
    closed: Mutex<bool>,
    // The sessions the state file keeps and the review comments, read at each save.
    current: Box<dyn Fn() -> SessionsAndComments + Send + Sync>,
}

/// What a save reads from the running core besides worktrees and settings.
pub(crate) struct SessionsAndComments {
    pub(crate) sessions: HashMap<PathBuf, Vec<Relaunch>>,
    pub(crate) comments: Kept,
}

/// What the state file brings back on start.
pub(crate) struct Restored {
    /// Repo by repo, main worktree first.
    pub(crate) worktrees: Vec<WorktreeInfo>,
    pub(crate) settings: Settings,
    /// Each agent session with its workspace.
    pub(crate) sessions: HashMap<PathBuf, Vec<Relaunch>>,
    /// The agent panel of each subagent session in a file from before version 10, where it
    /// showed in its worktree's workspace. It belongs in the workspace of its top parent.
    pub(crate) subagent_panels: Vec<AgentPanel>,
    pub(crate) comments: Kept,
}

/// Asks for a save without waiting for it, from any thread. Requests made while a save runs lead
/// to one more save.
#[derive(Clone, Default)]
pub(crate) struct SaveRequests(Arc<Notify>);

impl SaveRequests {
    pub(crate) fn request(&self) {
        self.0.notify_one();
    }
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    repos: Vec<SavedRepo>,
}

#[derive(Serialize, Deserialize)]
struct SavedRepo {
    main_worktree: SavedWorktree,
    linked_worktrees: Vec<SavedWorktree>,
    // An older file has neither.
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    overrides: ProfileSettings,
    // Only a version 2 file has these. They move into the overrides.
    #[serde(default, skip_serializing)]
    run_sessions: Vec<SavedRunSession>,
    #[serde(default, skip_serializing)]
    launch_args: SavedLaunchArgs,
}

#[derive(Serialize, Deserialize)]
struct SavedWorktree {
    path: PathBuf,
    branch: Option<String>,
    display_name: String,
    issue_link: Option<String>,
    base_branch: Option<String>,
    // An older file has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nested_under: Option<PathBuf>,
    // Only a file from versions 4 to 9 has one. Its tabs, selected tab and terminal move to each
    // agent session of the worktree.
    #[serde(default, skip_serializing)]
    workspace: Option<SavedWorkspace>,
    // An older file has neither.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sessions: Vec<Relaunch>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    comments: Vec<SavedComment>,
}

/// An agent session's workspace as the state file keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedWorkspace {
    chat: Vec<SavedAgentPanel>,
    panels: Vec<PanelKind>,
    selected: Option<PanelKind>,
    terminal: bool,
    // A file from before version 11 has none. Its Diff panel opened on `Head`.
    #[serde(default)]
    diff_base: DiffBase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedAgentPanel {
    agent: Agent,
    shown: bool,
    // A restart keeps session ids. None only in a file from before version 10, for a panel with
    // no session yet. Such a panel is not kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<SessionId>,
}

#[derive(Serialize, Deserialize)]
struct SavedComment {
    id: CommentId,
    path: PathBuf,
    start_line: u32,
    end_line: u32,
    text: String,
    body: String,
    state: CommentState,
    possibly_addressed: bool,
    // The rename it does not follow, when it was added on a path git showed renamed away.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    not_followed: Option<PathBuf>,
}

#[derive(Deserialize)]
struct SavedRunSession {
    name: String,
    command: String,
    port: Option<u16>,
}

#[derive(Deserialize, Default)]
struct SavedLaunchArgs {
    claude: Vec<String>,
    codex: Vec<String>,
}

// Read on its own first, so a newer file this grove cannot parse still reports its version.
#[derive(Deserialize)]
struct Version {
    version: u32,
}

/// Reads the state file in `home`. No file yet means nothing to bring back. It blocks while it
/// reads.
pub(crate) fn read(home: &Path) -> Result<Restored> {
    let path = home.join("state.json");
    // This never reads a leftover temp file. Only a finished save renames it into place.
    match fs::read(&path) {
        Ok(bytes) => parse(&path, &bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Restored {
            worktrees: Vec::new(),
            settings: Settings::new(),
            sessions: HashMap::new(),
            subagent_panels: Vec::new(),
            comments: Kept::new(),
        }),
        Err(e) => Err(CoreError::io("read the state file", &path)(e)),
    }
}

impl StateFile {
    /// The state file in `home`. Each save reads the sessions and review comments from
    /// `current`.
    pub(crate) fn new(
        home: &Path,
        current: Box<dyn Fn() -> SessionsAndComments + Send + Sync>,
    ) -> StateFile {
        StateFile {
            home: home.to_path_buf(),
            path: home.join("state.json"),
            temp: home.join("state.json.tmp"),
            closed: Mutex::new(false),
            current,
        }
    }

    /// Saves the whole of `worktrees` and `settings`, and the sessions and review comments. It
    /// writes the temp file, flushes it to disk and renames it over the state file,
    /// so the state file always holds one whole save. Then it flushes the home folder, so the
    /// rename itself survives a power cut. It blocks while it writes. Once the state file is
    /// closed it writes nothing.
    pub(crate) fn save(
        &self,
        worktrees: &Mutex<Vec<WorktreeInfo>>,
        settings: &Mutex<Settings>,
    ) -> Result<()> {
        self.save_then(false, worktrees, settings)
    }

    /// Saves for the last time, then closes the state file, so the sessions a stopping core ends
    /// next stay saved.
    pub(crate) fn close(
        &self,
        worktrees: &Mutex<Vec<WorktreeInfo>>,
        settings: &Mutex<Settings>,
    ) -> Result<()> {
        self.save_then(true, worktrees, settings)
    }

    fn save_then(
        &self,
        close: bool,
        worktrees: &Mutex<Vec<WorktreeInfo>>,
        settings: &Mutex<Settings>,
    ) -> Result<()> {
        let mut closed = self.closed.lock().unwrap_or_else(PoisonError::into_inner);
        if *closed {
            return Ok(());
        }
        *closed = close;
        // Read under the save lock, so the last save carries every change made before it. Each
        // of these takes its own locks and releases them before the next.
        let current = (self.current)();
        // The worktrees lock comes first, as everywhere that holds more than one.
        let saved = {
            let worktrees = worktrees.lock().unwrap_or_else(PoisonError::into_inner);
            let settings = settings.lock().unwrap_or_else(PoisonError::into_inner);
            Saved::of(&worktrees, &settings, current)
        };
        let json = serde_json::to_vec_pretty(&saved)
            .map_err(|e| CoreError::io("encode the state for", &self.path)(e.into()))?;
        let mut file =
            File::create(&self.temp).map_err(CoreError::io("write the state to", &self.temp))?;
        file.write_all(&json)
            .map_err(CoreError::io("write the state to", &self.temp))?;
        file.sync_all()
            .map_err(CoreError::io("flush the state to", &self.temp))?;
        fs::rename(&self.temp, &self.path)
            .map_err(CoreError::io("move the new state to", &self.path))?;
        File::open(&self.home)
            .and_then(|home| home.sync_all())
            .map_err(CoreError::io("flush the home folder", &self.home))
    }
}

/// Saves the state each time `saves` asks, off the tokio workers. A save that fails is tried
/// again at the next request. Runs until the core drops.
pub(crate) async fn save_when_asked(
    saves: SaveRequests,
    state: Arc<StateFile>,
    worktrees: Arc<Mutex<Vec<WorktreeInfo>>>,
    settings: Arc<Mutex<Settings>>,
) {
    loop {
        saves.0.notified().await;
        let (state, worktrees, settings) = (state.clone(), worktrees.clone(), settings.clone());
        // Fails only if the save panics.
        let _ = tokio::task::spawn_blocking(move || state.save(&worktrees, &settings)).await;
    }
}

impl Core {
    /// Saves the state file off the tokio workers.
    pub(crate) async fn save_state(&self) -> Result<()> {
        let (state, tracked, settings) = (
            self.state_file.clone(),
            self.worktrees.clone(),
            self.repo_settings.clone(),
        );
        let path = state.path.clone();
        self.runtime
            .spawn_blocking(move || state.save(&tracked, &settings))
            .await
            // This fails only if the task panics.
            .map_err(|e| CoreError::io("save the state to", &path)(io::Error::other(e)))?
    }
}

fn parse(path: &Path, bytes: &[u8]) -> Result<Restored> {
    let bad = |e: serde_json::Error| CoreError::BadStateFile {
        path: path.to_path_buf(),
        reason: e.to_string(),
    };
    let Version { version } = serde_json::from_slice(bytes).map_err(bad)?;
    if version > VERSION {
        return Err(CoreError::NewerStateFile {
            path: path.to_path_buf(),
            version,
            known: VERSION,
        });
    }
    let saved: Saved = serde_json::from_slice(bytes).map_err(bad)?;
    let mut restored = Restored {
        worktrees: Vec::new(),
        settings: Settings::new(),
        sessions: HashMap::new(),
        subagent_panels: Vec::new(),
        comments: Kept::new(),
    };
    for repo in saved.repos {
        let main = repo.main_worktree.path.clone();
        let repo_settings = repo.restore(&mut restored);
        restored.settings.insert(main, repo_settings);
    }
    Ok(restored)
}

impl Saved {
    // The tracked list goes repo by repo, main worktree first.
    fn of(
        tracked: &[WorktreeInfo],
        settings: &Settings,
        mut current: SessionsAndComments,
    ) -> Saved {
        let mut repos: Vec<SavedRepo> = Vec::new();
        for worktree in tracked {
            let sessions = current.sessions.remove(&worktree.path).unwrap_or_default();
            let comments = current.comments.remove(&worktree.path).unwrap_or_default();
            let saved = SavedWorktree::of(worktree, sessions, comments);
            match repos.last_mut() {
                Some(repo) if !worktree.is_main => repo.linked_worktrees.push(saved),
                _ => {
                    let repo = settings.get(&worktree.path).cloned().unwrap_or_default();
                    repos.push(SavedRepo {
                        main_worktree: saved,
                        linked_worktrees: Vec::new(),
                        profile: repo.profile,
                        overrides: repo.overrides,
                        run_sessions: Vec::new(),
                        launch_args: SavedLaunchArgs::default(),
                    });
                }
            }
        }
        Saved {
            version: VERSION,
            repos,
        }
    }
}

impl SavedRepo {
    // Adds the repo's worktrees, main worktree first, and their sessions and comments to
    // `restored`, and returns the repo's settings.
    fn restore(self, restored: &mut Restored) -> RepoSettings {
        let mut overrides = self.overrides;
        let runs = self.run_sessions.into_iter().map(Into::into).collect();
        overrides.run_sessions = overrides.run_sessions.or(non_empty(runs));
        let args = &mut overrides.launch_args;
        args.claude = args.claude.take().or(non_empty(self.launch_args.claude));
        args.codex = args.codex.take().or(non_empty(self.launch_args.codex));
        let worktrees = iter::once((self.main_worktree, true))
            .chain(self.linked_worktrees.into_iter().map(|w| (w, false)));
        for (mut worktree, is_main) in worktrees {
            let path = worktree.path.clone();
            let mut sessions = std::mem::take(&mut worktree.sessions);
            let of_worktree = worktree.workspace.take();
            let of_subagents = subagent_panels(of_worktree.as_ref(), &sessions);
            restored.subagent_panels.extend(of_subagents);
            if let Some(of_worktree) = of_worktree {
                of_worktree.move_to_sessions(&mut sessions);
            }
            if !sessions.is_empty() {
                restored.sessions.insert(path.clone(), sessions);
            }
            let comments: Vec<_> = worktree.comments.drain(..).map(Into::into).collect();
            if !comments.is_empty() {
                restored.comments.insert(path, comments);
            }
            restored.worktrees.push(worktree.into_info(is_main));
        }
        RepoSettings {
            profile: self.profile,
            overrides,
        }
    }
}

// Version 2 saved an empty list for none.
fn non_empty<T>(list: Vec<T>) -> Option<Vec<T>> {
    (!list.is_empty()).then_some(list)
}

impl From<SavedRunSession> for RunSession {
    fn from(run: SavedRunSession) -> Self {
        RunSession {
            name: run.name,
            command: run.command,
            port: run.port,
        }
    }
}

impl SavedWorktree {
    fn of(
        worktree: &WorktreeInfo,
        sessions: Vec<Relaunch>,
        comments: Vec<(ReviewComment, Option<PathBuf>)>,
    ) -> SavedWorktree {
        SavedWorktree {
            path: worktree.path.clone(),
            branch: worktree.branch.clone(),
            display_name: worktree.display_name.clone(),
            issue_link: worktree.issue_link.clone(),
            base_branch: worktree.base_branch.clone(),
            nested_under: worktree.nested_under.clone(),
            workspace: None,
            sessions,
            comments: comments.into_iter().map(Into::into).collect(),
        }
    }

    fn into_info(self, is_main: bool) -> WorktreeInfo {
        WorktreeInfo {
            path: self.path,
            branch: self.branch,
            is_main,
            display_name: self.display_name,
            issue_link: self.issue_link,
            base_branch: self.base_branch,
            nested_under: self.nested_under,
        }
    }
}

impl SavedWorkspace {
    // How a file from before version 10 comes into this one: each agent session in `sessions`,
    // ended or not, gets a copy of this worktree workspace's tabs, selected tab and terminal,
    // with one shown agent panel of its own. Its own panel is shown even if it was hidden: a
    // workspace with no panel shown would show nothing of the session it belongs to. A workspace
    // of a worktree with no agent session is dropped this way, and so is a panel with no session.
    // `subagent_panels` keeps the panels of subagent sessions.
    fn move_to_sessions(self, sessions: &mut [Relaunch]) {
        for relaunch in sessions {
            let Relaunch::Agent(record) = relaunch else {
                continue;
            };
            record.workspace.get_or_insert_with(|| SavedWorkspace {
                chat: vec![SavedAgentPanel {
                    agent: record.agent,
                    shown: true,
                    session: Some(record.id),
                }],
                panels: self.panels.clone(),
                selected: self.selected,
                terminal: self.terminal,
                diff_base: self.diff_base,
            });
        }
    }
}

// The agent panel of each subagent session in `sessions` that a file from before version 10
// saved: first those `of_worktree` showed, in its order and shown or hidden as there, then
// those it had no panel for, hidden. A version 10 file saves every session with a workspace, so
// it gives none.
fn subagent_panels(of_worktree: Option<&SavedWorkspace>, sessions: &[Relaunch]) -> Vec<AgentPanel> {
    let chat = of_worktree.map_or(&[][..], |workspace| &workspace.chat);
    let mut panels: Vec<(usize, AgentPanel)> = sessions
        .iter()
        .filter_map(|relaunch| match relaunch {
            Relaunch::Agent(record) if record.parent.is_some() && record.workspace.is_none() => {
                Some(record)
            }
            _ => None,
        })
        .map(|record| {
            let at = chat
                .iter()
                .position(|panel| panel.session == Some(record.id));
            let panel = AgentPanel {
                agent: record.agent,
                session: record.id,
                shown: at.is_some_and(|at| chat[at].shown),
            };
            (at.unwrap_or(chat.len()), panel)
        })
        .collect();
    // A stable sort, so those with no panel keep the order of `sessions`.
    panels.sort_by_key(|(at, _)| *at);
    panels.into_iter().map(|(_, panel)| panel).collect()
}

impl From<&Workspace> for SavedWorkspace {
    fn from(workspace: &Workspace) -> Self {
        SavedWorkspace {
            chat: workspace
                .chat
                .iter()
                .map(|panel| SavedAgentPanel {
                    agent: panel.agent,
                    shown: panel.shown,
                    session: Some(panel.session),
                })
                .collect(),
            panels: workspace.panels.clone(),
            selected: workspace.selected,
            terminal: workspace.terminal,
            diff_base: workspace.diff_base,
        }
    }
}

impl From<SavedWorkspace> for Workspace {
    fn from(saved: SavedWorkspace) -> Self {
        Workspace {
            chat: saved
                .chat
                .into_iter()
                .filter_map(|panel| {
                    Some(AgentPanel {
                        agent: panel.agent,
                        session: panel.session?,
                        shown: panel.shown,
                    })
                })
                .collect(),
            panels: saved.panels,
            selected: saved.selected,
            terminal: saved.terminal,
            diff_base: saved.diff_base,
        }
    }
}

impl From<(ReviewComment, Option<PathBuf>)> for SavedComment {
    fn from((c, not_followed): (ReviewComment, Option<PathBuf>)) -> Self {
        SavedComment {
            id: c.id,
            path: c.path,
            start_line: c.start_line,
            end_line: c.end_line,
            text: c.text,
            body: c.body,
            state: c.state,
            possibly_addressed: c.possibly_addressed,
            not_followed,
        }
    }
}

impl From<SavedComment> for (ReviewComment, Option<PathBuf>) {
    fn from(c: SavedComment) -> Self {
        let comment = ReviewComment {
            id: c.id,
            path: c.path,
            start_line: c.start_line,
            end_line: c.end_line,
            text: c.text,
            body: c.body,
            state: c.state,
            possibly_addressed: c.possibly_addressed,
        };
        (comment, c.not_followed)
    }
}
