use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};

use crate::{Agent, AgentState, CommentId, RequestId, SessionId};

pub type Result<T> = std::result::Result<T, CoreError>;

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("another grove is already running on {}", .0.display())]
    AlreadyRunning(PathBuf),
    #[error("could not {action} {}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not start the core's tokio runtime")]
    Runtime(#[source] io::Error),
    #[error("{} is not a git repo: {reason}", path.display())]
    NotARepo { path: PathBuf, reason: String },
    #[error("the repo at {} is already added", .0.display())]
    RepoAlreadyAdded(PathBuf),
    #[error("grove does not track a worktree at {}", .0.display())]
    UnknownWorktree(PathBuf),
    #[error("grove has no repo with its main worktree at {}", .0.display())]
    UnknownRepo(PathBuf),
    #[error("the repo at {} has no run session {name:?}", repo.display())]
    UnknownRunSession { repo: PathBuf, name: String },
    #[error("run session {name:?} {reason}")]
    InvalidRunSession { name: String, reason: &'static str },
    #[error("the {agent:?} launch arg {arg:?} would replace one grove passes")]
    InvalidLaunchArgs { agent: Agent, arg: String },
    #[error("{} is a main worktree, which grove forgets only with its repo", .0.display())]
    MainWorktree(PathBuf),
    #[error(
        "{} has {}; confirm to delete it",
        worktree.display(),
        unsaved(*uncommitted, *unpushed)
    )]
    DeleteNeedsConfirmation {
        worktree: PathBuf,
        uncommitted: bool,
        /// Commits on no remote and not in the base branch.
        unpushed: usize,
    },
    #[error("{name:?} is not a valid branch name: {reason}")]
    InvalidBranchName { name: String, reason: String },
    #[error("{} is already taken", .0.display())]
    FolderTaken(PathBuf),
    #[error("the repo at {} has no origin/HEAD, main or master to use as the base branch", .0.display())]
    NoBaseBranch(PathBuf),
    #[error("sessions in {} did not exit in time, so grove left it as it was", .0.display())]
    SessionsStillRunning(PathBuf),
    #[error("{} is being removed", .0.display())]
    Removing(PathBuf),
    #[error(
        "the setup script in {} {}, so no agent session started",
        worktree.display(),
        setup_ended(*code)
    )]
    SetupFailed {
        worktree: PathBuf,
        /// None when a signal ended it.
        code: Option<i32>,
    },
    #[error(
        "the cleanup script in {} {}, so grove kept the worktree:\n{output}",
        worktree.display(),
        cleanup_ended(*code)
    )]
    CleanupFailed {
        worktree: PathBuf,
        /// None when a signal ended it, or grove stopped it after 5 minutes.
        code: Option<i32>,
        /// Its last lines, stdout and stderr together.
        output: String,
    },
    #[error("branch {branch:?} is not merged into {base:?}, so grove keeps it")]
    BranchNotMerged { branch: String, base: String },
    #[error("the repo at {} has no branch {name:?}", repo.display())]
    UnknownBranch { repo: PathBuf, name: String },
    #[error("{} has no history in common with its base branch {base:?}", worktree.display())]
    NoMergeBase { worktree: PathBuf, base: String },
    #[error("{} is not a path inside the worktree", .0.display())]
    OutsideWorktree(PathBuf),
    #[error("{} has no lines {start} to {end}", path.display())]
    NoSuchLines { path: PathBuf, start: u32, end: u32 },
    #[error("there is no review comment {0}")]
    UnknownComment(CommentId),
    #[error("no agent session in {} to send the review batch to", .0.display())]
    NoAgentSession(PathBuf),
    #[error("agent session {session} cannot take a review batch {}", while_in(*state))]
    AgentNotReady {
        session: SessionId,
        /// None before its first hook or heuristic.
        state: Option<AgentState>,
    },
    #[error("agent session {session} cannot switch mode {}", while_in(Some(*state)))]
    AgentBusy {
        session: SessionId,
        state: AgentState,
    },
    #[error("agent session {0} is already switching mode")]
    SwitchingMode(SessionId),
    /// A message sent to the session has no turn started on it yet. Ending the process would
    /// lose it.
    #[error(
        "agent session {0} cannot switch mode while a message waits for the agent to start on it"
    )]
    MessagePending(SessionId),
    #[error("session {0} is not an agent session")]
    NotAnAgentSession(SessionId),
    #[error(
        "session {parent} is a subagent {limit} levels deep, the most grove allows, so it cannot start another"
    )]
    SubagentTooDeep { parent: SessionId, limit: usize },
    /// The panel is not one that session opened, or there is no such panel. Both read the same,
    /// so a session learns nothing about the panels of others.
    #[error("session {session} has no panel {panel}")]
    UnknownPanel { session: SessionId, panel: String },
    #[error("session {session} has {limit} panels open, the most grove allows. Close one first")]
    TooManyPanels { session: SessionId, limit: usize },
    #[error("agent session {0} has bracketed paste off, so it cannot take a review batch")]
    BracketedPasteOff(SessionId),
    #[error("{} has no open review comments to send", .0.display())]
    NothingToSend(PathBuf),
    #[error("could not open {} in the editor: {reason}", path.display())]
    OpenInEditor { path: PathBuf, reason: String },
    #[error("could not show {} in Finder: {reason}", path.display())]
    RevealInFinder { path: PathBuf, reason: String },
    /// Only web links open, so a link in agent text can never start an app or open a file.
    #[error("{link:?} is not an http or https link, so grove does not open it")]
    LinkIsNotHttpOrHttps { link: String },
    #[error("git2 failed in {}", path.display())]
    Git2 {
        path: PathBuf,
        #[source]
        source: git2::Error,
    },
    #[error("git failed in {}: {reason}", path.display())]
    Git { path: PathBuf, reason: String },
    #[error("could not {action}")]
    Pty {
        action: &'static str,
        #[source]
        source: Box<dyn Error + Send + Sync>,
    },
    #[error("the Claude Code settings grove wrote to {} are not valid: {reason}", path.display())]
    BadSettings { path: PathBuf, reason: String },
    #[error("session {0} is not running")]
    SessionNotRunning(SessionId),
    #[error("session {0} is an agent session in chat mode, which has no terminal")]
    NoTerminal(SessionId),
    #[error("session {0} is not an agent session in chat mode")]
    NotInChatMode(SessionId),
    #[error("{0:?} has no chat mode yet")]
    NoChatMode(Agent),
    #[error("agent session {session} has no pending request {request}")]
    NoPendingRequest {
        session: SessionId,
        request: RequestId,
    },
    #[error("the reply does not fit request {request}: {reason}")]
    WrongReply { request: RequestId, reason: String },
    #[error(
        "the state file {} has schema version {version}, newer than the {known} this grove reads",
        path.display()
    )]
    NewerStateFile {
        path: PathBuf,
        version: u32,
        known: u32,
    },
    #[error("the state file {} is not valid: {reason}", path.display())]
    BadStateFile { path: PathBuf, reason: String },
    #[error("the profile {} is not valid{}: {reason}", path.display(), at(field.as_deref(), *line))]
    BadProfile {
        path: PathBuf,
        /// The dotted key, as in `agent.model`. None when the file does not parse as TOML.
        field: Option<String>,
        /// From 1. None when the field holds no line of its own.
        line: Option<usize>,
        reason: String,
    },
    #[error("the repo's overrides are not valid{}: {reason}", at(field.as_deref(), *line))]
    BadOverrides {
        /// The dotted key, as in `agent.model`. None when the text does not parse as TOML.
        field: Option<String>,
        /// From 1. None when the field holds no line of its own.
        line: Option<usize>,
        reason: String,
    },
    #[error("there is no valid profile named {0:?}")]
    UnknownProfile(String),
    #[error("{0:?} is not a valid profile name: it needs an ASCII letter or digit")]
    InvalidProfileName(String),
    #[error("the workspace is not valid: {reason}")]
    InvalidWorkspace { reason: &'static str },
    #[error("no agent change to the workspace of {} is left to undo", .0.display())]
    NothingToUndo(PathBuf),
}

impl CoreError {
    /// Wraps an io error with what the core was doing and to which path.
    pub(crate) fn io(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }

    /// Wraps a git2 error with the worktree it happened in.
    pub(crate) fn git2(path: &Path) -> impl FnOnce(git2::Error) -> Self {
        let path = path.to_path_buf();
        move |source| Self::Git2 { path, source }
    }

    /// Wraps a PTY or thread error with what the core was doing.
    pub(crate) fn pty<E: Into<Box<dyn Error + Send + Sync>>>(
        action: &'static str,
    ) -> impl FnOnce(E) -> Self {
        move |source| Self::Pty {
            action,
            source: source.into(),
        }
    }
}

/// The error, then each of its causes. When a process fails to start, only a cause says why.
pub(crate) fn with_causes(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}

// What deleting a worktree would lose, as in "uncommitted changes and 2 commits on no remote".
fn unsaved(uncommitted: bool, unpushed: usize) -> String {
    let commits = match unpushed {
        0 => None,
        1 => Some("1 commit on no remote".to_owned()),
        n => Some(format!("{n} commits on no remote")),
    };
    let changes = uncommitted.then(|| "uncommitted changes".to_owned());
    changes
        .into_iter()
        .chain(commits)
        .collect::<Vec<_>>()
        .join(" and ")
}

// How a failed setup script ended, as in "exited with code 3".
fn setup_ended(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exited with code {code}"),
        None => "was stopped by a signal".to_owned(),
    }
}

// How a failed cleanup script ended, as in "exited with code 4".
fn cleanup_ended(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("exited with code {code}"),
        None => "was stopped by a signal or after 5 minutes".to_owned(),
    }
}

// Where in a profile file the problem is, as in " at line 4, field agent.model".
fn at(field: Option<&str>, line: Option<usize>) -> String {
    match (line, field) {
        (Some(line), Some(field)) => format!(" at line {line}, field {field}"),
        (Some(line), None) => format!(" at line {line}"),
        (None, Some(field)) => format!(" in field {field}"),
        (None, None) => String::new(),
    }
}

// What keeps an agent session from taking a review batch, as in "while Working".
fn while_in(state: Option<AgentState>) -> String {
    state.map_or_else(
        || "before its first state".to_owned(),
        |s| format!("while {s:?}"),
    )
}
