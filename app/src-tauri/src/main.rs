//! The grove app: a thin bridge from the UI to the core (ADR 0002). The core runs in this
//! process (ADR 0009). Commands only call the Core API; there is no business logic here.

mod dock;

use std::collections::HashMap;
use std::error::Error;
use std::future::{Future, poll_fn};
use std::path::PathBuf;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::task::Poll;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use grove_core::{
    AccountInfo, AgentMessage, AgentSpec, CommentId, Core, CoreConfig, CoreError, CoreEvent,
    DiffBase, FileStatus, LineKind, ModelChoice, ModelInfo, OutputFrame, RemoveOpts, RequestId,
    SessionId, Snapshot, TurnOutcome, WorktreeInfo,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::async_runtime::RwLock;
use tauri::ipc::{Channel, InvokeResponseBody, Response};
#[cfg(target_os = "macos")]
use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::{mpsc, watch};

/// The running core. Quitting takes it out, and dropping it ends every session.
struct Grove(RwLock<Option<Core>>);

/// The sessions the UI started or was told of, by the id the UI knows: the core's id as text.
/// Only the core makes a `SessionId`, so the UI's id comes back here to find it. Entries stay
/// until quit, because a panel still acks and detaches after its session exits.
struct Sessions(Mutex<HashMap<String, SessionId>>);

/// Every warning the core has sent, oldest first. The core sends some before the UI listens,
/// such as the login shell's at start, so the UI asks for these as well.
struct Warnings(Mutex<Vec<WarningSent>>);

/// Each agent's latest login, as its sessions reported it. An agent reports it once a session
/// starts, so the UI asks for these as well after a reload.
struct Accounts(Mutex<Vec<AccountChanged>>);

/// Turns true once grove starts quitting, so a command waiting while it holds the core lets go.
struct Quitting(watch::Sender<bool>);

/// Whether grove's window has focus, as its last `Focused` event said. False until the first, so
/// a change at start is not missed.
struct WindowFocused(AtomicBool);

/// The agent session whose notification the user clicked, as the UI gets it. Each notification
/// carries it, so a click finds it.
#[derive(Serialize, Deserialize, Clone)]
struct NotificationClicked {
    session: String,
    worktree: PathBuf,
}

const QUITTING: &str = "grove is quitting";

/// A worktree as the UI sees it. Mirrors `Worktree` in `app/src/bridge.ts`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Worktree {
    path: PathBuf,
    branch: Option<String>,
    is_main: bool,
    display_name: String,
    issue_link: Option<String>,
    base_branch: Option<String>,
    nested_under: Option<PathBuf>,
    setup: Option<SetupState>,
}

impl Worktree {
    /// `setup` as `Core::setup` reports it for the worktree.
    fn new(info: WorktreeInfo, setup: Option<grove_core::SetupState>) -> Self {
        Self {
            path: info.path,
            branch: info.branch,
            is_main: info.is_main,
            display_name: info.display_name,
            issue_link: info.issue_link,
            base_branch: info.base_branch,
            nested_under: info.nested_under,
            setup: setup.map(Into::into),
        }
    }
}

/// Mirrors `SetupState` in `app/src/bridge.ts`.
#[derive(Clone, Serialize)]
#[serde(tag = "state")]
enum SetupState {
    Running,
    Succeeded,
    Failed { code: Option<i32> },
}

impl From<grove_core::SetupState> for SetupState {
    fn from(state: grove_core::SetupState) -> Self {
        match state {
            grove_core::SetupState::Running => Self::Running,
            grove_core::SetupState::Succeeded => Self::Succeeded,
            grove_core::SetupState::Failed { code } => Self::Failed { code },
        }
    }
}

/// Mirrors `SetupChanged` in `app/src/bridge.ts`.
#[derive(Clone, Serialize)]
struct SetupChanged {
    worktree: PathBuf,
    setup: SetupState,
}

/// A worktree to add. Mirrors `NewWorktree` in `app/src/bridge.ts`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewWorktree {
    branch: String,
    display_name: Option<String>,
    issue_link: Option<String>,
    base_branch: Option<String>,
}

impl From<NewWorktree> for grove_core::NewWorktree {
    fn from(new: NewWorktree) -> Self {
        Self {
            branch: new.branch,
            display_name: new.display_name,
            issue_link: new.issue_link,
            base_branch: new.base_branch,
        }
    }
}

/// Mirrors `ProfileSuggestion` in `app/src/bridge.ts`.
#[derive(Serialize)]
struct Suggestion {
    profile: String,
    marker: String,
}

impl From<grove_core::Suggestion> for Suggestion {
    fn from(suggestion: grove_core::Suggestion) -> Self {
        Self {
            profile: suggestion.profile,
            marker: suggestion.marker,
        }
    }
}

/// Mirrors `DeleteCheck` in `app/src/bridge.ts`.
#[derive(Serialize)]
struct DeleteCheck {
    uncommitted: bool,
    unpushed: usize,
    branch: Option<String>,
    merged: bool,
}

impl From<grove_core::DeleteCheck> for DeleteCheck {
    fn from(check: grove_core::DeleteCheck) -> Self {
        Self {
            uncommitted: check.uncommitted,
            unpushed: check.unpushed,
            branch: check.branch,
            merged: check.merged,
        }
    }
}

/// What deleting a worktree did with its branch. Mirrors `BranchOutcome` in `app/src/bridge.ts`.
#[derive(Serialize)]
#[serde(tag = "outcome")]
enum BranchOutcome {
    Untouched,
    Kept { name: String, merged: bool },
    Deleted { name: String },
    NotDeleted { name: String, reason: String },
}

impl From<grove_core::BranchOutcome> for BranchOutcome {
    fn from(outcome: grove_core::BranchOutcome) -> Self {
        use grove_core::BranchOutcome as Core;
        match outcome {
            Core::Untouched => Self::Untouched,
            Core::Kept { name, merged } => Self::Kept { name, merged },
            Core::Deleted { name } => Self::Deleted { name },
            Core::NotDeleted { name, reason } => Self::NotDeleted { name, reason },
        }
    }
}

/// Mirrors `DiffBase` in `app/src/bridge.ts`.
#[derive(Deserialize, Clone, Copy)]
enum Base {
    Head,
    MergeBase,
}

impl From<Base> for DiffBase {
    fn from(base: Base) -> Self {
        match base {
            Base::Head => DiffBase::Head,
            Base::MergeBase => DiffBase::MergeBase,
        }
    }
}

/// Mirrors `FileStatus` in `app/src/bridge.ts`.
#[derive(Serialize, Deserialize, Clone, Copy)]
enum Status {
    Added,
    Deleted,
    Modified,
    Renamed,
    Untracked,
}

impl From<FileStatus> for Status {
    fn from(status: FileStatus) -> Self {
        match status {
            FileStatus::Added => Status::Added,
            FileStatus::Deleted => Status::Deleted,
            FileStatus::Modified => Status::Modified,
            FileStatus::Renamed => Status::Renamed,
            FileStatus::Untracked => Status::Untracked,
        }
    }
}

impl From<Status> for FileStatus {
    fn from(status: Status) -> Self {
        match status {
            Status::Added => FileStatus::Added,
            Status::Deleted => FileStatus::Deleted,
            Status::Modified => FileStatus::Modified,
            Status::Renamed => FileStatus::Renamed,
            Status::Untracked => FileStatus::Untracked,
        }
    }
}

/// A file's change with its hunks counted, so line text never crosses to the UI. Mirrors
/// `FileDiff` in `app/src/bridge.ts`, which sends it back to ask for its versions.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileDiff {
    status: Status,
    old_path: PathBuf,
    new_path: PathBuf,
    binary: bool,
    added: usize,
    removed: usize,
}

impl From<grove_core::FileDiff> for FileDiff {
    fn from(file: grove_core::FileDiff) -> Self {
        let count = |kind| {
            file.hunks
                .iter()
                .flat_map(|hunk| &hunk.lines)
                .filter(|line| line.kind == kind)
                .count()
        };
        Self {
            added: count(LineKind::Added),
            removed: count(LineKind::Removed),
            status: file.status.into(),
            old_path: file.old_path,
            new_path: file.new_path,
            binary: file.binary,
        }
    }
}

/// Mirrors `FileVersions` in `app/src/bridge.ts`.
#[derive(Serialize)]
struct FileVersions {
    old: Option<String>,
    new: Option<String>,
    binary: bool,
}

impl From<grove_core::FileVersions> for FileVersions {
    fn from(versions: grove_core::FileVersions) -> Self {
        // The UI never shows binary content. Otherwise the core has checked both are UTF-8.
        let text = |bytes: Option<Vec<u8>>| {
            bytes
                .filter(|_| !versions.binary)
                .and_then(|b| String::from_utf8(b).ok())
        };
        Self {
            old: text(versions.old),
            new: text(versions.new),
            binary: versions.binary,
        }
    }
}

/// Mirrors `Editor` in `app/src/bridge.ts`.
#[derive(Deserialize, Clone, Copy)]
enum Editor {
    Zed,
    VsCode,
}

impl From<Editor> for grove_core::Editor {
    fn from(editor: Editor) -> Self {
        match editor {
            Editor::Zed => grove_core::Editor::Zed,
            Editor::VsCode => grove_core::Editor::VsCode,
        }
    }
}

/// Mirrors `Agent` in `app/src/bridge.ts`.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
enum Agent {
    Claude,
    Codex,
}

impl From<Agent> for grove_core::Agent {
    fn from(agent: Agent) -> Self {
        match agent {
            Agent::Claude => grove_core::Agent::Claude,
            Agent::Codex => grove_core::Agent::Codex,
        }
    }
}

impl From<grove_core::Agent> for Agent {
    fn from(agent: grove_core::Agent) -> Self {
        match agent {
            grove_core::Agent::Claude => Agent::Claude,
            grove_core::Agent::Codex => Agent::Codex,
        }
    }
}

/// A model an agent offers. Mirrors `Model` in `app/src/bridge.ts`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Model {
    id: String,
    name: String,
    description: String,
    efforts: Vec<String>,
    default_effort: Option<String>,
    is_default: bool,
}

impl From<ModelInfo> for Model {
    fn from(info: ModelInfo) -> Self {
        Self {
            id: info.id,
            name: info.name,
            description: info.description,
            efforts: info.efforts,
            default_effort: info.default_effort,
            is_default: info.is_default,
        }
    }
}

/// Mirrors `AgentState` in `app/src/bridge.ts`. Its name goes in a `state` field, next to the
/// `kind` field of a `NeedsInput`.
#[derive(Serialize, Clone)]
#[serde(tag = "state")]
enum AgentState {
    Working,
    NeedsInput { kind: InputKind },
    Failed,
    Done,
    Idle,
    Exited,
}

impl From<grove_core::AgentState> for AgentState {
    fn from(state: grove_core::AgentState) -> Self {
        match state {
            grove_core::AgentState::Working => AgentState::Working,
            grove_core::AgentState::NeedsInput { kind } => {
                AgentState::NeedsInput { kind: kind.into() }
            }
            grove_core::AgentState::Failed => AgentState::Failed,
            grove_core::AgentState::Done => AgentState::Done,
            grove_core::AgentState::Idle => AgentState::Idle,
            grove_core::AgentState::Exited => AgentState::Exited,
        }
    }
}

/// Mirrors the `NeedsInput` kind in `app/src/bridge.ts`.
#[derive(Serialize, Clone)]
enum InputKind {
    Approval,
    Question,
}

impl From<grove_core::InputKind> for InputKind {
    fn from(kind: grove_core::InputKind) -> Self {
        match kind {
            grove_core::InputKind::Approval => InputKind::Approval,
            grove_core::InputKind::Question => InputKind::Question,
        }
    }
}

/// Mirrors `StateSource` in `app/src/bridge.ts`.
#[derive(Serialize, Clone)]
enum StateSource {
    Protocol,
    Hook,
    Heuristic,
}

impl From<grove_core::StateSource> for StateSource {
    fn from(source: grove_core::StateSource) -> Self {
        match source {
            grove_core::StateSource::Protocol => StateSource::Protocol,
            grove_core::StateSource::Hook => StateSource::Hook,
            grove_core::StateSource::Heuristic => StateSource::Heuristic,
        }
    }
}

/// Mirrors `Account` in `app/src/bridge.ts`.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Account {
    email: Option<String>,
    organization: Option<String>,
    plan: Option<String>,
    api_key_source: Option<String>,
}

impl From<AccountInfo> for Account {
    fn from(info: AccountInfo) -> Self {
        Self {
            email: info.email,
            organization: info.organization,
            plan: info.plan,
            api_key_source: info.api_key_source,
        }
    }
}

/// An agent's login, sent as the `account` event and listed by `accounts`. None when nobody is
/// logged in. Mirrors `AccountChanged` in `app/src/bridge.ts`.
#[derive(Serialize, Clone)]
struct AccountChanged {
    agent: Agent,
    account: Option<Account>,
}

/// A chat-mode message. Mirrors `AgentMessage` in `app/src/bridge.ts`. Its name goes in a `type`
/// field.
#[derive(Serialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
enum Message {
    User {
        text: String,
    },
    /// Each time is when the core got the message, or what the agent's record says. None when a
    /// record does not say.
    TurnStarted {
        time_unix_ms: Option<u64>,
    },
    TextDelta {
        text: String,
    },
    ToolCall {
        id: String,
        tool: String,
        input: Value,
        time_unix_ms: Option<u64>,
    },
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    ApprovalRequested {
        request: String,
        tool: String,
        input: Value,
    },
    QuestionAsked {
        request: String,
        questions: Vec<Question>,
    },
    Answered {
        request: String,
        reply: Reply,
    },
    TurnEnded {
        time_unix_ms: Option<u64>,
        #[serde(flatten)]
        outcome: Outcome,
    },
    ReviewBatch {
        comments: Vec<BatchComment>,
    },
    NotDelivered {
        text: String,
    },
}

/// Mirrors `BatchComment` in `app/src/bridge.ts`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchComment {
    path: PathBuf,
    start_line: u32,
    end_line: u32,
    text: String,
    body: String,
}

impl From<grove_core::BatchComment> for BatchComment {
    fn from(c: grove_core::BatchComment) -> Self {
        BatchComment {
            path: c.path,
            start_line: c.start_line,
            end_line: c.end_line,
            text: c.text,
            body: c.body,
        }
    }
}

/// Mirrors `ReviewComment` in `app/src/bridge.ts`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewComment {
    id: CommentId,
    path: PathBuf,
    start_line: u32,
    end_line: u32,
    text: String,
    body: String,
    state: CommentState,
    possibly_addressed: bool,
}

/// Mirrors `ReviewComment["state"]` in `app/src/bridge.ts`.
#[derive(Serialize)]
enum CommentState {
    Open,
    Sent,
    Resolved,
}

impl From<grove_core::ReviewComment> for ReviewComment {
    fn from(c: grove_core::ReviewComment) -> Self {
        ReviewComment {
            id: c.id,
            path: c.path,
            start_line: c.start_line,
            end_line: c.end_line,
            text: c.text,
            body: c.body,
            state: match c.state {
                grove_core::CommentState::Open => CommentState::Open,
                grove_core::CommentState::Sent => CommentState::Sent,
                grove_core::CommentState::Resolved => CommentState::Resolved,
            },
            possibly_addressed: c.possibly_addressed,
        }
    }
}

/// Mirrors `NewReviewComment` in `app/src/bridge.ts`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewReviewComment {
    path: PathBuf,
    start_line: u32,
    end_line: u32,
    body: String,
}

impl From<NewReviewComment> for grove_core::NewReviewComment {
    fn from(c: NewReviewComment) -> Self {
        grove_core::NewReviewComment {
            path: c.path,
            start_line: c.start_line,
            end_line: c.end_line,
            body: c.body,
        }
    }
}

impl From<AgentMessage> for Message {
    fn from(message: AgentMessage) -> Self {
        match message {
            AgentMessage::User { text } => Message::User { text },
            AgentMessage::TurnStarted { time_unix_ms } => Message::TurnStarted { time_unix_ms },
            AgentMessage::TextDelta { text } => Message::TextDelta { text },
            AgentMessage::ToolCall {
                id,
                tool,
                input,
                time_unix_ms,
            } => Message::ToolCall {
                id,
                tool,
                input,
                time_unix_ms,
            },
            AgentMessage::ToolResult {
                id,
                output,
                is_error,
            } => Message::ToolResult {
                id,
                output,
                is_error,
            },
            AgentMessage::ApprovalRequested {
                request,
                tool,
                input,
            } => Message::ApprovalRequested {
                request: request.0,
                tool,
                input,
            },
            AgentMessage::QuestionAsked { request, questions } => Message::QuestionAsked {
                request: request.0,
                questions: questions.into_iter().map(Question::from).collect(),
            },
            AgentMessage::Answered { request, reply } => Message::Answered {
                request: request.0,
                reply: reply.into(),
            },
            AgentMessage::TurnEnded {
                outcome,
                time_unix_ms,
            } => Message::TurnEnded {
                time_unix_ms,
                outcome: outcome.into(),
            },
            AgentMessage::ReviewBatch { comments } => Message::ReviewBatch {
                comments: comments.into_iter().map(BatchComment::from).collect(),
            },
            AgentMessage::NotDelivered { text } => Message::NotDelivered { text },
        }
    }
}

/// Mirrors `Question` in `app/src/bridge.ts`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Question {
    question: String,
    header: String,
    options: Vec<QuestionOption>,
    multi_select: bool,
}

#[derive(Serialize)]
struct QuestionOption {
    label: String,
    description: String,
}

impl From<grove_core::Question> for Question {
    fn from(q: grove_core::Question) -> Self {
        Question {
            question: q.question,
            header: q.header,
            options: q
                .options
                .into_iter()
                .map(|o| QuestionOption {
                    label: o.label,
                    description: o.description,
                })
                .collect(),
            multi_select: q.multi_select,
        }
    }
}

/// The user's answer to a request, both ways. Mirrors `Reply` in `app/src/bridge.ts`. Its name
/// goes in a `type` field.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "type")]
enum Reply {
    AllowOnce,
    AllowForSession,
    Deny { message: Option<String> },
    Answers { answers: Vec<Vec<String>> },
}

impl From<grove_core::Reply> for Reply {
    fn from(reply: grove_core::Reply) -> Self {
        match reply {
            grove_core::Reply::AllowOnce => Reply::AllowOnce,
            grove_core::Reply::AllowForSession => Reply::AllowForSession,
            grove_core::Reply::Deny { message } => Reply::Deny { message },
            grove_core::Reply::Answers(answers) => Reply::Answers { answers },
        }
    }
}

impl From<Reply> for grove_core::Reply {
    fn from(reply: Reply) -> Self {
        match reply {
            Reply::AllowOnce => grove_core::Reply::AllowOnce,
            Reply::AllowForSession => grove_core::Reply::AllowForSession,
            Reply::Deny { message } => grove_core::Reply::Deny { message },
            Reply::Answers { answers } => grove_core::Reply::Answers(answers),
        }
    }
}

/// Mirrors `TurnOutcome` in `app/src/bridge.ts`. Its name goes in an `outcome` field.
#[derive(Serialize)]
#[serde(tag = "outcome")]
enum Outcome {
    Completed,
    Interrupted,
    Failed { kind: String, message: String },
}

impl From<TurnOutcome> for Outcome {
    fn from(outcome: TurnOutcome) -> Self {
        match outcome {
            TurnOutcome::Completed => Outcome::Completed,
            TurnOutcome::Interrupted => Outcome::Interrupted,
            TurnOutcome::Failed { kind, message } => Outcome::Failed { kind, message },
        }
    }
}

/// A change an agent session made, sent as the `agent-change` event for the agent change notice.
/// Mirrors `AgentChange` in `app/src/bridge.ts`.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind")]
enum AgentChange {
    /// `session` is the agent session the workspace belongs to.
    Workspace {
        session: String,
        by: String,
    },
    Profiles {
        by: String,
    },
}

impl AgentChange {
    /// The change `event` reports, when an agent session made it.
    fn of(event: &CoreEvent) -> Option<Self> {
        match event {
            CoreEvent::WorkspaceChanged {
                session,
                by: Some(by),
            } => Some(Self::Workspace {
                session: session.to_string(),
                by: by.to_string(),
            }),
            CoreEvent::ProfilesChanged { by: Some(by) } => {
                Some(Self::Profiles { by: by.to_string() })
            }
            _ => None,
        }
    }
}

/// An agent session's new state, sent as the `agent-state` event. Mirrors `AgentStateChanged`
/// in `app/src/bridge.ts`.
#[derive(Serialize, Clone)]
struct AgentStateChanged {
    session: String,
    #[serde(flatten)]
    state: AgentState,
    source: StateSource,
}

/// A warning from the core, sent as the `warning` event and listed by `warnings`. `id` is its
/// place in that list. Mirrors `WarningSent` in `app/src/bridge.ts`.
#[derive(Serialize, Clone)]
struct WarningSent {
    id: usize,
    #[serde(flatten)]
    warning: Warning,
}

/// Mirrors `Warning` in `app/src/bridge.ts`. Its name goes in a `kind` field. Codex's hook
/// warning waits until the UI starts Codex sessions.
#[derive(Serialize, Clone)]
#[serde(tag = "kind")]
enum Warning {
    LoginShellEnv { reason: String },
    SessionNotRestored { worktree: PathBuf, reason: String },
    WatcherNotStarted { worktree: PathBuf, reason: String },
}

// Attention queue (#53).

/// An agent session waiting on the user. Mirrors `AttentionEntry` in `app/src/bridge.ts`.
#[derive(Serialize)]
struct AttentionEntry {
    session: String,
    worktree: PathBuf,
}

impl From<grove_core::AttentionEntry> for AttentionEntry {
    fn from(entry: grove_core::AttentionEntry) -> Self {
        Self {
            session: entry.session.to_string(),
            worktree: entry.worktree,
        }
    }
}

// Agent session records (#62).

/// An agent session, running or ended, as `agent_sessions` lists it for the UI. `state`, with
/// the `kind` of a `NeedsInput`, is left out before the session's first state. Times are in
/// milliseconds since the Unix epoch.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentSession {
    session: String,
    worktree: PathBuf,
    agent: Agent,
    mode: Mode,
    parent: Option<String>,
    title: String,
    created_unix_ms: u64,
    ended: bool,
    #[serde(flatten)]
    state: Option<AgentState>,
    since_unix_ms: u64,
}

impl AgentSession {
    /// Remembers the session, so the UI can call session commands with one it never started,
    /// such as one that ended before the last restart.
    fn from_core(info: grove_core::AgentSessionInfo, sessions: &Sessions) -> Self {
        AgentSession {
            session: remember(sessions, info.session),
            worktree: info.worktree,
            agent: info.agent.into(),
            mode: info.mode.into(),
            // The core lists a parent only while the parent is listed, so its own row
            // remembers it.
            parent: info.parent.map(|parent| parent.to_string()),
            title: info.title,
            created_unix_ms: unix_ms(info.created),
            ended: info.ended,
            state: info.state.map(Into::into),
            since_unix_ms: unix_ms(info.since),
        }
    }
}

/// `time` in milliseconds since the Unix epoch. 0 for a time before it.
fn unix_ms(time: SystemTime) -> u64 {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

/// Every agent session of every tracked worktree, ended ones too. The UI asks again on each
/// `agent-sessions-changed` event.
#[tauri::command]
fn agent_sessions(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
) -> Result<Vec<AgentSession>, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let listed = core.agent_sessions().into_iter();
    Ok(listed
        .map(|info| AgentSession::from_core(info, &sessions))
        .collect())
}

/// Deletes the agent session and the subagent sessions below it, for good.
#[tauri::command]
async fn delete_agent_session(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<(), String> {
    let id = *lock(&sessions)
        .get(&session)
        .ok_or_else(|| format!("grove has no session {session}"))?;
    // Quitting waits for the delete, at most the core's wait for the processes to exit.
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.delete_agent_session(id)
        .await
        .map_err(|e| with_causes(&e))
}

/// Starts the ended agent session again under the same id, resuming its conversation.
#[tauri::command]
fn resume_agent_session(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<(), String> {
    let id = *lock(&sessions)
        .get(&session)
        .ok_or_else(|| format!("grove has no session {session}"))?;
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.resume_agent_session(id).map_err(|e| with_causes(&e))
}

// Workspaces (#122).

/// Mirrors `PanelKind` in `app/src/bridge.ts`.
#[derive(Serialize, Deserialize, Clone, Copy)]
enum PanelKind {
    Files,
    Diff,
    Browser,
    Logs,
    Profile,
}

impl From<grove_core::PanelKind> for PanelKind {
    fn from(kind: grove_core::PanelKind) -> Self {
        match kind {
            grove_core::PanelKind::Files => PanelKind::Files,
            grove_core::PanelKind::Diff => PanelKind::Diff,
            grove_core::PanelKind::Browser => PanelKind::Browser,
            grove_core::PanelKind::Logs => PanelKind::Logs,
            grove_core::PanelKind::Profile => PanelKind::Profile,
        }
    }
}

impl From<PanelKind> for grove_core::PanelKind {
    fn from(kind: PanelKind) -> Self {
        match kind {
            PanelKind::Files => grove_core::PanelKind::Files,
            PanelKind::Diff => grove_core::PanelKind::Diff,
            PanelKind::Browser => grove_core::PanelKind::Browser,
            PanelKind::Logs => grove_core::PanelKind::Logs,
            PanelKind::Profile => grove_core::PanelKind::Profile,
        }
    }
}

/// An agent panel in the chat area. Mirrors `AgentPanel` in `app/src/bridge.ts`.
#[derive(Serialize, Deserialize)]
struct AgentPanel {
    agent: Agent,
    session: String,
    shown: bool,
}

/// One agent session's layout. Mirrors `Workspace` in `app/src/bridge.ts`.
#[derive(Serialize, Deserialize)]
struct Workspace {
    chat: Vec<AgentPanel>,
    panels: Vec<PanelKind>,
    selected: Option<PanelKind>,
    terminal: bool,
}

impl Workspace {
    /// Remembers each panel's session, so the UI can call session commands with it after a reload.
    fn from_core(workspace: grove_core::Workspace, sessions: &Sessions) -> Self {
        Workspace {
            chat: workspace
                .chat
                .into_iter()
                .map(|panel| AgentPanel {
                    agent: panel.agent.into(),
                    session: remember(sessions, panel.session),
                    shown: panel.shown,
                })
                .collect(),
            panels: workspace.panels.into_iter().map(Into::into).collect(),
            selected: workspace.selected.map(Into::into),
            terminal: workspace.terminal,
        }
    }

    /// Fails when a panel names a session grove never gave the UI.
    fn into_core(self, sessions: &Sessions) -> Result<grove_core::Workspace, String> {
        let chat = self
            .chat
            .into_iter()
            .map(|panel| {
                Ok(grove_core::AgentPanel {
                    agent: panel.agent.into(),
                    session: known_session(sessions, &panel.session)?,
                    shown: panel.shown,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(grove_core::Workspace {
            chat,
            panels: self.panels.into_iter().map(Into::into).collect(),
            selected: self.selected.map(Into::into),
            terminal: self.terminal,
        })
    }
}

/// The workspace agent session `session` shows in: its own, or its top parent's.
#[tauri::command]
fn workspace(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<Workspace, String> {
    let session = known_session(&sessions, &session)?;
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let workspace = core.workspace(session).map_err(|e| e.to_string())?;
    Ok(Workspace::from_core(workspace, &sessions))
}

/// The workspace a new agent session in `worktree` starts from, with no agent panel yet.
#[tauri::command]
fn starting_workspace(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    worktree: PathBuf,
) -> Result<Workspace, String> {
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let workspace = core
        .starting_workspace(&worktree)
        .map_err(|e| e.to_string())?;
    Ok(Workspace::from_core(workspace, &sessions))
}

#[tauri::command]
async fn set_workspace(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    workspace: Workspace,
) -> Result<(), String> {
    let session = known_session(&sessions, &session)?;
    let workspace = workspace.into_core(&sessions)?;
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.set_workspace(session, workspace, None)
        .await
        .map_err(|e| e.to_string())
}

// The agent change notice (#4).

#[tauri::command]
async fn undo_workspace(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<(), String> {
    let session = known_session(&sessions, &session)?;
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.undo_workspace(session)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn save_workspace_to_profile(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<(), String> {
    let session = known_session(&sessions, &session)?;
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.save_workspace_to_profile(session)
        .await
        .map_err(|e| e.to_string())
}

// Worktree management (#52).

#[tauri::command]
async fn create_worktree(
    grove: State<'_, Grove>,
    repo: PathBuf,
    worktree: NewWorktree,
) -> Result<Worktree, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    let added = core
        .create_worktree(&repo, worktree.into())
        .await
        .map_err(|e| with_causes(&e))?;
    let setup = core.setup(&added.worktree.path).map(|setup| setup.state);
    Ok(Worktree::new(added.worktree, setup))
}

#[tauri::command]
async fn set_base_branch(
    grove: State<'_, Grove>,
    worktree: PathBuf,
    base: String,
) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.set_base_branch(&worktree, base)
        .await
        .map_err(|e| with_causes(&e))
}

#[tauri::command]
async fn forget_worktree(grove: State<'_, Grove>, worktree: PathBuf) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.remove_worktree(&worktree, RemoveOpts::Forget)
        .await
        .map(|_| ())
        .map_err(|e| with_causes(&e))
}

#[tauri::command]
async fn delete_check(grove: State<'_, Grove>, worktree: PathBuf) -> Result<DeleteCheck, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.delete_check(&worktree)
        .await
        .map(Into::into)
        .map_err(|e| with_causes(&e))
}

#[tauri::command]
async fn delete_worktree(
    grove: State<'_, Grove>,
    worktree: PathBuf,
    confirmed: bool,
    delete_branch: bool,
) -> Result<BranchOutcome, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    let opts = RemoveOpts::Delete {
        confirmed,
        delete_branch,
    };
    core.remove_worktree(&worktree, opts)
        .await
        .map(Into::into)
        .map_err(|e| with_causes(&e))
}

#[tauri::command]
fn profile_names(grove: State<'_, Grove>) -> Result<Vec<String>, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let profiles = core.profiles().map_err(|e| e.to_string())?;
    Ok(profiles.into_iter().map(|p| p.name).collect())
}

#[tauri::command]
fn suggest_profile(grove: State<'_, Grove>, repo: PathBuf) -> Result<Option<Suggestion>, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    Ok(core.suggest_profile(&repo).map(Into::into))
}

#[tauri::command]
fn repo_profile(grove: State<'_, Grove>, repo: PathBuf) -> Result<Option<String>, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let settings = core.repo_settings(&repo).map_err(|e| e.to_string())?;
    Ok(settings.profile)
}

/// Keeps the repo's overrides. An overrides save between the read and the write is lost, but
/// both come from the user's own clicks in the Profile panel.
#[tauri::command]
async fn set_repo_profile(
    grove: State<'_, Grove>,
    repo: PathBuf,
    profile: Option<String>,
) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    let mut settings = core.repo_settings(&repo).map_err(|e| e.to_string())?;
    settings.profile = profile;
    core.set_repo_settings(&repo, settings)
        .await
        .map_err(|e| e.to_string())
}

/// One of a repo's run sessions. Mirrors `RunSession` in `app/src/bridge.ts`.
#[derive(Serialize)]
struct RunSession {
    name: String,
    command: String,
    port: Option<u16>,
}

impl From<grove_core::RunSession> for RunSession {
    fn from(run: grove_core::RunSession) -> Self {
        RunSession {
            name: run.name,
            command: run.command,
            port: run.port,
        }
    }
}

#[tauri::command]
fn run_sessions(grove: State<'_, Grove>, repo: PathBuf) -> Result<Vec<RunSession>, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let runs = core.run_sessions(&repo).map_err(|e| e.to_string())?;
    Ok(runs.into_iter().map(Into::into).collect())
}

// Profile panel (#57).

/// Mirrors `ProfileText` in `app/src/bridge.ts`.
#[derive(Serialize)]
struct ProfileText {
    path: PathBuf,
    text: String,
}

/// Mirrors `TextProblem` in `app/src/bridge.ts`: why a text save was refused, with its line.
#[derive(Serialize)]
struct TextProblem {
    line: Option<usize>,
    message: String,
}

impl From<CoreError> for TextProblem {
    fn from(error: CoreError) -> Self {
        match error {
            CoreError::BadProfile {
                field,
                line,
                reason,
                ..
            }
            | CoreError::BadOverrides {
                field,
                line,
                reason,
            } => Self {
                line,
                // The panel shows the line itself, and the file is the one it edits.
                message: match field {
                    Some(field) => format!("{field}: {reason}"),
                    None => reason,
                },
            },
            other => Self {
                line: None,
                message: with_causes(&other),
            },
        }
    }
}

#[tauri::command]
fn profile_text(grove: State<'_, Grove>, name: String) -> Result<ProfileText, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let file = core.profile_text(&name).map_err(|e| e.to_string())?;
    Ok(ProfileText {
        path: file.path,
        text: file.text,
    })
}

/// The user's own edit, so it names no agent session.
#[tauri::command]
async fn set_profile_text(
    grove: State<'_, Grove>,
    name: String,
    text: String,
) -> Result<(), TextProblem> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or_else(quitting_problem)?;
    Ok(core.set_profile_text(&name, text, None).await?)
}

#[tauri::command]
fn repo_overrides_text(grove: State<'_, Grove>, repo: PathBuf) -> Result<String, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.repo_overrides_text(&repo).map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_repo_overrides_text(
    grove: State<'_, Grove>,
    repo: PathBuf,
    text: String,
) -> Result<(), TextProblem> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or_else(quitting_problem)?;
    Ok(core.set_repo_overrides_text(&repo, &text).await?)
}

fn quitting_problem() -> TextProblem {
    TextProblem {
        line: None,
        message: QUITTING.into(),
    }
}

// Terminal mode (#127).

/// Mirrors `Mode` in `app/src/bridge.ts`.
#[derive(Serialize, Deserialize)]
enum Mode {
    Chat,
    Terminal,
}

impl From<grove_core::Mode> for Mode {
    fn from(mode: grove_core::Mode) -> Self {
        match mode {
            grove_core::Mode::Chat => Mode::Chat,
            grove_core::Mode::Terminal => Mode::Terminal,
        }
    }
}

impl From<Mode> for grove_core::Mode {
    fn from(mode: Mode) -> Self {
        match mode {
            Mode::Chat => grove_core::Mode::Chat,
            Mode::Terminal => grove_core::Mode::Terminal,
        }
    }
}

/// Switches the agent session to `mode`, resuming the same conversation. Returns once the new
/// process runs, and in chat mode once its history is read back.
#[tauri::command]
async fn set_mode(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    mode: Mode,
) -> Result<(), String> {
    let id = *lock(&sessions)
        .get(&session)
        .ok_or_else(|| format!("grove has no session {session}"))?;
    // Quitting waits for the switch, at most the core's wait for a resumed chat's history.
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.set_mode(id, mode.into())
        .await
        .map_err(|e| e.to_string())
}

// Review comments (#54).

#[tauri::command]
async fn add_comment(
    grove: State<'_, Grove>,
    worktree: PathBuf,
    comment: NewReviewComment,
) -> Result<CommentId, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.add_comment(&worktree, comment.into())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn comments(grove: State<'_, Grove>, worktree: PathBuf) -> Result<Vec<ReviewComment>, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let comments = core.comments(&worktree).map_err(|e| e.to_string())?;
    Ok(comments.into_iter().map(ReviewComment::from).collect())
}

#[tauri::command]
fn resolve_comment(grove: State<'_, Grove>, id: CommentId) -> Result<(), String> {
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.resolve_comment(id).map_err(|e| e.to_string())
}

/// `to` is the UI's id of the agent session. None sends to the most recently focused one.
#[tauri::command]
fn send_review_batch(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    worktree: PathBuf,
    to: Option<String>,
) -> Result<(), String> {
    let to = to
        .map(|session| {
            lock(&sessions)
                .get(&session)
                .copied()
                .ok_or_else(|| format!("grove has no session {session}"))
        })
        .transpose()?;
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.send_review_batch(&worktree, to)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn last_focused_agent(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    worktree: PathBuf,
) -> Result<Option<String>, String> {
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    Ok(core
        .last_focused_agent(&worktree)
        .map(|id| remember(&sessions, id)))
}

/// The Send review picker reads it to say which terminal-mode agent session can take a batch.
#[tauri::command]
fn bracketed_paste(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<bool, String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.bracketed_paste(id)
    })
}

/// The UI calls it after Cmd-Q, once the user has confirmed if an agent session was working or
/// waiting. Exiting drops the core, which ends every session.
#[tauri::command]
fn quit(app: tauri::AppHandle) {
    app.exit(0);
}

/// As `spawn_agent`, once the worktree's setup script exits 0. Fails with `SetupFailed` when it
/// failed.
#[tauri::command]
async fn spawn_agent_after_setup(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    quitting: State<'_, Quitting>,
    worktree: PathBuf,
    agent: Option<Agent>,
) -> Result<String, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    let after_setup = async {
        core.spawn_agent_after_setup(&worktree, agent_spec(agent))
            .await
            .map_err(|e| with_causes(&e))
    };
    let session = unless_quitting(&quitting, after_setup).await?;
    Ok(remember(&sessions, session))
}

/// Runs `work` until grove starts quitting. Quitting waits for every command holding the core,
/// so one that may wait minutes, as for a setup script, gives up instead.
async fn unless_quitting<T>(
    quitting: &Quitting,
    work: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let mut quit = quitting.0.subscribe();
    let mut quit = pin!(async move {
        // Fails only once the sender is gone, which is after quitting too.
        let _ = quit.wait_for(|quitting| *quitting).await;
    });
    let mut work = pin!(work);
    poll_fn(|cx| match work.as_mut().poll(cx) {
        Poll::Ready(done) => Poll::Ready(done),
        Poll::Pending => quit.as_mut().poll(cx).map(|()| Err(QUITTING.to_owned())),
    })
    .await
}

/// macOS's app menu as Tauri's default has it, except its Quit item asks the UI first. The
/// default one quits at once, and tao gives no way to stop it.
#[cfg(target_os = "macos")]
fn menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::default(app)?;
    let info = app.package_info();
    let about = AboutMetadata {
        name: Some(info.name.clone()),
        version: Some(info.version.to_string()),
        copyright: app.config().bundle.copyright.clone(),
        ..Default::default()
    };
    let app_menu = Submenu::with_items(
        app,
        info.name.clone(),
        true,
        &[
            &PredefinedMenuItem::about(app, None, Some(about))?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(app, QUIT_ITEM, "Quit grove", true, Some("CmdOrCtrl+Q"))?,
        ],
    )?;
    menu.remove_at(0)?;
    menu.prepend(&app_menu)?;
    Ok(menu)
}

/// The id of grove's own Quit item. Only the macOS menu has it.
const QUIT_ITEM: &str = "quit";

/// The UI asks again on each `agent-state` event, so the core alone decides the order.
#[tauri::command]
fn attention_queue(grove: State<'_, Grove>) -> Result<Vec<AttentionEntry>, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    Ok(core.attention_queue().into_iter().map(Into::into).collect())
}

#[tauri::command]
async fn add_repo(grove: State<'_, Grove>, path: PathBuf) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.add_repo(&path).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_worktrees(grove: State<'_, Grove>) -> Result<Vec<Worktree>, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    Ok(core
        .list_worktrees()
        .await
        .into_iter()
        .map(|info| {
            let setup = core.setup(&info.path).map(|setup| setup.state);
            Worktree::new(info, setup)
        })
        .collect())
}

#[tauri::command]
async fn diff(
    grove: State<'_, Grove>,
    worktree: PathBuf,
    base: Base,
) -> Result<Vec<FileDiff>, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    let files = core
        .diff(&worktree, base.into())
        .await
        .map_err(|e| e.to_string())?;
    Ok(files.into_iter().map(Into::into).collect())
}

#[tauri::command]
async fn file_versions(
    grove: State<'_, Grove>,
    worktree: PathBuf,
    base: Base,
    file: FileDiff,
) -> Result<FileVersions, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    // The core reads only the status, paths and binary flag.
    let file = grove_core::FileDiff {
        status: file.status.into(),
        old_path: file.old_path,
        new_path: file.new_path,
        binary: file.binary,
        hunks: Vec::new(),
    };
    core.file_versions(&worktree, base.into(), &file)
        .await
        .map(Into::into)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_in_editor(
    grove: State<'_, Grove>,
    editor: Editor,
    worktree: PathBuf,
    file: PathBuf,
    line: u32,
) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.open_in_editor(editor.into(), &worktree, &file, line)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn reveal_in_finder(grove: State<'_, Grove>, worktree: PathBuf) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.reveal_in_finder(&worktree)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn open_link_in_default_browser(grove: State<'_, Grove>, link: String) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.open_link_in_default_browser(&link)
        .await
        .map_err(|e| e.to_string())
}

// Files panel (#124).

#[tauri::command]
async fn list_files(grove: State<'_, Grove>, worktree: PathBuf) -> Result<Vec<PathBuf>, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.list_files(&worktree).await.map_err(|e| e.to_string())
}

/// The file's bytes as they are, so the UI decides what is text.
#[tauri::command]
async fn read_file(
    grove: State<'_, Grove>,
    worktree: PathBuf,
    file: PathBuf,
) -> Result<Response, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.read_file(&worktree, &file)
        .await
        .map(Response::new)
        .map_err(|e| e.to_string())
}

/// The editor saves only text files, so the text comes as a string and goes to disk as UTF-8.
#[tauri::command]
async fn write_file(
    grove: State<'_, Grove>,
    worktree: PathBuf,
    file: PathBuf,
    text: String,
) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.write_file(&worktree, &file, text.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

/// The file's gutter marks: its lines changed since `HEAD`.
#[tauri::command]
async fn line_changes(
    grove: State<'_, Grove>,
    worktree: PathBuf,
    file: PathBuf,
) -> Result<Vec<LineChange>, String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    let changes = core
        .line_changes(&worktree, &file)
        .await
        .map_err(|e| e.to_string())?;
    Ok(changes.into_iter().map(Into::into).collect())
}

/// Mirrors `LineChange` in `app/src/bridge.ts`.
#[derive(Serialize, Debug, PartialEq)]
#[serde(tag = "kind")]
enum LineChange {
    Added { start: u32, end: u32 },
    Modified { start: u32, end: u32 },
    Deleted { after: u32 },
}

impl From<grove_core::LineChange> for LineChange {
    fn from(change: grove_core::LineChange) -> Self {
        match change {
            grove_core::LineChange::Added { start, end } => Self::Added { start, end },
            grove_core::LineChange::Modified { start, end } => Self::Modified { start, end },
            grove_core::LineChange::Deleted { after } => Self::Deleted { after },
        }
    }
}

// The session commands are sync, so they run on the main thread one at a time, in the order the UI
// sent them: keystrokes stay in order, and a detach never overtakes the attach after it.

#[tauri::command]
fn spawn_shell(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    worktree: PathBuf,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let session = core
        .spawn_shell(&worktree, cols, rows)
        .map_err(|e| with_causes(&e))?;
    Ok(remember(&sessions, session))
}

#[tauri::command]
fn spawn_run(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    worktree: PathBuf,
    name: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let spec = grove_core::RunSpec { name, cols, rows };
    let session = core
        .spawn_run(&worktree, spec)
        .map_err(|e| with_causes(&e))?;
    Ok(remember(&sessions, session))
}

#[tauri::command]
fn spawn_agent(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    worktree: PathBuf,
    agent: Option<Agent>,
) -> Result<String, String> {
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let session = core
        .spawn_agent(&worktree, agent_spec(agent))
        .map_err(|e| with_causes(&e))?;
    Ok(remember(&sessions, session))
}

// The user types the first prompt into the agent panel's composer. A switch to terminal mode
// starts the agent's TUI at this size, as a shell starts, until its panel has measured itself.
// With no `agent`, the core picks the one the worktree's profile starts with.
fn agent_spec(agent: Option<Agent>) -> AgentSpec {
    AgentSpec {
        agent: agent.map(Into::into),
        mode: grove_core::Mode::Chat,
        prompt: None,
        cols: 80,
        rows: 24,
    }
}

/// A session in a worktree. Mirrors `SessionInfo` in `app/src/bridge.ts`.
#[derive(Serialize)]
struct SessionInfo {
    session: String,
    #[serde(flatten)]
    kind: SessionKind,
    running: bool,
}

/// Mirrors the `kind` of `SessionInfo` in `app/src/bridge.ts`. Its name goes in a `kind` field.
#[derive(Serialize)]
#[serde(tag = "kind")]
enum SessionKind {
    Agent { agent: Agent, mode: Mode },
    Shell,
    Run { name: String },
}

impl From<grove_core::SessionKind> for SessionKind {
    fn from(kind: grove_core::SessionKind) -> Self {
        match kind {
            grove_core::SessionKind::Agent { agent, mode, .. } => SessionKind::Agent {
                agent: agent.into(),
                mode: mode.into(),
            },
            grove_core::SessionKind::Shell => SessionKind::Shell,
            grove_core::SessionKind::Run { name } => SessionKind::Run { name },
        }
    }
}

/// Remembers each session, as a restart brings sessions back that the UI never started.
#[tauri::command]
fn sessions(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    worktree: PathBuf,
) -> Result<Vec<SessionInfo>, String> {
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let listed = core.sessions(&worktree).map_err(|e| e.to_string())?;
    Ok(listed
        .into_iter()
        .map(|info| SessionInfo {
            session: remember(&sessions, info.session),
            kind: info.kind.into(),
            running: info.running,
        })
        .collect())
}

#[tauri::command]
fn focus(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.focus(id);
        Ok(())
    })
}

#[tauri::command]
fn write(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    bytes: Vec<u8>,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.write(id, &bytes)
    })
}

#[tauri::command]
fn resize(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.resize(id, cols, rows)
    })
}

#[tauri::command]
fn attach(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    output: Channel<InvokeResponseBody>,
) -> Result<Response, String> {
    with_session(&grove, &sessions, &session, |core, id| {
        let (sink, mut frames) = mpsc::unbounded_channel();
        let snapshot = core.attach(id, sink)?;
        // Ends when the core drops the sink: on detach, on the next attach, or when the output ends.
        tauri::async_runtime::spawn(async move {
            while let Some(frame) = frames.recv().await {
                // Fails only once the webview is gone.
                if output
                    .send(InvokeResponseBody::Raw(frame_body(frame)))
                    .is_err()
                {
                    break;
                }
            }
        });
        // Raw, so the UI gets an ArrayBuffer rather than a JSON array of numbers.
        Ok(Response::new(snapshot_body(snapshot)))
    })
}

#[tauri::command]
fn subscribe(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    messages: Channel<Vec<Message>>,
) -> Result<Vec<Message>, String> {
    with_session(&grove, &sessions, &session, |core, id| {
        let (sink, mut frames) = mpsc::unbounded_channel();
        let history = core.subscribe(id, sink)?;
        // Ends when the core drops the sink: on the next subscribe, or once the session's
        // process has exited and its last frame is out.
        tauri::async_runtime::spawn(async move {
            while let Some(frame) = frames.recv().await {
                let frame = frame.into_iter().map(Message::from).collect();
                // Fails only once the webview is gone.
                if messages.send(frame).is_err() {
                    break;
                }
            }
        });
        Ok(history.into_iter().map(Message::from).collect())
    })
}

#[tauri::command]
fn send_message(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    text: String,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.send_message(id, &text)
    })
}

#[tauri::command]
fn interrupt(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| core.interrupt(id))
}

/// Answers a pending approval or question. Fails if the request ended or the reply does not fit it.
#[tauri::command]
fn answer(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    request: String,
    reply: Reply,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.answer(id, RequestId(request), reply.into())
    })
}

/// The models the session's agent offers, once it has listed them.
#[tauri::command]
async fn models(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<Vec<Model>, String> {
    // Waits without the core's lock, so quitting never waits on an agent that has not listed them.
    let listed = with_session(&grove, &sessions, &session, |core, id| core.models(id))?;
    let models = listed.await.map_err(|e| e.to_string())?;
    Ok(models.into_iter().map(Into::into).collect())
}

/// Sets the session's model, and its effort when given, from the next turn on.
#[tauri::command]
fn set_model(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    model: String,
    effort: Option<String>,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.set_model(id, &model, effort.as_deref())
    })
}

/// The model and effort `set_model` last saved for the agent session, or null.
#[tauri::command]
fn model_choice(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<Option<ModelChoice>, String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.model_choice(id)
    })
}

#[tauri::command]
fn ack(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
    bytes: usize,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.ack(id, bytes);
        Ok(())
    })
}

#[tauri::command]
fn detach(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| {
        core.detach(id);
        Ok(())
    })
}

#[tauri::command]
fn kill(
    grove: State<'_, Grove>,
    sessions: State<'_, Sessions>,
    session: String,
) -> Result<(), String> {
    with_session(&grove, &sessions, &session, |core, id| core.kill(id))
}

/// Runs `call` with the core and the session the UI calls `session`.
fn with_session<T>(
    grove: &Grove,
    sessions: &Sessions,
    session: &str,
    call: impl FnOnce(&Core, SessionId) -> grove_core::Result<T>,
) -> Result<T, String> {
    // Only quitting holds the write lock, so this never waits.
    let core = grove.0.try_read().map_err(|_| QUITTING)?;
    let core = core.as_ref().ok_or(QUITTING)?;
    let id = *lock(sessions)
        .get(session)
        .ok_or_else(|| format!("grove has no session {session}"))?;
    call(core, id).map_err(|e| e.to_string())
}

fn lock(sessions: &Sessions) -> MutexGuard<'_, HashMap<String, SessionId>> {
    sessions.0.lock().unwrap_or_else(PoisonError::into_inner)
}

#[tauri::command]
fn warnings(warnings: State<'_, Warnings>) -> Vec<WarningSent> {
    warnings
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Starts the worktree's watcher after a `WatcherNotStarted` warning. The error holds the new
/// cause when it fails again.
#[tauri::command]
async fn retry_watcher(grove: State<'_, Grove>, worktree: PathBuf) -> Result<(), String> {
    let core = grove.0.read().await;
    let core = core.as_ref().ok_or(QUITTING)?;
    core.retry_watcher(&worktree)
        .await
        .map_err(|e| with_causes(&e))
}

// Keeps the warning for `warnings`, then sends it to the UI.
fn warn(app: &tauri::AppHandle, warning: Warning) {
    let sent = {
        let warnings = app.state::<Warnings>();
        let mut warnings = warnings.0.lock().unwrap_or_else(PoisonError::into_inner);
        let sent = WarningSent {
            id: warnings.len(),
            warning,
        };
        warnings.push(sent.clone());
        sent
    };
    let _ = app.emit("warning", sent);
}

#[tauri::command]
fn accounts(accounts: State<'_, Accounts>) -> Vec<AccountChanged> {
    accounts
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

// Keeps the agent's login for `accounts` in place of its last one, then sends it to the UI.
fn report_account(app: &tauri::AppHandle, change: AccountChanged) {
    {
        let accounts = app.state::<Accounts>();
        let mut accounts = accounts.0.lock().unwrap_or_else(PoisonError::into_inner);
        accounts.retain(|known| known.agent != change.agent);
        accounts.push(change.clone());
    }
    let _ = app.emit("account", change);
}

/// The session the UI knows as `id`.
fn known_session(sessions: &Sessions, id: &str) -> Result<SessionId, String> {
    lock(sessions)
        .get(id)
        .copied()
        .ok_or_else(|| format!("grove has no session {id}"))
}

/// Keeps a new session under the id the UI will know it by, and returns that id.
fn remember(sessions: &Sessions, session: SessionId) -> String {
    let id = session.to_string();
    lock(sessions).insert(id.clone(), session);
    id
}

/// The error, then each of its causes. When a session fails to start, only a cause holds the
/// reason, such as a command missing from PATH.
fn with_causes(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}

/// A frame as `parseFrame` in `app/src/bridge.ts` reads it: its seq in 8 big-endian bytes, then
/// its output.
fn frame_body(frame: OutputFrame) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + frame.bytes.len());
    body.extend_from_slice(&frame.seq.to_be_bytes());
    body.extend_from_slice(&frame.bytes);
    body
}

/// A snapshot as `parseSnapshot` in `app/src/bridge.ts` reads it: its seq in 8 big-endian bytes,
/// its cols and rows in 2 each, then its bytes.
fn snapshot_body(snapshot: Snapshot) -> Vec<u8> {
    let mut body = Vec::with_capacity(12 + snapshot.bytes.len());
    body.extend_from_slice(&snapshot.seq.to_be_bytes());
    body.extend_from_slice(&snapshot.cols.to_be_bytes());
    body.extend_from_slice(&snapshot.rows.to_be_bytes());
    body.extend_from_slice(&snapshot.bytes);
    body
}

// What the notification of an agent session that entered `state` in `worktree` says: the
// worktree's name, then what happened. None while grove's window has focus, since the user is
// already looking.
fn notification(
    window_focused: bool,
    worktree: &std::path::Path,
    state: grove_core::AgentState,
) -> Option<(String, String)> {
    if window_focused {
        return None;
    }
    let name = worktree
        .file_name()
        .unwrap_or(worktree.as_os_str())
        .to_string_lossy();
    let what = match state {
        grove_core::AgentState::NeedsInput {
            kind: grove_core::InputKind::Approval,
        } => "An agent needs your approval",
        grove_core::AgentState::NeedsInput {
            kind: grove_core::InputKind::Question,
        } => "An agent has a question",
        grove_core::AgentState::Failed => "An agent failed",
        grove_core::AgentState::Done => "An agent is done",
        grove_core::AgentState::Working => "An agent is working",
        grove_core::AgentState::Idle => "An agent is idle",
        grove_core::AgentState::Exited => "An agent exited",
    };
    Some((name.into_owned(), what.to_owned()))
}

// Shows the notification of an agent session that entered `state`, unless grove's window has
// focus. It returns at once: the plugin waits for the click on a thread of its own.
fn notify(
    app: &tauri::AppHandle,
    session: SessionId,
    worktree: PathBuf,
    state: grove_core::AgentState,
) {
    let focused = app.state::<WindowFocused>().0.load(Ordering::Relaxed);
    let Some((title, body)) = notification(focused, &worktree, state) else {
        return;
    };
    let clicked = NotificationClicked {
        session: session.to_string(),
        worktree,
    };
    // Fails only when the notification could not be prepared, and there is no one to tell.
    let _ = app
        .notification()
        .builder()
        .title(title)
        .body(body)
        .extra("clicked", clicked)
        .show();
}

// A click on a notification brings grove forward and tells the UI whose it was. The UI does
// nothing more for a session that is gone by then.
fn on_notification_action(
    app: &tauri::AppHandle,
    performed: &tauri_plugin_notification::ActionPerformed,
) {
    if performed.action_id() != "tap" {
        return;
    }
    show_window(app);
    let clicked = performed
        .notification()
        .and_then(|notification| notification.extra().get("clicked").cloned())
        .and_then(|clicked| serde_json::from_value::<NotificationClicked>(clicked).ok());
    if let Some(clicked) = clicked {
        let _ = app.emit("notification-clicked", clicked);
    }
}

// Shows the main window, which may be hidden, and brings grove forward.
fn show_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn main() -> anyhow::Result<()> {
    let mut cfg = CoreConfig::for_user().context("could not find the home folder")?;
    // grove-hook sits next to this executable: in target/<profile>/ after a workspace build,
    // and in Contents/MacOS/ in the .app that `npm run bundle` makes. Without it the core skips
    // the copy, so the app still starts but agents get no hook status.
    cfg.hook_relay_source = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("grove-hook")))
        .filter(|relay| relay.exists());
    // The Claude sidecar sits next to it in the .app. Elsewhere the core runs `claude-sidecar`
    // from PATH, so chat mode fails to start unless one is there.
    if let Some(sidecar) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("claude-sidecar")))
        .filter(|sidecar| sidecar.exists())
    {
        cfg.claude_sidecar_command = sidecar;
    }
    // grove-mcp sits next to this executable, as grove-hook does. Elsewhere the core runs
    // `grove-mcp` from PATH, so agents cannot spawn subagents unless one is there.
    if let Some(mcp_server) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("grove-mcp")))
        .filter(|mcp_server| mcp_server.exists())
    {
        cfg.mcp_server = mcp_server;
    }
    let (core, mut events) = Core::start(cfg)?;

    tauri::Builder::default()
        .manage(Grove(RwLock::new(Some(core))))
        .manage(Sessions(Mutex::default()))
        .manage(Quitting(watch::Sender::new(false)))
        .manage(Warnings(Mutex::default()))
        .manage(Accounts(Mutex::default()))
        .manage(WindowFocused(AtomicBool::new(false)))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_menu(menu(app.handle())?)?;
            // Only SessionExited, DiffChanged, AgentState, AgentSessionsChanged, each agent's
            // account, the login shell's warning, the warning for a session a restart could not
            // bring back, each worktree's setup, ProfilesChanged, RepoSettingsChanged,
            // CommentsChanged and WorkspaceChanged reach the UI so far. Notify becomes a macOS
            // notification.
            let app = app.handle().clone();
            // Registered before any notification shows, since one shown earlier reports no click.
            let clicks = app.clone();
            app.notification()
                .on_action(move |performed| on_notification_action(&clicks, performed))?;
            tauri::async_runtime::spawn(async move {
                loop {
                    match events.recv().await {
                        // A run session's tab says when it stopped.
                        Ok(CoreEvent::SessionExited { session, .. }) => {
                            let _ = app.emit("session-exited", session.to_string());
                        }
                        Ok(CoreEvent::DiffChanged { worktree }) => {
                            let _ = app.emit("diff-changed", worktree);
                        }
                        Ok(CoreEvent::AgentState {
                            session,
                            state,
                            source,
                        }) => {
                            let change = AgentStateChanged {
                                session: session.to_string(),
                                state: state.into(),
                                source: source.into(),
                            };
                            let _ = app.emit("agent-state", change);
                        }
                        // The UI lists the agent sessions again.
                        Ok(CoreEvent::AgentSessionsChanged) => {
                            let _ = app.emit("agent-sessions-changed", ());
                        }
                        Ok(CoreEvent::Notify {
                            session,
                            worktree,
                            state,
                        }) => notify(&app, session, worktree, state),
                        Ok(CoreEvent::Account { agent, account }) => {
                            let change = AccountChanged {
                                agent: agent.into(),
                                account: account.map(Into::into),
                            };
                            report_account(&app, change);
                        }
                        Ok(CoreEvent::Warning(grove_core::Warning::LoginShellEnv { reason })) => {
                            warn(&app, Warning::LoginShellEnv { reason });
                        }
                        Ok(CoreEvent::Warning(grove_core::Warning::SessionNotRestored {
                            worktree,
                            reason,
                        })) => {
                            warn(&app, Warning::SessionNotRestored { worktree, reason });
                        }
                        Ok(CoreEvent::Warning(grove_core::Warning::WatcherNotStarted {
                            worktree,
                            reason,
                        })) => {
                            warn(&app, Warning::WatcherNotStarted { worktree, reason });
                        }
                        Ok(CoreEvent::Setup { worktree, setup }) => {
                            let change = SetupChanged {
                                worktree,
                                setup: setup.state.into(),
                            };
                            let _ = app.emit("setup", change);
                        }
                        Ok(event @ CoreEvent::ProfilesChanged { .. }) => {
                            if let Some(change) = AgentChange::of(&event) {
                                let _ = app.emit("agent-change", change);
                            }
                            let _ = app.emit("profiles-changed", ());
                        }
                        // Each Profile panel of the repo reloads.
                        Ok(CoreEvent::RepoSettingsChanged { repo, .. }) => {
                            let _ = app.emit("repo-settings-changed", repo);
                        }
                        Ok(CoreEvent::CommentsChanged { worktree }) => {
                            let _ = app.emit("comments-changed", worktree);
                        }
                        // The notice goes first, so the UI holds the workspace from before the
                        // change when the reload comes.
                        Ok(event @ CoreEvent::WorkspaceChanged { .. }) => {
                            if let Some(change) = AgentChange::of(&event) {
                                let _ = app.emit("agent-change", change);
                            }
                            if let CoreEvent::WorkspaceChanged { session, .. } = event {
                                let _ = app.emit("workspace-changed", session.to_string());
                            }
                        }
                        Ok(_) => {}
                        Err(_) if events.is_closed() => break,
                        // After a lag the missed events are gone. Carry on with the next one.
                        Err(_) => {}
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            add_repo,
            list_worktrees,
            diff,
            file_versions,
            open_in_editor,
            reveal_in_finder,
            open_link_in_default_browser,
            list_files,
            read_file,
            write_file,
            line_changes,
            spawn_shell,
            spawn_run,
            spawn_agent,
            sessions,
            focus,
            write,
            resize,
            attach,
            subscribe,
            send_message,
            interrupt,
            answer,
            models,
            set_model,
            model_choice,
            ack,
            detach,
            kill,
            warnings,
            retry_watcher,
            accounts,
            // Attention queue (#53).
            attention_queue,
            // Agent session records (#62).
            agent_sessions,
            delete_agent_session,
            resume_agent_session,
            // Workspaces (#122).
            workspace,
            starting_workspace,
            set_workspace,
            undo_workspace,
            save_workspace_to_profile,
            // Worktree management (#52).
            create_worktree,
            set_base_branch,
            forget_worktree,
            delete_check,
            delete_worktree,
            profile_names,
            suggest_profile,
            repo_profile,
            set_repo_profile,
            profile_text,
            set_profile_text,
            repo_overrides_text,
            set_repo_overrides_text,
            run_sessions,
            quit,
            spawn_agent_after_setup,
            // Terminal mode (#127).
            set_mode,
            // Review comments (#54).
            add_comment,
            comments,
            resolve_comment,
            send_review_batch,
            last_focused_agent,
            bracketed_paste,
            // App icon (#55).
            dock::set_dock_icon
        ])
        // Cmd-Q shows the window, since it may be hidden, and asks the UI.
        .on_menu_event(|app, event| {
            if event.id() == QUIT_ITEM {
                show_window(app);
                let _ = app.emit("quit-requested", ());
            }
        })
        .on_window_event(|window, event| match event {
            // Closing the window only hides it. grove and its sessions keep running.
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Focused(focused) => {
                window
                    .state::<WindowFocused>()
                    .0
                    .store(*focused, Ordering::Relaxed);
            }
            _ => {}
        })
        .build(tauri::generate_context!())?
        .run(|app, event| match event {
            // Clicking the Dock icon shows the hidden window again.
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => show_window(app),
            // Cmd-Q: dropping the core ends every session before the process exits. Commands
            // that wait on the core give it up first.
            RunEvent::Exit => {
                app.state::<Quitting>().0.send_replace(true);
                drop(app.state::<Grove>().0.blocking_write().take());
            }
            _ => {}
        });
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use grove_core::{DiffLine, FileStatus, Hunk, LineKind, TurnOutcome};
    use tauri::ipc::IpcResponse;

    use super::*;

    fn line(kind: LineKind) -> DiffLine {
        DiffLine {
            kind,
            text: b"x\n".to_vec(),
        }
    }

    #[test]
    fn a_file_diff_counts_its_added_and_removed_lines_across_hunks() {
        let hunk = |lines| Hunk {
            old_start: 1,
            old_lines: 0,
            new_start: 1,
            new_lines: 0,
            lines,
        };
        let core = grove_core::FileDiff {
            status: FileStatus::Renamed,
            old_path: "old.rs".into(),
            new_path: "new.rs".into(),
            binary: false,
            hunks: vec![
                hunk(vec![
                    line(LineKind::Context),
                    line(LineKind::Removed),
                    line(LineKind::Added),
                    line(LineKind::Added),
                ]),
                hunk(vec![line(LineKind::Added), line(LineKind::Removed)]),
            ],
        };

        let file = FileDiff::from(core);

        assert_eq!((file.added, file.removed), (3, 2));
        assert_eq!(file.old_path, Path::new("old.rs"));
        assert_eq!(file.new_path, Path::new("new.rs"));
    }

    // The agent change notice (#4).

    #[test]
    fn only_a_change_an_agent_made_reaches_the_ui_as_an_agent_change() {
        let by: SessionId = serde_json::from_str("7").unwrap();
        let workspace = |by| CoreEvent::WorkspaceChanged {
            session: serde_json::from_str("3").unwrap(),
            by,
        };

        let changes = [
            workspace(Some(by)),
            workspace(None),
            CoreEvent::ProfilesChanged { by: Some(by) },
            CoreEvent::ProfilesChanged { by: None },
        ]
        .iter()
        .filter_map(AgentChange::of)
        .collect::<Vec<_>>();

        assert_eq!(
            serde_json::to_string(&changes).unwrap(),
            r#"[{"kind":"Workspace","session":"3","by":"7"},{"kind":"Profiles","by":"7"}]"#
        );
    }

    // Files panel (#124).

    #[test]
    fn line_changes_reach_the_ui_as_bridge_ts_reads_them() {
        let changes: Vec<LineChange> = [
            grove_core::LineChange::Modified { start: 1, end: 1 },
            grove_core::LineChange::Added { start: 3, end: 4 },
            grove_core::LineChange::Deleted { after: 4 },
        ]
        .into_iter()
        .map(Into::into)
        .collect();

        assert_eq!(
            serde_json::to_string(&changes).unwrap(),
            r#"[{"kind":"Modified","start":1,"end":1},{"kind":"Added","start":3,"end":4},{"kind":"Deleted","after":4}]"#
        );
    }

    #[test]
    fn a_worktrees_setup_reaches_the_ui_as_bridge_ts_reads_it() {
        let states: Vec<SetupState> = [
            grove_core::SetupState::Running,
            grove_core::SetupState::Succeeded,
            grove_core::SetupState::Failed { code: Some(3) },
            grove_core::SetupState::Failed { code: None },
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let worktree = Worktree::new(
            WorktreeInfo {
                path: "/w".into(),
                branch: None,
                is_main: false,
                display_name: "w".into(),
                issue_link: None,
                base_branch: None,
                nested_under: None,
            },
            Some(grove_core::SetupState::Running),
        );
        let change = SetupChanged {
            worktree: "/w".into(),
            setup: grove_core::SetupState::Failed { code: Some(3) }.into(),
        };

        assert_eq!(
            serde_json::to_string(&states).unwrap(),
            r#"[{"state":"Running"},{"state":"Succeeded"},{"state":"Failed","code":3},{"state":"Failed","code":null}]"#
        );
        assert_eq!(
            serde_json::to_string(&worktree).unwrap(),
            r#"{"path":"/w","branch":null,"isMain":false,"displayName":"w","issueLink":null,"baseBranch":null,"nestedUnder":null,"setup":{"state":"Running"}}"#
        );
        assert_eq!(
            serde_json::to_string(&change).unwrap(),
            r#"{"worktree":"/w","setup":{"state":"Failed","code":3}}"#
        );
    }

    #[test]
    fn a_wait_on_the_core_gives_up_once_grove_quits_and_finished_work_wins() {
        let quitting = Quitting(tokio::sync::watch::Sender::new(false));
        let waiting_for_setup = async {
            quitting.0.send_replace(true);
            std::future::pending::<Result<(), String>>().await
        };

        assert_eq!(
            tauri::async_runtime::block_on(unless_quitting(&quitting, waiting_for_setup)),
            Err(QUITTING.to_owned())
        );
        assert_eq!(
            tauri::async_runtime::block_on(unless_quitting(&quitting, async { Ok(1) })),
            Ok(1)
        );
    }

    #[test]
    fn worktree_management_replies_reach_the_ui_as_bridge_ts_reads_them() {
        let worktree = Worktree::new(
            WorktreeInfo {
                path: "/code/grove-wt/feat-52".into(),
                branch: Some("feat-52".into()),
                is_main: false,
                display_name: "Worktree UI".into(),
                issue_link: None,
                base_branch: Some("main".into()),
                nested_under: Some("/code/grove".into()),
            },
            None,
        );
        let outcomes: Vec<BranchOutcome> = [
            grove_core::BranchOutcome::Untouched,
            grove_core::BranchOutcome::Kept {
                name: "a".into(),
                merged: true,
            },
            grove_core::BranchOutcome::Deleted { name: "b".into() },
            grove_core::BranchOutcome::NotDeleted {
                name: "c".into(),
                reason: "locked".into(),
            },
        ]
        .into_iter()
        .map(Into::into)
        .collect();

        assert_eq!(
            serde_json::to_string(&worktree).unwrap(),
            r#"{"path":"/code/grove-wt/feat-52","branch":"feat-52","isMain":false,"displayName":"Worktree UI","issueLink":null,"baseBranch":"main","nestedUnder":"/code/grove","setup":null}"#
        );
        let check = DeleteCheck::from(grove_core::DeleteCheck {
            uncommitted: true,
            unpushed: 2,
            branch: None,
            merged: false,
        });
        let suggestion = Suggestion::from(grove_core::Suggestion {
            profile: "Rust crate".into(),
            marker: "Cargo.toml".into(),
        });
        assert_eq!(
            serde_json::to_string(&check).unwrap(),
            r#"{"uncommitted":true,"unpushed":2,"branch":null,"merged":false}"#
        );
        assert_eq!(
            serde_json::to_string(&suggestion).unwrap(),
            r#"{"profile":"Rust crate","marker":"Cargo.toml"}"#
        );
        assert_eq!(
            serde_json::to_string(&outcomes).unwrap(),
            r#"[{"outcome":"Untouched"},{"outcome":"Kept","name":"a","merged":true},{"outcome":"Deleted","name":"b"},{"outcome":"NotDeleted","name":"c","reason":"locked"}]"#
        );
    }

    #[test]
    fn a_refused_text_save_sends_its_line_and_field_as_bridge_ts_reads_them() {
        let bad = TextProblem::from(CoreError::BadOverrides {
            field: Some("agent.modle".into()),
            line: Some(4),
            reason: "unknown field `modle`".into(),
        });
        let syntax = TextProblem::from(CoreError::BadProfile {
            path: "/home/.grovecode/profiles/rust-crate.toml".into(),
            field: None,
            line: Some(2),
            reason: "unclosed table, expected `]`".into(),
        });
        let other = TextProblem::from(CoreError::UnknownProfile("Go".into()));
        let unwritable = TextProblem::from(CoreError::Io {
            action: "write the profile",
            path: "/home/.grovecode/profiles/rust-crate.toml".into(),
            source: std::io::Error::other("disk full"),
        });

        assert_eq!(
            unwritable.message,
            "could not write the profile /home/.grovecode/profiles/rust-crate.toml: disk full"
        );
        assert_eq!(
            serde_json::to_string(&bad).unwrap(),
            r#"{"line":4,"message":"agent.modle: unknown field `modle`"}"#
        );
        assert_eq!(
            serde_json::to_string(&syntax).unwrap(),
            r#"{"line":2,"message":"unclosed table, expected `]`"}"#
        );
        assert_eq!(
            serde_json::to_string(&other).unwrap(),
            r#"{"line":null,"message":"there is no valid profile named \"Go\""}"#
        );
    }

    #[test]
    fn run_sessions_reach_the_ui_as_bridge_ts_reads_them() {
        let dev = grove_core::RunSession {
            name: "dev".into(),
            command: "pnpm dev".into(),
            port: Some(5173),
        };

        assert_eq!(
            serde_json::to_string(&RunSession::from(dev)).unwrap(),
            r#"{"name":"dev","command":"pnpm dev","port":5173}"#
        );
    }

    #[test]
    fn restored_sessions_and_their_warning_reach_the_ui_as_bridge_ts_reads_them() {
        let kinds: Vec<SessionKind> = [
            grove_core::SessionKind::Agent {
                agent: grove_core::Agent::Codex,
                mode: grove_core::Mode::Terminal,
                parent: None,
            },
            grove_core::SessionKind::Shell,
            grove_core::SessionKind::Run { name: "dev".into() },
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let sessions: Vec<SessionInfo> = kinds
            .into_iter()
            .zip([("3", true), ("4", true), ("5", false)])
            .map(|(kind, (session, running))| SessionInfo {
                session: session.into(),
                kind,
                running,
            })
            .collect();
        let warning = WarningSent {
            id: 1,
            warning: Warning::SessionNotRestored {
                worktree: "/w".into(),
                reason: "could not start codex".into(),
            },
        };

        assert_eq!(
            serde_json::to_string(&sessions).unwrap(),
            r#"[{"session":"3","kind":"Agent","agent":"Codex","mode":"Terminal","running":true},{"session":"4","kind":"Shell","running":true},{"session":"5","kind":"Run","name":"dev","running":false}]"#
        );
        assert_eq!(
            serde_json::to_string(&warning).unwrap(),
            r#"{"id":1,"kind":"SessionNotRestored","worktree":"/w","reason":"could not start codex"}"#
        );
    }

    #[test]
    fn a_new_worktree_from_the_ui_reads_as_bridge_ts_sends_it() {
        let new: NewWorktree = serde_json::from_str(
            r##"{"branch":"feat-52","displayName":null,"issueLink":"#52","baseBranch":"main"}"##,
        )
        .unwrap();

        assert_eq!(
            grove_core::NewWorktree::from(new),
            grove_core::NewWorktree {
                branch: "feat-52".into(),
                display_name: None,
                issue_link: Some("#52".into()),
                base_branch: Some("main".into()),
            }
        );
    }

    #[test]
    fn a_frame_goes_to_the_ui_as_its_seq_in_8_big_endian_bytes_then_its_output() {
        let frame = OutputFrame {
            seq: 258,
            bytes: vec![0x68, 0x69, 0xe6],
        };

        assert_eq!(
            frame_body(frame),
            [0, 0, 0, 0, 0, 0, 1, 2, 0x68, 0x69, 0xe6]
        );
    }

    #[test]
    fn a_snapshot_goes_to_the_ui_as_its_seq_in_8_bytes_its_size_in_4_then_its_bytes() {
        let snapshot = Snapshot {
            seq: 258,
            cols: 300,
            rows: 24,
            bytes: b"\x1b[H".to_vec(),
        };

        assert_eq!(
            snapshot_body(snapshot),
            [0, 0, 0, 0, 0, 0, 1, 2, 1, 44, 0, 24, 0x1b, b'[', b'H']
        );
    }

    // Without it WebKit blocks Tauri's IPC fetch, and Tauri falls back to postMessage. That hands
    // a channel message of 1 KiB or more to the UI as a number array, so a panel stops at its
    // first large frame.
    #[test]
    fn the_csp_lets_the_ui_fetch_from_the_ipc_protocol() {
        let config = include_str!("../tauri.conf.json");

        assert!(config.contains("connect-src 'self' ipc: http://ipc.localhost;"));
    }

    #[test]
    fn an_attention_entry_reaches_the_ui_as_bridge_ts_reads_it() {
        let entry = AttentionEntry {
            session: "3".into(),
            worktree: "/code/grove-wt/feat-53".into(),
        };

        assert_eq!(
            serde_json::to_string(&entry).unwrap(),
            r#"{"session":"3","worktree":"/code/grove-wt/feat-53"}"#
        );
    }

    #[test]
    fn an_agent_state_reaches_the_ui_as_bridge_ts_reads_it() {
        let json = |state, source| {
            let change = AgentStateChanged {
                session: "3".into(),
                state: AgentState::from(state),
                source: StateSource::from(source),
            };
            match change.body() {
                Ok(InvokeResponseBody::Json(json)) => json,
                _ => panic!("an agent state goes to the UI as JSON"),
            }
        };

        assert_eq!(
            json(
                grove_core::AgentState::NeedsInput {
                    kind: grove_core::InputKind::Question
                },
                grove_core::StateSource::Hook
            ),
            r#"{"session":"3","state":"NeedsInput","kind":"Question","source":"Hook"}"#
        );
        assert_eq!(
            json(
                grove_core::AgentState::Done,
                grove_core::StateSource::Heuristic
            ),
            r#"{"session":"3","state":"Done","source":"Heuristic"}"#
        );
        assert_eq!(
            json(
                grove_core::AgentState::Working,
                grove_core::StateSource::Protocol
            ),
            r#"{"session":"3","state":"Working","source":"Protocol"}"#
        );
    }

    #[test]
    fn a_workspace_reaches_the_ui_as_bridge_ts_reads_it_and_comes_back_the_same() {
        let sessions = Sessions(Mutex::default());
        let core = grove_core::Workspace {
            chat: vec![grove_core::AgentPanel {
                agent: grove_core::Agent::Codex,
                session: serde_json::from_str("4").unwrap(),
                shown: false,
            }],
            panels: vec![grove_core::PanelKind::Files, grove_core::PanelKind::Logs],
            selected: Some(grove_core::PanelKind::Logs),
            terminal: true,
        };

        let sent = json(Workspace::from_core(core.clone(), &sessions));

        assert_eq!(
            sent,
            r#"{"chat":[{"agent":"Codex","session":"4","shown":false}],"panels":["Files","Logs"],"selected":"Logs","terminal":true}"#
        );
        let back: Workspace = serde_json::from_str(&sent).unwrap();
        assert_eq!(back.into_core(&sessions), Ok(core));
    }

    #[test]
    fn a_listed_agent_session_reaches_the_ui_with_its_times_and_is_remembered() {
        let sessions = Sessions(Mutex::default());
        let id = |n: u64| serde_json::from_value::<SessionId>(serde_json::json!(n)).unwrap();
        let at = |ms: u64| UNIX_EPOCH + std::time::Duration::from_millis(ms);
        let info = |state| grove_core::AgentSessionInfo {
            session: id(7),
            worktree: "/repo/feat".into(),
            agent: grove_core::Agent::Codex,
            mode: grove_core::Mode::Terminal,
            parent: Some(id(3)),
            title: "fix the login bug".into(),
            created: at(1_763_560_920_000),
            ended: true,
            state,
            since: at(1_763_560_983_000),
        };

        let ended = json(AgentSession::from_core(
            info(Some(grove_core::AgentState::Exited)),
            &sessions,
        ));
        let no_state_yet = json(AgentSession::from_core(info(None), &sessions));

        assert_eq!(
            ended,
            r#"{"session":"7","worktree":"/repo/feat","agent":"Codex","mode":"Terminal","parent":"3","title":"fix the login bug","createdUnixMs":1763560920000,"ended":true,"state":"Exited","sinceUnixMs":1763560983000}"#
        );
        assert_eq!(
            no_state_yet,
            r#"{"session":"7","worktree":"/repo/feat","agent":"Codex","mode":"Terminal","parent":"3","title":"fix the login bug","createdUnixMs":1763560920000,"ended":true,"sinceUnixMs":1763560983000}"#
        );
        // Every call on a listed session finds it, one the UI never started too.
        assert_eq!(lock(&sessions).get("7"), Some(&id(7)));
    }

    #[test]
    fn a_workspace_naming_a_session_grove_never_started_is_refused() {
        let sessions = Sessions(Mutex::default());
        let from_ui = r#"{"chat":[{"agent":"Claude","session":"7","shown":true}],"panels":[],"selected":null,"terminal":false}"#;

        let workspace: Workspace = serde_json::from_str(from_ui).unwrap();

        assert_eq!(
            workspace.into_core(&sessions),
            Err("grove has no session 7".to_string())
        );
    }

    fn json(body: impl IpcResponse) -> String {
        match body.body() {
            Ok(InvokeResponseBody::Json(json)) => json,
            _ => panic!("it goes to the UI as JSON"),
        }
    }

    #[test]
    fn chat_messages_reach_the_ui_as_bridge_ts_reads_them() {
        let message = |m| json(vec![Message::from(m)]);

        assert_eq!(
            message(AgentMessage::User {
                text: "add a test".into()
            }),
            r#"[{"type":"User","text":"add a test"}]"#
        );
        assert_eq!(
            message(AgentMessage::TurnStarted {
                time_unix_ms: Some(1_763_560_920_000)
            }),
            r#"[{"type":"TurnStarted","timeUnixMs":1763560920000}]"#
        );
        assert_eq!(
            message(AgentMessage::ToolCall {
                id: "toolu_03".into(),
                tool: "Read".into(),
                input: serde_json::json!({"file_path": "a.rs"}),
                time_unix_ms: Some(1_763_560_921_500),
            }),
            r#"[{"type":"ToolCall","id":"toolu_03","tool":"Read","input":{"file_path":"a.rs"},"timeUnixMs":1763560921500}]"#
        );
        assert_eq!(
            message(AgentMessage::ToolCall {
                id: "call_1".into(),
                tool: "commandExecution".into(),
                input: serde_json::json!({}),
                time_unix_ms: None,
            }),
            r#"[{"type":"ToolCall","id":"call_1","tool":"commandExecution","input":{},"timeUnixMs":null}]"#
        );
        assert_eq!(
            message(AgentMessage::TextDelta {
                text: "I'll".into()
            }),
            r#"[{"type":"TextDelta","text":"I'll"}]"#
        );
        assert_eq!(
            message(AgentMessage::ApprovalRequested {
                request: RequestId("toolu_01".into()),
                tool: "Bash".into(),
                input: serde_json::json!({"command": "npm test"}),
            }),
            r#"[{"type":"ApprovalRequested","request":"toolu_01","tool":"Bash","input":{"command":"npm test"}}]"#
        );
        assert_eq!(
            message(AgentMessage::QuestionAsked {
                request: RequestId("toolu_02".into()),
                questions: vec![grove_core::Question {
                    question: "Which test runner?".into(),
                    header: "Runner".into(),
                    options: vec![grove_core::QuestionOption {
                        label: "vitest".into(),
                        description: "Fast".into(),
                    }],
                    multi_select: true,
                }],
            }),
            r#"[{"type":"QuestionAsked","request":"toolu_02","questions":[{"question":"Which test runner?","header":"Runner","options":[{"label":"vitest","description":"Fast"}],"multiSelect":true}]}]"#
        );
        assert_eq!(
            message(AgentMessage::Answered {
                request: RequestId("toolu_02".into()),
                reply: grove_core::Reply::AllowOnce,
            }),
            r#"[{"type":"Answered","request":"toolu_02","reply":{"type":"AllowOnce"}}]"#
        );
        assert_eq!(
            message(AgentMessage::Answered {
                request: RequestId("toolu_02".into()),
                reply: grove_core::Reply::Answers(vec![vec!["unit".into(), "e2e".into()]]),
            }),
            r#"[{"type":"Answered","request":"toolu_02","reply":{"type":"Answers","answers":[["unit","e2e"]]}}]"#
        );
        assert_eq!(
            message(AgentMessage::Answered {
                request: RequestId("toolu_02".into()),
                reply: grove_core::Reply::Deny { message: None },
            }),
            r#"[{"type":"Answered","request":"toolu_02","reply":{"type":"Deny","message":null}}]"#
        );
        assert_eq!(
            message(AgentMessage::TurnEnded {
                outcome: TurnOutcome::Interrupted,
                time_unix_ms: Some(1_763_560_983_000),
            }),
            r#"[{"type":"TurnEnded","timeUnixMs":1763560983000,"outcome":"Interrupted"}]"#
        );
        assert_eq!(
            message(AgentMessage::TurnEnded {
                outcome: TurnOutcome::Failed {
                    kind: "overloaded".into(),
                    message: "API Error: 529 overloaded".into(),
                },
                time_unix_ms: None,
            }),
            r#"[{"type":"TurnEnded","timeUnixMs":null,"outcome":"Failed","kind":"overloaded","message":"API Error: 529 overloaded"}]"#
        );
        assert_eq!(
            message(AgentMessage::ReviewBatch {
                comments: vec![grove_core::BatchComment {
                    path: "src/a.rs".into(),
                    start_line: 10,
                    end_line: 11,
                    text: "line 10\nline 11".into(),
                    body: "split this".into(),
                }]
            }),
            r#"[{"type":"ReviewBatch","comments":[{"path":"src/a.rs","startLine":10,"endLine":11,"text":"line 10\nline 11","body":"split this"}]}]"#
        );
        assert_eq!(
            message(AgentMessage::NotDelivered {
                text: "add a test".into()
            }),
            r#"[{"type":"NotDelivered","text":"add a test"}]"#
        );
    }

    #[test]
    fn review_comments_reach_the_ui_as_bridge_ts_reads_them_and_new_ones_come_back() {
        let sent = json(vec![ReviewComment::from(grove_core::ReviewComment {
            id: serde_json::from_str("7").unwrap(),
            path: "src/a.rs".into(),
            start_line: 10,
            end_line: 11,
            text: "line 10\nline 11".into(),
            body: "split this".into(),
            state: grove_core::CommentState::Sent,
            possibly_addressed: true,
        })]);
        assert_eq!(
            sent,
            r#"[{"id":7,"path":"src/a.rs","startLine":10,"endLine":11,"text":"line 10\nline 11","body":"split this","state":"Sent","possiblyAddressed":true}]"#
        );

        let new: NewReviewComment =
            serde_json::from_str(r#"{"path":"src/a.rs","startLine":3,"endLine":4,"body":"why?"}"#)
                .unwrap();
        assert_eq!(
            grove_core::NewReviewComment::from(new),
            grove_core::NewReviewComment {
                path: "src/a.rs".into(),
                start_line: 3,
                end_line: 4,
                body: "why?".into(),
            }
        );
    }

    #[test]
    fn replies_from_the_ui_reach_the_core_as_bridge_ts_sends_them() {
        let reply =
            |body: &str| grove_core::Reply::from(serde_json::from_str::<Reply>(body).unwrap());

        assert_eq!(
            reply(r#"{"type":"AllowOnce"}"#),
            grove_core::Reply::AllowOnce
        );
        assert_eq!(
            reply(r#"{"type":"AllowForSession"}"#),
            grove_core::Reply::AllowForSession
        );
        assert_eq!(
            reply(r#"{"type":"Deny","message":null}"#),
            grove_core::Reply::Deny { message: None }
        );
        assert_eq!(
            reply(r#"{"type":"Deny","message":"use pnpm"}"#),
            grove_core::Reply::Deny {
                message: Some("use pnpm".into())
            }
        );
        assert_eq!(
            reply(r#"{"type":"Answers","answers":[["vitest"],["unit","e2e"]]}"#),
            grove_core::Reply::Answers(vec![
                vec!["vitest".into()],
                vec!["unit".into(), "e2e".into()]
            ])
        );
    }

    #[test]
    fn tool_messages_reach_the_ui_as_bridge_ts_reads_them() {
        let call = AgentMessage::ToolCall {
            id: "toolu_01".into(),
            tool: "Edit".into(),
            input: serde_json::json!({"file_path": "src/a.ts"}),
            time_unix_ms: Some(1_763_560_921_500),
        };
        let result = AgentMessage::ToolResult {
            id: "toolu_01".into(),
            output: "ok".into(),
            is_error: false,
        };

        assert_eq!(
            json(vec![Message::from(call), Message::from(result)]),
            r#"[{"type":"ToolCall","id":"toolu_01","tool":"Edit","input":{"file_path":"src/a.ts"},"timeUnixMs":1763560921500},{"type":"ToolResult","id":"toolu_01","output":"ok","isError":false}]"#
        );
    }

    #[test]
    fn a_mode_from_the_ui_reaches_the_core_as_bridge_ts_sends_it() {
        let modes: Vec<Mode> = serde_json::from_str(r#"["Chat", "Terminal"]"#).unwrap();
        let modes: Vec<grove_core::Mode> = modes.into_iter().map(Into::into).collect();
        assert_eq!(modes, [grove_core::Mode::Chat, grove_core::Mode::Terminal]);
    }

    #[test]
    fn an_account_reaches_the_ui_as_bridge_ts_reads_it() {
        let account = grove_core::AccountInfo {
            email: Some("ann@example.com".into()),
            organization: None,
            plan: Some("max".into()),
            api_key_source: None,
        };
        let change = |account| AccountChanged {
            agent: Agent::Claude,
            account,
        };

        assert_eq!(
            json(change(Some(account.into()))),
            r#"{"agent":"Claude","account":{"email":"ann@example.com","organization":null,"plan":"max","apiKeySource":null}}"#
        );
        assert_eq!(json(change(None)), r#"{"agent":"Claude","account":null}"#);
    }

    #[test]
    fn a_model_reaches_the_ui_as_bridge_ts_reads_it() {
        let model = ModelInfo {
            id: "gpt-5.5".into(),
            name: "GPT-5.5".into(),
            description: "Frontier model".into(),
            efforts: vec!["low".into(), "medium".into()],
            default_effort: Some("medium".into()),
            is_default: true,
        };

        assert_eq!(
            json(vec![Model::from(model)]),
            r#"[{"id":"gpt-5.5","name":"GPT-5.5","description":"Frontier model","efforts":["low","medium"],"defaultEffort":"medium","isDefault":true}]"#
        );
    }

    #[test]
    fn a_codex_account_reaches_the_ui_as_bridge_ts_reads_it() {
        let change = AccountChanged {
            agent: grove_core::Agent::Codex.into(),
            account: None,
        };

        assert_eq!(json(change), r#"{"agent":"Codex","account":null}"#);
    }

    #[test]
    fn a_warning_reaches_the_ui_as_bridge_ts_reads_it() {
        let sent = WarningSent {
            id: 2,
            warning: Warning::LoginShellEnv {
                reason: "/bin/zsh did not print its environment within 5s".into(),
            },
        };

        match sent.body() {
            Ok(InvokeResponseBody::Json(json)) => assert_eq!(
                json,
                r#"{"id":2,"kind":"LoginShellEnv","reason":"/bin/zsh did not print its environment within 5s"}"#
            ),
            _ => panic!("a warning goes to the UI as JSON"),
        }
    }

    #[test]
    fn a_session_that_fails_to_start_says_why() {
        let error = grove_core::CoreError::Pty {
            action: "start the session's command",
            source: "Unable to spawn claude because it doesn't exist on the filesystem".into(),
        };

        assert_eq!(
            with_causes(&error),
            "could not start the session's command: \
             Unable to spawn claude because it doesn't exist on the filesystem"
        );
    }

    #[test]
    fn binary_versions_never_reach_the_ui() {
        let core = grove_core::FileVersions {
            old: Some(vec![0x89, b'P', b'N', b'G']),
            new: Some(b"text".to_vec()),
            binary: true,
        };

        let versions = FileVersions::from(core);

        assert_eq!(
            (versions.old, versions.new, versions.binary),
            (None, None, true)
        );
    }

    #[test]
    fn no_notification_while_the_window_is_focused() {
        let worktree = Path::new("/code/grove-wt/feat-36");
        assert_eq!(
            notification(true, worktree, grove_core::AgentState::Failed),
            None
        );
    }

    #[test]
    fn a_notification_names_the_worktree_and_says_what_the_session_needs() {
        let worktree = Path::new("/code/grove-wt/feat-36");
        let approval = grove_core::AgentState::NeedsInput {
            kind: grove_core::InputKind::Approval,
        };
        assert_eq!(
            notification(false, worktree, approval),
            Some(("feat-36".into(), "An agent needs your approval".into()))
        );
        assert_eq!(
            notification(false, worktree, grove_core::AgentState::Failed),
            Some(("feat-36".into(), "An agent failed".into()))
        );
    }
}
