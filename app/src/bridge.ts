import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";

/** A worktree grove tracks, as the core reports it. */
export interface Worktree {
  path: string;
  /** null when HEAD is detached. */
  branch: string | null;
  /** The repo's original checkout. */
  isMain: boolean;
  /** Defaults to the branch, or to the folder name when HEAD is detached. */
  displayName: string;
  issueLink: string | null;
  /** What a whole-branch diff compares against. null when the repo has no default branch. */
  baseBranch: string | null;
  /**
   * The worktree of the same repo whose agent session first started a subagent here, which makes
   * this a nested worktree. It stays after both sessions end, and goes when that worktree leaves
   * grove. null for one that nests under nothing, and always for a main worktree.
   */
  nestedUnder: string | null;
  /** The setup script grove ran after adding it. null when it ran none since grove started. */
  setup: SetupState | null;
}

/** A setup script's state. A failed one's `code` is null when a signal ended it. */
export type SetupState = { state: "Running" } | { state: "Succeeded" } | { state: "Failed"; code: number | null };

export interface SetupChanged {
  worktree: string;
  setup: SetupState;
}

/** A worktree to add with `createWorktree`. null takes the core's default. */
export interface NewWorktree {
  /** A new name makes a branch from the base branch. An existing one is checked out. */
  branch: string;
  /** null means the branch name. */
  displayName: string | null;
  issueLink: string | null;
  /** null means the repo's default branch: `origin/HEAD`, then `main`, then `master`. */
  baseBranch: string | null;
}

/** The profile grove suggests for a repo, and the marker file the repo has for it. */
export interface ProfileSuggestion {
  profile: string;
  /** As the profile lists it, such as `Cargo.toml`. */
  marker: string;
}

/** A profile's file, as the Profile panel shows it. */
export interface ProfileText {
  /** Under `~/.grovecode/profiles/`. */
  path: string;
  /** As grove last read or wrote it, comments included. */
  text: string;
}

/** Why the core refused a profile's or overrides' text. `line` is from 1, null when no line holds it. */
export interface TextProblem {
  line: number | null;
  message: string;
}

/** What `setProfileText` or `setRepoOverridesText` rejected with. A command that fails before the core runs rejects with a string. */
export function textProblemOf(error: unknown): TextProblem {
  if (typeof error === "object" && error !== null && "message" in error) {
    const { line, message } = error as Partial<TextProblem>;
    return { line: typeof line === "number" ? line : null, message: String(message) };
  }
  return { line: null, message: String(error) };
}

/** What deleting a worktree would lose, and whether its branch may go with it. */
export interface DeleteCheck {
  /** Uncommitted changes, untracked files included. */
  uncommitted: boolean;
  /** Commits on no remote and not in the base branch. */
  unpushed: number;
  /** null when HEAD is detached. */
  branch: string | null;
  /** The branch is merged into the base branch, so deleting it is offered. */
  merged: boolean;
}

/** What deleting a worktree did with its branch. */
export type BranchOutcome =
  /** HEAD was detached, so there was no branch. */
  | { outcome: "Untouched" }
  | { outcome: "Kept"; name: string; merged: boolean }
  | { outcome: "Deleted"; name: string }
  /** The worktree is gone, but `git branch -D` failed. */
  | { outcome: "NotDeleted"; name: string; reason: string };

/** What a diff compares the worktree against. */
export type DiffBase = "Head" | "MergeBase";

export type FileStatus = "Added" | "Deleted" | "Modified" | "Renamed" | "Untracked";

/** One file's change, with its hunks counted rather than sent. */
export interface FileDiff {
  status: FileStatus;
  /** Relative to the worktree. The same as `newPath` unless the file was renamed. */
  oldPath: string;
  newPath: string;
  /** As git decides it. A binary file counts no lines. */
  binary: boolean;
  added: number;
  removed: number;
}

/** Both versions of a file in a diff. null where the file does not exist, and both null when binary. */
export interface FileVersions {
  old: string | null;
  new: string | null;
  /** git calls the file binary, or a version is not UTF-8. */
  binary: boolean;
}

export type Editor = "Zed" | "VsCode";

/** A session the core started, by the id the core gave it. */
export type SessionId = string;

/** An agent CLI grove runs. */
export type Agent = "Claude" | "Codex";

/** How an agent session runs. GLOSSARY.md has the meanings. */
export type Mode = "Chat" | "Terminal";

/** A model an agent offers, as `Model` in `app/src-tauri/src/main.rs` sends it. */
export interface Model {
  /** What `setModel` takes. */
  id: string;
  name: string;
  description: string;
  /** The efforts it takes, in the agent's order. Empty when it takes none. */
  efforts: string[];
  /** null when the agent does not say. */
  defaultEffort: string | null;
  /** The agent's own default. */
  isDefault: boolean;
}

/** The model and effort a session runs with, as `ModelChoice` in grove-core saves them. */
export interface SavedModelChoice {
  /** A `Model` id. */
  model: string;
  effort: string | null;
}

/** A session in a worktree, as `SessionInfo` in `app/src-tauri/src/main.rs` sends it. */
export type SessionInfo = {
  session: SessionId;
  /** False only for a run session that was running when grove quit, which comes back stopped. */
  running: boolean;
} & (
  | { kind: "Agent"; agent: Agent; mode: Mode }
  | { kind: "Shell" }
  /** One of the repo's run sessions, by name. */
  | { kind: "Run"; name: string }
);

/** One of a repo's run sessions: a named command from its profile or its overrides. */
export interface RunSession {
  /** Unique among the repo's run sessions. */
  name: string;
  command: string;
  /** Where it serves, for the port router. */
  port: number | null;
}

/** What an agent session is doing. GLOSSARY.md has the meanings. */
export type AgentState =
  | { state: "Working" | "Failed" | "Done" | "Idle" | "Exited" }
  | { state: "NeedsInput"; kind: "Approval" | "Question" };

/** Where an agent state came from: Protocol in chat mode; in terminal mode Hook beats Heuristic. */
export type StateSource = "Protocol" | "Hook" | "Heuristic";

/** An agent session's new state, as `AgentStateChanged` in `app/src-tauri/src/main.rs` sends it. */
export type AgentStateChanged = AgentState & { session: SessionId; source: StateSource };

/**
 * An agent session, running or ended, as `AgentSession` in `app/src-tauri/src/main.rs` sends it. It
 * has no state before its first one. Times are in milliseconds since the Unix epoch.
 */
export type AgentSessionRecord = {
  session: SessionId;
  worktree: string;
  agent: Agent;
  mode: Mode;
  /** The agent session that spawned it as a subagent. null for one the user started. */
  parent: SessionId | null;
  /** The first line of its first user message, or "New session" before that. */
  title: string;
  createdUnixMs: number;
  ended: boolean;
  /** When it entered its state, or before its first state when it started. */
  sinceUnixMs: number;
} & (AgentState | { state?: undefined });

/** The macOS notification of an agent session that the user clicked. */
export type NotificationClicked = { session: SessionId; worktree: string };

/** A problem the core worked around, so grove runs with less than it should. */
export type Warning =
  | {
      kind: "LoginShellEnv";
      /** Why the login shell's environment could not be read, so sessions got the minimal one. */
      reason: string;
    }
  /** Codex runs none of grove's hooks in this agent session, so its state comes from heuristics. The user approves them in Codex's `/hooks`. */
  | { kind: "CodexHooksNotTrusted"; session: SessionId; worktree: string }
  /** A session that was running when grove quit could not start again, so it is gone with its panel. */
  | { kind: "SessionNotRestored"; worktree: string; reason: string }
  /** A worktree's watcher could not start, so its diff does not update live until `retryWatcher` starts it. */
  | { kind: "WatcherNotStarted"; worktree: string; reason: string };

/** A warning as `WarningSent` in `app/src-tauri/src/main.rs` sends it. `id` tells one from another. */
export type WarningSent = Warning & { id: number };

/** Who is logged in to an agent, as the agent reports it. Any field may be null. */
export interface Account {
  email: string | null;
  organization: string | null;
  /** The subscription plan, such as "max". null with an API key. */
  plan: string | null;
  /** Where the API key came from, such as "ANTHROPIC_API_KEY". null with a subscription login. */
  apiKeySource: string | null;
}

/** An agent's login, as `AccountChanged` in `app/src-tauri/src/main.rs` sends it. null when nobody is logged in. */
export interface AccountChanged {
  agent: Agent;
  account: Account | null;
}

/** How a turn ended. A failed turn has the agent's own name for the error, such as "rate_limit". */
export type TurnOutcome =
  | { outcome: "Completed" | "Interrupted" }
  | { outcome: "Failed"; kind: string; message: string };

/** One question of a `QuestionAsked`, as `Question` in `app/src-tauri/src/main.rs` sends it. */
export interface Question {
  question: string;
  /** A short label for it, such as "Runner". */
  header: string;
  options: { label: string; description: string }[];
  /** The user may pick more than one option. */
  multiSelect: boolean;
}

/** The user's answer to a pending request, as `Reply` in `app/src-tauri/src/main.rs` reads and sends it. */
export type Reply =
  /** Approvals only. AllowForSession stops the agent asking for calls like this one. */
  | { type: "AllowOnce" | "AllowForSession" }
  /** Either kind. `message` tells the agent why, or what to do instead. */
  | { type: "Deny"; message: string | null }
  /** Questions only. One answer per question, in order: the chosen options' labels, or the user's own text. */
  | { type: "Answers"; answers: string[][] };

/**
 * One message of a chat-mode conversation, as `Message` in `app/src-tauri/src/main.rs` sends it.
 * A tool result pairs with its call by `id`, and an answer with its request by `request`. A
 * request still pending when its turn ends ends with it. `timeUnixMs` is when the core got the
 * message, or when the agent's record says it happened, in milliseconds since the Unix epoch.
 * null when a record does not say, as for a restored Codex tool call.
 */
export type AgentMessage =
  | { type: "User"; text: string }
  | { type: "TurnStarted"; timeUnixMs: number | null }
  | { type: "TextDelta"; text: string }
  | { type: "ToolCall"; id: string; tool: string; input: unknown; timeUnixMs: number | null }
  | { type: "ToolResult"; id: string; output: string; isError: boolean }
  | { type: "ApprovalRequested"; request: string; tool: string; input: unknown }
  | { type: "QuestionAsked"; request: string; questions: Question[] }
  | { type: "Answered"; request: string; reply: Reply }
  | ({ type: "TurnEnded"; timeUnixMs: number | null } & TurnOutcome)
  | { type: "ReviewBatch"; comments: BatchComment[] }
  /** A message the user sent never reached the agent: its process ended first. `text` is what the agent would have got. */
  | { type: "NotDelivered"; text: string }
  /**
   * The commands and skills the agent offers now, in place of any list before. Never part of the
   * conversation: a subscribe gets the latest list after the history, and frames bring changes.
   */
  | { type: "Commands"; commands: SlashCommand[] };

/** A slash command or skill the agent offers, as `SlashCommand` in `app/src-tauri/src/main.rs` sends it. */
export interface SlashCommand {
  name: string;
  description: string;
  /** What it takes, such as `<pr>`. Empty when it takes nothing or the agent does not say. */
  argumentHint: string;
  /** What the user types to run it: `/review` for Claude, `$review` for a Codex skill. */
  invocation: string;
}

/** One review comment of a review batch, as sent, with control characters but newline and tab stripped. */
export interface BatchComment {
  /** Relative to the worktree. */
  path: string;
  startLine: number;
  endLine: number;
  /** The lines it is on, joined by `\n`. */
  text: string;
  body: string;
}

/** Names a review comment for as long as grove keeps it, across restarts. */
export type CommentId = number;

/** A comment for `addComment`, on lines of the worktree's current version of a file. Lines count from 1. */
export interface NewReviewComment {
  /** Relative to the worktree. */
  path: string;
  startLine: number;
  endLine: number;
  body: string;
}

/** A review comment, as `ReviewComment` in `app/src-tauri/src/main.rs` sends it. */
export interface ReviewComment {
  id: CommentId;
  /** Where its text is now, or was last seen. Relative to the worktree. */
  path: string;
  startLine: number;
  endLine: number;
  /** The lines it is on, as they were when it was added, joined by `\n`. */
  text: string;
  body: string;
  /** Sent: it went to an agent session in a review batch. Resolved: hidden, but kept. */
  state: "Open" | "Sent" | "Resolved";
  /** Its text is gone, its file is deleted, or its file was renamed in a way git does not detect. */
  possiblyAddressed: boolean;
}

/** An agent session waiting on the user, as `AttentionEntry` in `app/src-tauri/src/main.rs` sends it. */
export interface AttentionEntry {
  session: SessionId;
  worktree: string;
}

/** A batch of an attached session's output. A session numbers its frames from 1, with no gaps. */
export interface OutputFrame {
  seq: number;
  bytes: Uint8Array;
}

/** A session's terminal when a panel attaches, as bytes for a reset xterm.js to write. */
export interface Snapshot {
  /** The last frame it covers. Live frames start at `seq + 1`. 0 if no frame has gone out yet. */
  seq: number;
  /** The size the core wrote the bytes for, in cells. */
  cols: number;
  rows: number;
  /** Scrollback, screen, cursor and modes, ending with any unfinished escape sequence. */
  bytes: Uint8Array;
}

/** A file's lines changed since HEAD, for the editor's gutter. Lines are 1-based, in the file as it is now. */
export type LineChange =
  | { kind: "Added" | "Modified"; start: number; end: number }
  /** Lines were removed after line `after`. 0 means above the first line. */
  | { kind: "Deleted"; after: number };

/** A panel the panel area can show as a tab. */
export type PanelKind = "Files" | "Diff" | "Browser" | "Logs" | "Profile";

/** An agent panel in the chat area, as `AgentPanel` in `app/src-tauri/src/main.rs` sends and reads it. */
export interface AgentPanel {
  agent: Agent;
  /** null until the UI starts its agent session. */
  session: SessionId | null;
  /** A hidden panel's session keeps running. */
  shown: boolean;
}

/** One worktree's layout, as `Workspace` in `app/src-tauri/src/main.rs` sends and reads it. */
export interface Workspace {
  /** The agent panels in the chat area, in order. */
  chat: AgentPanel[];
  /** The panel area's tabs, in order. */
  panels: PanelKind[];
  /** One of `panels`. null shows no tab. */
  selected: PanelKind | null;
  /** Whether the terminal panel is open at the bottom of the panel area. */
  terminal: boolean;
}

/**
 * A change an agent session made through `grove-mcp`, as `AgentChange` in `app/src-tauri/src/main.rs`
 * sends it. It comes before the change's own `workspace-changed` or `profiles-changed`.
 */
/** A Dock icon grove bundles: the app icon's style, in its light or dark version (#55). */
export type DockIcon = "mono-dark" | "mono-light" | "blue-dark" | "blue-light";

export type AgentChange = { kind: "Workspace"; worktree: string; by: SessionId } | { kind: "Profiles"; by: SessionId };

/** Everything the UI asks of the core. The app uses `tauriBridge`; tests use `fakeBridge`. */
export interface Bridge {
  /** Adds the repo that `path` is in and adopts every worktree in its `git worktree list`. */
  addRepo(path: string): Promise<void>;
  /** Opens the system folder picker. The picked folder's path, or null when cancelled. */
  pickFolder(): Promise<string | null>;
  /** Every tracked worktree, repo by repo in the order they were added, main worktree first. */
  listWorktrees(): Promise<Worktree[]>;
  /** The worktree's diff against `base`, file by file in path order. */
  diff(worktree: string, base: DiffBase): Promise<FileDiff[]>;
  fileVersions(worktree: string, base: DiffBase, file: FileDiff): Promise<FileVersions>;
  /** Calls `listener` with a worktree's path when its diff may have changed. Returns the unsubscribe. */
  onDiffChanged(listener: (worktree: string) => void): () => void;
  /** Opens `file`, relative to `worktree`, in `editor` at `line` (1-based). An empty `file` opens the worktree itself. */
  openInEditor(editor: Editor, worktree: string, file: string, line: number): Promise<void>;
  /** Shows the worktree's folder in Finder. */
  revealInFinder(worktree: string): Promise<void>;
  /** Opens an `http` or `https` link in the user's default browser. The core refuses any other link. */
  openLinkInDefaultBrowser(link: string): Promise<void>;
  /**
   * The worktree's shell: the one running there, at the size it has, or the user's shell started
   * now in a terminal of `cols` by `rows` cells.
   */
  spawnShell(worktree: string, cols: number, rows: number): Promise<SessionId>;
  /**
   * Starts the run session `name` of the worktree's repo in the worktree, in a terminal of `cols`
   * by `rows` cells. A stopped one of that name that a restart brought back there gives way to it.
   */
  spawnRun(worktree: string, name: string, cols: number, rows: number): Promise<SessionId>;
  /** Calls `listener` with a session's id when its process ends. Returns the unsubscribe. */
  onSessionExited(listener: (session: SessionId) => void): () => void;
  /**
   * Starts an agent session in chat mode in the worktree, in agent panel `panel` of its workspace
   * (an index into `chat`), and saves the workspace. With null, or a panel that has a session, it
   * goes in a new shown panel after the last one. A panel past the last one fails and starts
   * nothing.
   */
  spawnAgent(worktree: string, agent: Agent, panel: number | null): Promise<SessionId>;
  /**
   * The worktree's agent, shell and run sessions, oldest first: those running, such as the ones a
   * restart brought back, and the run sessions that were running when grove quit, now stopped.
   */
  sessions(worktree: string): Promise<SessionInfo[]>;
  /**
   * Sends the chat-mode session's messages to `onMessages` a frame at a time, in place of any
   * earlier subscribe. Returns every message before the first frame, which can arrive before the
   * history does. Each message is in exactly one of the two.
   */
  subscribe(session: SessionId, onMessages: (messages: AgentMessage[]) => void): Promise<AgentMessage[]>;
  /** Sends the user's message to the chat-mode session, which starts a turn. */
  sendMessage(session: SessionId, text: string): Promise<void>;
  /** Asks the chat-mode session's agent to stop the turn. */
  interrupt(session: SessionId): Promise<void>;
  /**
   * Asks the chat-mode session's agent a side question in a fork of its conversation, in place of
   * any earlier side question of the session. The answer goes to `onMessages` a frame at a time
   * and never joins the conversation. Returns the side question's id. Fails in terminal mode.
   */
  askSideQuestion(
    session: SessionId,
    question: string,
    onMessages: (messages: AgentMessage[]) => void,
  ): Promise<number>;
  /** Ends the side question and its fork. Does nothing once it has ended. */
  endSideQuestion(session: SessionId, sideQuestion: number): Promise<void>;
  /**
   * Answers a pending approval or question of the chat-mode session. Fails when the request is no
   * longer pending or the reply does not fit it.
   */
  answer(session: SessionId, request: string, reply: Reply): Promise<void>;
  /**
   * The models the chat-mode session's agent offers, once it has listed them. Fails if the
   * session ends first.
   */
  models(session: SessionId): Promise<Model[]>;
  /**
   * Sets the chat-mode session's model, and its effort unless null, from the next turn on. A
   * running turn keeps its own.
   */
  setModel(session: SessionId, model: string, effort: string | null): Promise<void>;
  /**
   * The model and effort `setModel` last saved for the agent session. null for a session with no
   * choice yet, which runs on its agent's default.
   */
  modelChoice(session: SessionId): Promise<SavedModelChoice | null>;
  /**
   * Calls `listener` with each agent's login as the agent reports it, the latest before the call
   * too. Returns the unsubscribe.
   */
  onAccount(listener: (change: AccountChanged) => void): () => void;
  /** Calls `listener` each time an agent session's state changes. Returns the unsubscribe. */
  onAgentState(listener: (change: AgentStateChanged) => void): () => void;
  /** Every agent session of every tracked worktree, ended ones too. */
  agentSessions(): Promise<AgentSessionRecord[]>;
  /**
   * Called when what `agentSessions` lists changed: a session started, got its title, switched
   * mode, ended, was resumed or was deleted. A new state comes through `onAgentState` alone.
   */
  onAgentSessionsChanged(listener: () => void): () => void;
  /** Deletes the agent session and every subagent session below it, for good. */
  deleteAgentSession(session: SessionId): Promise<void>;
  /**
   * Starts the ended agent session again under the same id, resuming its conversation. It shows in
   * the agent panel it had, or in a new one after the last. A running one stays as it is.
   */
  resumeAgentSession(session: SessionId): Promise<void>;
  /**
   * Calls `listener` each time the user clicks an agent session's notification, after grove came
   * forward. Returns the unsubscribe.
   */
  onNotificationClicked(listener: (clicked: NotificationClicked) => void): () => void;
  /** Lists the active warnings at startup and whenever the core changes them. */
  onWarnings(listener: (warnings: WarningSent[]) => void): () => void;
  /** Starts the worktree's watcher after a `WatcherNotStarted` warning. Fails with the new cause. */
  retryWatcher(worktree: string): Promise<void>;
  /** Tells the core the session's panel got focus, so a `Done` agent session becomes `Idle`. */
  focus(session: SessionId): Promise<void>;
  /** Sends input to the session's terminal. */
  write(session: SessionId, bytes: Uint8Array): Promise<void>;
  resize(session: SessionId, cols: number, rows: number): Promise<void>;
  /**
   * Sends the session's output to `onFrame` frame by frame, in place of any earlier attach.
   * Returns a snapshot of the session's terminal up to the first frame, which can arrive before the
   * snapshot does. The session's reader pauses while too many bytes are unacked. The snapshot's
   * bytes never count.
   */
  attach(session: SessionId, onFrame: (frame: OutputFrame) => void): Promise<Snapshot>;
  /** Counts `bytes` of the session's output as drawn. */
  ack(session: SessionId, bytes: number): Promise<void>;
  /** Stops sending the session's output. The session keeps running. */
  detach(session: SessionId): Promise<void>;
  /** Ends the session's process and everything it started. */
  kill(session: SessionId): Promise<void>;

  // Files panel (#124).
  /** Every file in the worktree, relative to it and sorted: tracked ones, and untracked ones not ignored. */
  listFiles(worktree: string): Promise<string[]>;
  /** The bytes of `file`, relative to the worktree. */
  readFile(worktree: string, file: string): Promise<Uint8Array>;
  /** Replaces `file`, relative to the worktree, with `text` as UTF-8. Readers never see part of it. */
  writeFile(worktree: string, file: string, text: string): Promise<void>;
  /** The lines of `file` changed since HEAD, from the Head diff. Empty when it has no change. */
  lineChanges(worktree: string, file: string): Promise<LineChange[]>;

  // Attention queue (#53).
  /**
   * The agent sessions waiting on the user, head first: approvals, then questions, then `Failed`,
   * then `Done`, each oldest first.
   */
  attentionQueue(): Promise<AttentionEntry[]>;

  // Workspaces (#122).
  /**
   * The worktree's workspace: the saved one, or its profile's layout. A panel whose session no
   * longer runs is left out.
   */
  workspace(worktree: string): Promise<Workspace>;
  /** Saves the worktree's workspace. Fails when its selected tab is not one of its tabs. */
  setWorkspace(worktree: string, workspace: Workspace): Promise<void>;
  /** Calls `listener` with a worktree's path when its workspace changes. Returns the unsubscribe. */
  onWorkspaceChanged(listener: (worktree: string) => void): () => void;
  // The agent change notice (#4).
  /** Calls `listener` with each change an agent session made. Returns the unsubscribe. */
  onAgentChange(listener: (change: AgentChange) => void): () => void;
  /**
   * Restores the worktree's workspace from before the last change an agent session made to it,
   * keeping a panel for each session placed since. Fails when there is nothing to undo.
   */
  undoWorkspace(worktree: string): Promise<void>;
  /**
   * Writes the worktree's shown agent panels, its tabs and its terminal into its repo's profile.
   * The selected tab and hidden panels are left out. Fails when the repo uses no profile.
   */
  saveWorkspaceToProfile(worktree: string): Promise<void>;

  // Worktree management (#52).
  /** Adds a worktree to the repo whose main worktree is at `repo`, with `git worktree add`. */
  createWorktree(repo: string, worktree: NewWorktree): Promise<Worktree>;
  /** Changes the worktree's base branch. The branch must exist locally or on `origin`. */
  setBaseBranch(worktree: string, base: string): Promise<void>;
  /** Stops tracking the worktree and ends its sessions. Its folder and branch stay. Refused for a main worktree. */
  forgetWorktree(worktree: string): Promise<void>;
  /** What deleting the worktree would lose, and whether its branch is merged. */
  deleteCheck(worktree: string): Promise<DeleteCheck>;
  /**
   * Ends the worktree's sessions, removes it from disk and stops tracking it. Without `confirmed`
   * it is refused when the worktree has uncommitted changes or commits on no remote.
   * `deleteBranch` is refused, before anything is removed, when the branch is not merged.
   */
  deleteWorktree(worktree: string, confirmed: boolean, deleteBranch: boolean): Promise<BranchOutcome>;
  /** Every valid profile's name, sorted. Fails when a profile file is not valid. */
  profileNames(): Promise<string[]>;
  /** The profile whose marker file the repo has. null when none has one there. */
  suggestProfile(repo: string): Promise<ProfileSuggestion | null>;
  /** The profile the repo uses. null when it uses none. */
  repoProfile(repo: string): Promise<string | null>;
  /** Sets the profile the repo uses, keeping its overrides. null uses none. */
  setRepoProfile(repo: string, profile: string | null): Promise<void>;
  // Profile panel (#57).
  /**
   * The file of the profile named `name`. When no valid profile has that name, a file that is not
   * valid now and had that name when last valid, or that is named after it, so the panel can repair it.
   */
  profileText(name: string): Promise<ProfileText>;
  /**
   * Replaces the file of the profile named `name` with `text`, comments included. Rejects with a
   * `TextProblem`, saving nothing, when `text` is not a valid profile or holds another name.
   */
  setProfileText(name: string, text: string): Promise<void>;
  /** The repo's overrides as TOML: a profile without `name` and `suggest_for`. Empty when it has none. */
  repoOverridesText(repo: string): Promise<string>;
  /**
   * Replaces the repo's overrides, keeping its profile. Comments are not kept. Rejects with a
   * `TextProblem`, saving nothing, when `text` is not valid.
   */
  setRepoOverridesText(repo: string, text: string): Promise<void>;
  /** The repo's run sessions: its profile's, or its own when its overrides set them. */
  runSessions(repo: string): Promise<RunSession[]>;
  /** Calls `listener` when a profile changed, through grove or in its file. Returns the unsubscribe. */
  onProfilesChanged(listener: () => void): () => void;
  /**
   * Calls `listener` with the repo's main worktree when the repo's settings changed: its profile
   * pick or its overrides. Returns the unsubscribe.
   */
  onRepoSettingsChanged(listener: (repo: string) => void): () => void;
  /** Calls `listener` when the user asks to quit grove with Cmd-Q. Returns the unsubscribe. */
  onQuitRequested(listener: () => void): () => void;
  /** Quits grove, which ends every session. */
  quit(): Promise<void>;
  /**
   * Calls `listener` with whether the window is full screen, at once and on each change. macOS
   * hides the window controls in full screen. Returns the unsubscribe.
   */
  onFullscreenChanged(listener: (fullscreen: boolean) => void): () => void;
  /**
   * As `spawnAgent`, once the worktree's setup script succeeds, or at once when none runs. Fails,
   * starting nothing, when setup failed.
   */
  spawnAgentAfterSetup(worktree: string, agent: Agent, panel: number | null): Promise<SessionId>;
  /** Calls `listener` when a worktree's setup script starts or ends. Returns the unsubscribe. */
  onSetup(listener: (change: SetupChanged) => void): () => void;
  // Terminal mode (#127).
  /**
   * Switches the agent session to `mode`. Its process ends, and a new one resumes the same
   * conversation under the same session id. Settles once the new process runs, and in chat mode
   * once its history is read back. Starts a session that has ended again. Fails while the session is
   * `Working` or `NeedsInput`, or already switching. A session already in `mode` stays as it is.
   */
  setMode(session: SessionId, mode: Mode): Promise<void>;
  // Review comments (#54).
  /** Adds a comment on lines of the worktree's current version of a file. Fails when the file lacks those lines. */
  addComment(worktree: string, comment: NewReviewComment): Promise<CommentId>;
  /** The worktree's open and sent comments, oldest first. */
  comments(worktree: string): Promise<ReviewComment[]>;
  /** Resolves a comment, open or sent. It leaves the list but is kept. */
  resolveComment(id: CommentId): Promise<void>;
  /**
   * Sends the worktree's open comments as one review batch to its agent session `to`, or with null
   * to its most recently focused one. The session must be `Idle` or `Done`, and in terminal mode
   * have bracketed paste on. The comments become `Sent`.
   */
  sendReviewBatch(worktree: string, to: SessionId | null): Promise<void>;
  /** Calls `listener` with a worktree's path when its comments change. Returns the unsubscribe. */
  onCommentsChanged(listener: (worktree: string) => void): () => void;
  /** The worktree's agent session that last got focus. null when none has. */
  lastFocusedAgent(worktree: string): Promise<SessionId | null>;
  /** Whether the session's program has bracketed paste on. Fails for an agent session in chat mode. */
  bracketedPaste(session: SessionId): Promise<boolean>;
  /** Shows `icon` in the Dock while grove runs. When grove quits, the bundle icon shows again. */
  setDockIcon(icon: DockIcon): Promise<void>;
}

// A failed command rejects with the core's error message as a string.
export const tauriBridge: Bridge = {
  addRepo: (path) => invoke("add_repo", { path }),
  pickFolder: () => open({ directory: true, multiple: false, title: "Add repo" }),
  listWorktrees: () => invoke("list_worktrees"),
  diff: (worktree, base) => invoke("diff", { worktree, base }),
  fileVersions: (worktree, base, file) => invoke("file_versions", { worktree, base, file }),
  onDiffChanged(listener) {
    const unlisten = listen<string>("diff-changed", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  openInEditor: (editor, worktree, file, line) =>
    invoke("open_in_editor", { editor, worktree, file, line }),
  revealInFinder: (worktree) => invoke("reveal_in_finder", { worktree }),
  openLinkInDefaultBrowser: (link) => invoke("open_link_in_default_browser", { link }),
  spawnShell: (worktree, cols, rows) => invoke("spawn_shell", { worktree, cols, rows }),
  spawnRun: (worktree, name, cols, rows) => invoke("spawn_run", { worktree, name, cols, rows }),
  onSessionExited(listener) {
    const unlisten = listen<SessionId>("session-exited", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  spawnAgent: (worktree, agent, panel) => invoke("spawn_agent", { worktree, agent, panel }),
  sessions: (worktree) => invoke("sessions", { worktree }),
  async subscribe(session, onMessages) {
    // Each subscribe has its own channel, so frames still on their way from an earlier one stay apart.
    const messages = new Channel<AgentMessage[]>(onMessages);
    return invoke("subscribe", { session, messages });
  },
  sendMessage: (session, text) => invoke("send_message", { session, text }),
  interrupt: (session) => invoke("interrupt", { session }),
  async askSideQuestion(session, question, onMessages) {
    const answer = new Channel<AgentMessage[]>(onMessages);
    return invoke("ask_side_question", { session, question, answer });
  },
  endSideQuestion: (session, sideQuestion) => invoke("end_side_question", { session, sideQuestion }),
  answer: (session, request, reply) => invoke("answer", { session, request, reply }),
  models: (session) => invoke("models", { session }),
  setModel: (session, model, effort) => invoke("set_model", { session, model, effort }),
  modelChoice: (session) => invoke("model_choice", { session }),
  // An agent reports its login once, when a session starts, which can be before a reload.
  onAccount: (listener) => listenAndList("account", "accounts", listener),
  onAgentState(listener) {
    const unlisten = listen<AgentStateChanged>("agent-state", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  agentSessions: () => invoke("agent_sessions"),
  onAgentSessionsChanged(listener) {
    const unlisten = listen("agent-sessions-changed", () => listener());
    return () => void unlisten.then((stop) => stop());
  },
  deleteAgentSession: (session) => invoke("delete_agent_session", { session }),
  resumeAgentSession: (session) => invoke("resume_agent_session", { session }),
  onNotificationClicked(listener) {
    const unlisten = listen<NotificationClicked>("notification-clicked", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  // The core sends some before the UI listens, such as the login shell's at start.
  onWarnings(listener) {
    let subscribed = true;
    let latest = 0;
    async function refresh() {
      if (!subscribed) return;
      const request = ++latest;
      try {
        const active = await invoke<WarningSent[]>("warnings");
        if (subscribed && request === latest) listener(active);
      } catch (error) {
        console.warn("could not list warnings:", error);
      }
    }
    const unlisten = listen("warnings-changed", refresh);
    void unlisten.catch((error: unknown) => console.warn("could not listen for warnings:", error))
      .then(refresh);
    return () => {
      subscribed = false;
      void unlisten.then((stop) => stop(), () => {});
    };
  },
  retryWatcher: (worktree) => invoke("retry_watcher", { worktree }),
  focus: (session) => invoke("focus", { session }),
  write: (session, bytes) => invoke("write", { session, bytes: Array.from(bytes) }),
  resize: (session, cols, rows) => invoke("resize", { session, cols, rows }),
  async attach(session, onFrame) {
    // Each attach has its own channel, so frames still on their way from an earlier one stay apart.
    const output = new Channel<ArrayBuffer>((body) => onFrame(parseFrame(body)));
    return parseSnapshot(await invoke<ArrayBuffer>("attach", { session, output }));
  },
  ack: (session, bytes) => invoke("ack", { session, bytes }),
  detach: (session) => invoke("detach", { session }),
  kill: (session) => invoke("kill", { session }),

  // Files panel (#124).
  listFiles: (worktree) => invoke("list_files", { worktree }),
  readFile: async (worktree, file) =>
    new Uint8Array(await invoke<ArrayBuffer>("read_file", { worktree, file })),
  writeFile: (worktree, file, text) => invoke("write_file", { worktree, file, text }),
  lineChanges: (worktree, file) => invoke("line_changes", { worktree, file }),

  // Attention queue (#53).
  attentionQueue: () => invoke("attention_queue"),

  // Workspaces (#122).
  workspace: (worktree) => invoke("workspace", { worktree }),
  setWorkspace: (worktree, workspace) => invoke("set_workspace", { worktree, workspace }),
  onWorkspaceChanged(listener) {
    const unlisten = listen<string>("workspace-changed", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  // The agent change notice (#4).
  onAgentChange(listener) {
    const unlisten = listen<AgentChange>("agent-change", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  undoWorkspace: (worktree) => invoke("undo_workspace", { worktree }),
  saveWorkspaceToProfile: (worktree) => invoke("save_workspace_to_profile", { worktree }),

  // Worktree management (#52).
  createWorktree: (repo, worktree) => invoke("create_worktree", { repo, worktree }),
  setBaseBranch: (worktree, base) => invoke("set_base_branch", { worktree, base }),
  forgetWorktree: (worktree) => invoke("forget_worktree", { worktree }),
  deleteCheck: (worktree) => invoke("delete_check", { worktree }),
  deleteWorktree: (worktree, confirmed, deleteBranch) =>
    invoke("delete_worktree", { worktree, confirmed, deleteBranch }),
  profileNames: () => invoke("profile_names"),
  suggestProfile: (repo) => invoke("suggest_profile", { repo }),
  repoProfile: (repo) => invoke("repo_profile", { repo }),
  setRepoProfile: (repo, profile) => invoke("set_repo_profile", { repo, profile }),
  // Profile panel (#57).
  profileText: (name) => invoke("profile_text", { name }),
  setProfileText: (name, text) => invoke("set_profile_text", { name, text }),
  repoOverridesText: (repo) => invoke("repo_overrides_text", { repo }),
  setRepoOverridesText: (repo, text) => invoke("set_repo_overrides_text", { repo, text }),
  runSessions: (repo) => invoke("run_sessions", { repo }),
  onProfilesChanged(listener) {
    const unlisten = listen("profiles-changed", () => listener());
    return () => void unlisten.then((stop) => stop());
  },
  onRepoSettingsChanged(listener) {
    const unlisten = listen<string>("repo-settings-changed", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  onQuitRequested(listener) {
    const unlisten = listen("quit-requested", () => listener());
    return () => void unlisten.then((stop) => stop());
  },
  quit: () => invoke("quit"),
  // Entering and leaving full screen resize the window.
  onFullscreenChanged(listener) {
    const window = getCurrentWindow();
    let listening = true;
    const check = () => void window.isFullscreen().then((fullscreen) => listening && listener(fullscreen));
    check();
    const unlisten = window.onResized(check);
    return () => {
      listening = false;
      void unlisten.then((stop) => stop());
    };
  },
  spawnAgentAfterSetup: (worktree, agent, panel) =>
    invoke("spawn_agent_after_setup", { worktree, agent, panel }),
  onSetup(listener) {
    const unlisten = listen<SetupChanged>("setup", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  // Terminal mode (#127).
  setMode: (session, mode) => invoke("set_mode", { session, mode }),
  // Review comments (#54).
  addComment: (worktree, comment) => invoke("add_comment", { worktree, comment }),
  comments: (worktree) => invoke("comments", { worktree }),
  resolveComment: (id) => invoke("resolve_comment", { id }),
  sendReviewBatch: (worktree, to) => invoke("send_review_batch", { worktree, to }),
  onCommentsChanged(listener) {
    const unlisten = listen<string>("comments-changed", (event) => listener(event.payload));
    return () => void unlisten.then((stop) => stop());
  },
  lastFocusedAgent: (worktree) => invoke("last_focused_agent", { worktree }),
  bracketedPaste: (session) => invoke("bracketed_paste", { session }),
  setDockIcon: (icon) => invoke("set_dock_icon", { icon }),
};

/**
 * Calls `listener` with each `event`, and with what `command` lists: those sent before the UI
 * listened. One sent between the listen and the list comes twice. When listening fails, the
 * listed ones still come. Returns the unsubscribe.
 */
function listenAndList<T>(event: string, command: string, listener: (payload: T) => void) {
  let listening = true;
  const unlisten = listen<T>(event, (e) => listener(e.payload));
  const listened = unlisten.catch((e: unknown) => console.warn(`could not listen for ${event}:`, e));
  void listened
    .then(() => invoke<T[]>(command))
    .then((listed) => listed.forEach((payload) => listening && listener(payload)));
  return () => {
    listening = false;
    void unlisten.then((stop) => stop(), () => {});
  };
}

/** A frame as `attach` in `app/src-tauri/src/main.rs` sends it: its seq in 8 big-endian bytes, then its output. */
export function parseFrame(body: ArrayBuffer): OutputFrame {
  return { seq: Number(new DataView(body).getBigUint64(0)), bytes: new Uint8Array(body, 8) };
}

/**
 * A snapshot as `attach` in `app/src-tauri/src/main.rs` returns it: its seq in 8 big-endian bytes,
 * its cols and rows in 2 each, then its bytes.
 */
export function parseSnapshot(body: ArrayBuffer): Snapshot {
  const view = new DataView(body);
  return {
    seq: Number(view.getBigUint64(0)),
    cols: view.getUint16(8),
    rows: view.getUint16(10),
    bytes: new Uint8Array(body, 12),
  };
}
