import type {
  AccountChanged,
  AgentChange,
  Agent,
  AgentMessage,
  AgentStateChanged,
  BatchComment,
  Bridge,
  DeleteCheck,
  DiffBase,
  Editor,
  FileDiff,
  FileVersions,
  LineChange,
  Mode,
  Model,
  NotificationClicked,
  OutputFrame,
  Reply,
  ReviewComment,
  RunSession,
  SessionId,
  SessionInfo,
  SetupChanged,
  SetupState,
  TextProblem,
  Warning,
  WarningSent,
  Workspace,
  Worktree,
} from "./bridge";
import { fileLines, findLines } from "./anchor";

/** One file in a fake diff, with both its versions. */
export interface FakeChange {
  file: FileDiff;
  versions: FileVersions;
  /** The file's gutter marks, for a Head change. None when left out. */
  lines?: LineChange[];
}

/**
 * Each worktree's diff per base, by worktree path. A base left out fails as the core does when
 * the worktree has no base branch. Tests may change it, then call `diffChanged`.
 */
export type FakeDiffs = Record<string, Partial<Record<DiffBase, FakeChange[]>>>;

/** Each worktree's files on disk, by worktree path, then by path. Bytes for a file that is not text. */
export type FakeFiles = Record<string, Record<string, string | Uint8Array>>;

/** A worktree as git would list it. What a test leaves out takes the core's default. */
export type FakeWorktree = Pick<Worktree, "path" | "branch" | "isMain"> & Partial<Worktree>;

/** What the fake core finds when it checks a worktree before deleting it. */
export interface FakeWorktreeStatus extends Pick<DeleteCheck, "uncommitted" | "unpushed" | "merged"> {
  /** Why `git branch -D` fails for its branch. null when it works. */
  branchNotDeleted: string | null;
}

/** A profile as the fake core knows it. */
export interface FakeProfile {
  name: string;
  /** Marker files, relative to a repo's main worktree. */
  suggestFor: string[];
  /** Runs in each worktree grove adds. */
  setup?: string;
  /** Its file. Left out, a file with the fields above. */
  text?: string;
}

/** The starter profiles the core writes on first start. */
export const STARTER_PROFILES: FakeProfile[] = [
  { name: "Next.js app", suggestFor: ["next.config.js", "next.config.mjs", "next.config.ts"], setup: "npm ci" },
  { name: "Rust crate", suggestFor: ["Cargo.toml"], setup: "cargo fetch" },
];

export interface FakeBridge extends Bridge {
  /** Tells every listener the worktree's diff may have changed, as the core's watcher does. */
  diffChanged(worktree: string): void;
  /** Tells every listener an agent session's state changed, as the core's hooks and heuristics do. */
  agentStateChanged(change: AgentStateChanged): void;
  /** Sends a warning, as the core does. A listener that comes later still gets it. */
  warn(warning: Warning): void;
  /** Makes `retryWatcher` for the worktree fail with `reason`, or succeed again with null. */
  failWatcherRetry(worktree: string, reason: string | null): void;
  /** Every `retryWatcher` call, in order. */
  watcherRetries: string[];
  /** Reports an agent's login, as its chat-mode transport does. A listener that comes later still gets the latest. */
  accountChanged(change: AccountChanged): void;
  /**
   * Adds the messages to a chat-mode session's conversation, as its agent sends them. If a panel
   * subscribed, they go to it as one frame, as the core's batcher sends them.
   */
  messages(session: SessionId, ...messages: FakeMessage[]): void;
  /** Every `openInEditor` call, in order. */
  opened: { editor: Editor; worktree: string; file: string; line: number }[];
  /** Every worktree `revealInFinder` showed, in order. */
  revealed: string[];
  /** Every link `openLinkInDefaultBrowser` opened, in order. */
  openedLinks: string[];
  /** Makes the next `pickFolder` give `path`, or null as a cancel does. Until called, it cancels. */
  willPickFolder(path: string | null): void;
  /** How many times `pickFolder` was opened. */
  readonly foldersPicked: number;
  /**
   * Nests the worktree under `from` as the core does when an agent session in `from` starts a
   * subagent in it: only when it nests under nothing yet.
   */
  nestWorktree(worktree: string, from: string): void;
  /** Every shell session started, in order. */
  spawned: { session: SessionId; worktree: string; cols: number; rows: number }[];
  /** Every agent session started, in order. */
  spawnedAgents: { session: SessionId; worktree: string; agent: Agent }[];
  /** Every `sendMessage` call, in order. */
  sent: { session: SessionId; text: string }[];
  /** Every `interrupt` call, in order. */
  interrupted: SessionId[];
  /** Every `answer` call the fake core took, in order. */
  answered: { session: SessionId; request: string; reply: Reply }[];
  /** What `models` lists for each agent. Tests may change it before a session starts. */
  agentModels: Record<Agent, Model[]>;
  /** Every `setModel` call, in order. */
  modelsSet: { session: SessionId; model: string; effort: string | null }[];
  /** Every `subscribe` call, in order. */
  subscribed: SessionId[];
  /** Every `write` call, in order. */
  writes: { session: SessionId; bytes: Uint8Array }[];
  /** Every `resize` call, in order. */
  resizes: { session: SessionId; cols: number; rows: number }[];
  /** Every `ack` call, in order. */
  acks: { session: SessionId; bytes: number }[];
  /** Every `focus` call, in order. */
  focused: SessionId[];
  killed: SessionId[];
  attached(session: SessionId): boolean;
  /**
   * Prints `bytes` in the session. If a panel is attached, they go to it as the next frame, as the
   * core's batcher sends them. Attached or not, they go into the snapshot of every later attach.
   */
  output(session: SessionId, bytes: Uint8Array): void;
  /**
   * Replaces everything the session printed with `bytes`, for the snapshot of every later attach.
   * The core's snapshot holds the terminal's state, not its history, so a mode the session turned
   * back off has no sequence in it.
   */
  setSnapshot(session: SessionId, bytes: Uint8Array): void;
  /** Sends `frame` to the session's attached panel as is, with a seq the core may never send. */
  sendFrame(session: SessionId, frame: OutputFrame): void;

  // Files panel (#124).
  /** Changes a file on disk, as an agent would. Call `diffChanged` after, as the core's watcher would. */
  setFile(worktree: string, file: string, contents: string | Uint8Array): void;
  /** Every `readFile` call, in order. */
  readFiles: { worktree: string; file: string }[];
  /** Every `writeFile` call, in order. */
  writtenFiles: { worktree: string; file: string; text: string }[];

  // Workspaces (#122).
  /** Every `setWorkspace` call the fake core took, in order. */
  workspacesSet: { worktree: string; workspace: Workspace }[];
  /**
   * Starts an agent session that no workspace names, as a subagent starts, or one from before
   * the core placed sessions in panels.
   */
  spawnAgentWithoutPanel(worktree: string, agent: Agent): SessionId;
  /**
   * Holds back each `workspace-changed` from now on, as a busy window gets it late. Returns what
   * sends the held ones, in order, and stops holding.
   */
  holdWorkspaceChanges(): () => void;
  // The agent change notice (#4).
  /**
   * Saves the worktree's workspace as the agent session `by` does through `grove-mcp`: the one from
   * before stays for Undo, and `agent-change` comes before `workspace-changed`, as in the core.
   */
  agentChangesWorkspace(worktree: string, by: SessionId, workspace: Workspace): void;
  /** Changes a profile as the agent session `by` does through `grove-mcp`. */
  agentChangesProfile(by: SessionId): void;
  /** Every `undoWorkspace` call the fake core took, in order. */
  undone: string[];
  /** Every `saveWorkspaceToProfile` call the fake core took, in order. */
  savedToProfile: string[];

  // Worktree management (#52).
  /** Changes what the fake core finds in the worktree. A worktree starts clean and unmerged. */
  setStatus(worktree: string, status: Partial<FakeWorktreeStatus>): void;
  /** Every `deleteWorktree` call the fake core took, in order. */
  deleted: { worktree: string; confirmed: boolean; deleteBranch: boolean }[];
  /** Asks to quit, as Cmd-Q does. */
  requestQuit(): void;
  /** Clicks the session's notification, as the shell reports it. */
  clickNotification(clicked: NotificationClicked): void;
  /** How many times `quit` was called. */
  quitCount(): number;
  /** Puts the window in or out of full screen. It starts out of it. */
  setFullscreen(fullscreen: boolean): void;
  /** Ends the worktree's setup script with exit `code`. null means a signal ended it. */
  finishSetup(worktree: string, code: number | null): void;
  // Terminal mode (#127).
  /** Every `setMode` call the fake core took, in order. */
  modesSet: { session: SessionId; mode: Mode }[];
  /**
   * Holds each `setMode` from now on between its two processes: the old one has ended and the new
   * one has not started. Returns what lets the held ones go on, and stops holding.
   */
  holdModeSwitches(): () => void;
  // Profile panel (#57).
  /**
   * Replaces a profile's file, as the user's own editor would. Sends ProfilesChanged, as the core's
   * watcher does. Text that is not a valid profile makes `profileNames` fail, as in the core.
   */
  changeProfileOnDisk(name: string, text: string): void;
  /** Every `setProfileText` call the fake core took, in order. */
  profileTextsSet: { name: string; text: string }[];
  /** Every `setRepoOverridesText` call the fake core took, in order. */
  overridesSet: { repo: string; text: string }[];
  // Review comments (#54).
  /** Turns bracketed paste on or off in a terminal-mode session, as its TUI does. Off until set, and after each mode switch. */
  setBracketedPaste(session: SessionId, on: boolean): void;
  /**
   * Every review batch the fake core sent, in order. A chat-mode session also gets it in its
   * conversation. A terminal-mode one gets nothing else.
   */
  reviewBatches: { worktree: string; to: SessionId }[];
  // Run sessions (#168).
  /** Every run session started, in order. */
  spawnedRuns: { session: SessionId; worktree: string; name: string; cols: number; rows: number }[];
  /** Brings back the run session `name` in the worktree stopped, with no output, as a restart does with one that was running at quit. Returns its id. */
  restoreStoppedRun(worktree: string, name: string): SessionId;
  /**
   * Ends the session's process, as when its command finishes. Sends SessionExited, as the core does.
   * As in the core, a run session is then listed as stopped and its final screen stays attachable,
   * until another of its name starts in its worktree. A shell's output goes.
   */
  exit(session: SessionId): void;
}

/** The layout a worktree starts with when its profile sets none, as in the core. */
export const DEFAULT_WORKSPACE: Workspace = {
  chat: [{ agent: "Claude", session: null, shown: true }],
  panels: ["Files", "Diff"],
  selected: "Files",
  terminal: true,
};

/** The models the fake agents list, each with its own default, as the real ones do. */
export const FAKE_MODELS: Record<Agent, Model[]> = {
  Claude: [
    {
      id: "default",
      name: "Default (recommended)",
      description: "Opus 5.5",
      efforts: ["low", "medium", "high", "xhigh", "max"],
      defaultEffort: null,
      isDefault: true,
    },
    { id: "haiku", name: "Haiku 5.5", description: "Fastest", efforts: [], defaultEffort: null, isDefault: false },
  ],
  Codex: [
    {
      id: "gpt-5.5",
      name: "GPT-5.5",
      description: "Frontier model",
      efforts: ["low", "medium", "high"],
      defaultEffort: "medium",
      isDefault: true,
    },
    {
      id: "gpt-5.5-mini",
      name: "GPT-5.5 mini",
      description: "Smaller and faster",
      efforts: ["low", "medium"],
      defaultEffort: "low",
      isDefault: false,
    },
  ],
};

/** An agent message as a test writes it: a time it leaves out is null, as a record with none. */
export type FakeMessage = AgentMessage extends infer Message
  ? Message extends { timeUnixMs: number | null }
    ? Omit<Message, "timeUnixMs"> & { timeUnixMs?: number | null }
    : Message
  : never;

function withTime(message: FakeMessage): AgentMessage {
  if (!TIMED.has(message.type)) return message as AgentMessage;
  return { ...message, timeUnixMs: (message as { timeUnixMs?: number | null }).timeUnixMs ?? null } as AgentMessage;
}

const TIMED = new Set<AgentMessage["type"]>(["TurnStarted", "ToolCall", "TurnEnded"]);

/**
 * An in-memory bridge for tests, with no Tauri and no core. `repos` maps a path the user can
 * add to the worktrees git would list for it, main worktree first.
 */
export function fakeBridge(
  repos: Record<string, FakeWorktree[]>,
  diffs: FakeDiffs = {},
  files: FakeFiles = {},
  profiles: FakeProfile[] = STARTER_PROFILES,
): FakeBridge {
  const tracked: Worktree[] = [];
  const listeners = new Set<(worktree: string) => void>();
  const stateListeners = new Set<(change: AgentStateChanged) => void>();
  const warnings: WarningSent[] = [];
  const warningListeners = new Set<(warning: WarningSent) => void>();
  const watcherFailures = new Map<string, string>();
  const watcherRetries: string[] = [];
  const accounts = new Map<Agent, AccountChanged>();
  const accountListeners = new Set<(change: AccountChanged) => void>();
  const opened: FakeBridge["opened"] = [];
  const revealed: string[] = [];
  const openedLinks: string[] = [];
  let nextFolder: string | null = null;
  let foldersPicked = 0;
  const spawned: FakeBridge["spawned"] = [];
  const spawnedAgents: FakeBridge["spawnedAgents"] = [];
  const sent: FakeBridge["sent"] = [];
  const interrupted: SessionId[] = [];
  const answered: FakeBridge["answered"] = [];
  const agentModels: FakeBridge["agentModels"] = { ...FAKE_MODELS };
  const modelsSet: FakeBridge["modelsSet"] = [];
  const subscribed: SessionId[] = [];
  // Each chat-mode session's conversation, and the panel that subscribed to it.
  const chats = new Map<SessionId, AgentMessage[]>();
  const messageSinks = new Map<SessionId, (messages: AgentMessage[]) => void>();
  const writes: FakeBridge["writes"] = [];
  const resizes: FakeBridge["resizes"] = [];
  const acks: FakeBridge["acks"] = [];
  const focused: SessionId[] = [];
  const agentStates = new Map<SessionId, AgentStateChanged>();
  // Attention queue (#53): each agent session's worktree, and when it entered its state, as a count.
  const agentWorktrees = new Map<SessionId, string>();
  const entered = new Map<SessionId, number>();
  let changesSoFar = 0;
  const killed: SessionId[] = [];
  const running = new Set<SessionId>();
  const sinks = new Map<SessionId, (frame: OutputFrame) => void>();
  const lastSeq = new Map<SessionId, number>();
  const sizes = new Map<SessionId, { cols: number; rows: number }>();
  // What each session printed, or what a test set in its place. A reset xterm.js that writes it
  // draws what the core's snapshot would.
  const printed = new Map<SessionId, Uint8Array>();
  // Shell and agent sessions share the core's ids.
  let sessions = 0;
  // Files panel (#124).
  const readFiles: FakeBridge["readFiles"] = [];
  const writtenFiles: FakeBridge["writtenFiles"] = [];
  // Workspaces (#122): the saved ones, by worktree path.
  const workspaces = new Map<string, Workspace>();
  const workspacesSet: FakeBridge["workspacesSet"] = [];
  // The agent change notice (#4): each worktree's workspace from before an agent changed it.
  const beforeAgentChange = new Map<string, Workspace>();
  const agentChangeListeners = new Set<(change: AgentChange) => void>();
  const undone: string[] = [];
  const savedToProfile: string[] = [];
  // Worktree management (#52).
  const statuses = new Map<string, FakeWorktreeStatus>();
  const deleted: FakeBridge["deleted"] = [];
  const repoProfiles = new Map<string, string | null>();
  const quitListeners = new Set<() => void>();
  const fullscreenListeners = new Set<(fullscreen: boolean) => void>();
  let fullscreen = false;
  const notificationListeners = new Set<(clicked: NotificationClicked) => void>();
  let quits = 0;
  const setupListeners = new Set<(change: SetupChanged) => void>();
  // What each `spawnAgentAfterSetup` call waits on, by worktree.
  const setupWaiters = new Map<string, (() => void)[]>();
  // Terminal mode (#127): the mode each agent session runs in.
  const modes = new Map<SessionId, Mode>();
  const modesSet: FakeBridge["modesSet"] = [];
  // Profile panel (#57): each profile's file by name, and each repo's overrides by repo.
  const profileTexts = new Map(profiles.map((p) => [p.name, p.text ?? profileFile(p)]));
  const overrides = new Map<string, string>();
  const profilesListeners = new Set<() => void>();
  const repoSettingsListeners = new Set<(repo: string) => void>();
  const profileTextsSet: FakeBridge["profileTextsSet"] = [];
  const overridesSet: FakeBridge["overridesSet"] = [];

  function profilesChanged() {
    profilesListeners.forEach((listener) => listener());
  }

  function repoSettingsChanged(repo: string) {
    repoSettingsListeners.forEach((listener) => listener(repo));
  }

  // Review comments (#54): each worktree's comments, oldest first, and what they need.
  const comments = new Map<string, ReviewComment[]>();
  let nextComment = 0;
  const commentListeners = new Set<(worktree: string) => void>();
  const workspaceListeners = new Set<(worktree: string) => void>();
  // The worktrees of the held `workspace-changed` events, in order. null while none are held.
  let heldWorkspaceChanges: string[] | null = null;
  // What lets each held `setMode` go on, in order. null while none are held.
  let heldModeSwitches: (() => void)[] | null = null;
  // The agent sessions between their two processes in a mode switch.
  const switching = new Set<SessionId>();
  const lastFocused = new Map<string, SessionId>();
  const pastes = new Map<SessionId, boolean>();
  const reviewBatches: FakeBridge["reviewBatches"] = [];
  // Run sessions (#168): those started, and the stopped ones: those that exited, whose output
  // stays, and those a restart brought back, which have none.
  const spawnedRuns: FakeBridge["spawnedRuns"] = [];
  const stoppedRuns: { session: SessionId; worktree: string; name: string; kept: boolean }[] = [];
  const exitListeners = new Set<(session: SessionId) => void>();

  function sessionExited(session: SessionId) {
    running.delete(session);
    const run = spawnedRuns.find((s) => s.session === session);
    if (run) {
      dropStoppedRun(run.worktree, run.name);
      stoppedRuns.push({ session, worktree: run.worktree, name: run.name, kept: true });
    }
    exitListeners.forEach((listener) => listener(session));
  }

  function dropStoppedRun(worktree: string, name: string) {
    const stopped = stoppedRuns.findIndex((s) => s.worktree === worktree && s.name === name);
    if (stopped >= 0) stoppedRuns.splice(stopped, 1);
  }

  // As the core: the overrides' run sessions when they set any, else the profile's.
  function runSessionsOf(repo: string): RunSession[] {
    const profile = repoProfiles.get(repo);
    const profileText = profile ? (profileTexts.get(profile) ?? "") : "";
    return runSessionsIn(overrides.get(repo) ?? "") ?? runSessionsIn(profileText) ?? [];
  }

  function checkTracked(worktree: string) {
    if (!tracked.some((w) => w.path === worktree)) {
      throw `grove does not track a worktree at ${worktree}`;
    }
  }

  function filesOf(worktree: string) {
    const found = files[worktree];
    if (!found) throw `grove does not track a worktree at ${worktree}`;
    return found;
  }

  function check(session: SessionId) {
    if (!running.has(session)) throw `session ${session} is not running`;
  }

  function start(worktree: string, cols: number, rows: number): SessionId {
    checkTracked(worktree);
    const session = String(sessions++);
    running.add(session);
    sizes.set(session, { cols, rows });
    printed.set(session, new Uint8Array());
    return session;
  }

  // The conversation outlives each process, so it is kept across mode switches.
  function chat(session: SessionId): AgentMessage[] {
    check(session);
    const history = chats.get(session);
    if (!history || modes.get(session) !== "Chat") throw `session ${session} is not in chat mode`;
    return history;
  }

  function addMessages(session: SessionId, messages: AgentMessage[]) {
    chat(session).push(...messages);
    messageSinks.get(session)?.(messages);
  }

  function changeState(change: AgentStateChanged) {
    // As in the core, the same state from another source keeps the session's place in the queue.
    const last = agentStates.get(change.session);
    if (last?.state !== change.state || attentionRank(last) !== attentionRank(change)) {
      entered.set(change.session, changesSoFar++);
    }
    agentStates.set(change.session, change);
    stateListeners.forEach((listener) => listener(change));
  }

  // As `create_worktree` and the other repo calls find a repo: by its main worktree.
  function repoAt(repo: string): Worktree[] {
    const at = tracked.findIndex((w) => w.isMain && w.path === repo);
    if (at < 0) throw `grove has no repo with its main worktree at ${repo}`;
    const next = tracked.findIndex((w, i) => i > at && w.isMain);
    return tracked.slice(at, next < 0 ? undefined : next);
  }

  // The worktree, refused as `remove_worktree` refuses it: unknown, or a main worktree.
  function linked(worktree: string): Worktree {
    const found = tracked.find((w) => w.path === worktree);
    if (!found) throw `grove does not track a worktree at ${worktree}`;
    if (found.isMain) throw `${worktree} is a main worktree, which grove forgets only with its repo`;
    return found;
  }

  // The fake repo has the branches its worktrees are on and their base branches.
  function checkBranch(worktree: Worktree, branch: string) {
    const main = mainOf(worktree);
    const known = repoAt(main.path).flatMap((w) => [w.branch, w.baseBranch]);
    if (!known.includes(branch)) throw `the repo at ${main.path} has no branch ${JSON.stringify(branch)}`;
  }

  function mainOf(worktree: Worktree): Worktree {
    return tracked
      .slice(0, tracked.indexOf(worktree) + 1)
      .filter((w) => w.isMain)
      .pop()!;
  }

  function statusOf(worktree: string): FakeWorktreeStatus {
    return statuses.get(worktree) ?? { uncommitted: false, unpushed: 0, merged: false, branchNotDeleted: null };
  }

  // A switch to terminal mode starts the agent's TUI at 80x24, as the app does.
  function startAgentSession(worktree: string, agent: Agent): SessionId {
    const session = start(worktree, 80, 24);
    chats.set(session, []);
    modes.set(session, "Chat");
    agentWorktrees.set(session, worktree);
    spawnedAgents.push({ session, worktree, agent });
    return session;
  }

  // As in the core: a panel `workspace` lists has no session yet, or one running in the worktree,
  // a session between its two processes in a mode switch too.
  function listed(worktree: string, session: SessionId | null) {
    if (session === null) return true;
    return (running.has(session) || switching.has(session)) && agentWorktrees.get(session) === worktree;
  }

  // As in the core: a panel whose session no longer runs in the worktree is left out.
  function workspaceOf(worktree: string): Workspace {
    const saved = structuredClone(workspaces.get(worktree) ?? DEFAULT_WORKSPACE);
    saved.chat = saved.chat.filter((panel) => listed(worktree, panel.session));
    return saved;
  }

  function workspaceChanged(worktree: string) {
    if (heldWorkspaceChanges) heldWorkspaceChanges.push(worktree);
    else workspaceListeners.forEach((listener) => listener(worktree));
  }

  // As the core's `spawn_agent`: the session goes in panel `panel`, counted as `workspace` lists
  // them, when it has none, or else in a new shown panel after the last one. The other panels stay,
  // those of ended sessions too, and the workspace is saved.
  function spawnAgentInPanel(worktree: string, agent: Agent, panel: number | null): SessionId {
    checkTracked(worktree);
    if (panel !== null && panel >= workspaceOf(worktree).chat.length) {
      throw "the workspace is not valid: its agent panel is past the last one";
    }
    const saved = structuredClone(workspaces.get(worktree) ?? DEFAULT_WORKSPACE);
    const target = panel === null ? undefined : saved.chat.filter((p) => listed(worktree, p.session))[panel];
    const session = startAgentSession(worktree, agent);
    const placed = { agent, session, shown: true };
    if (target?.session === null) saved.chat[saved.chat.indexOf(target)] = placed;
    else saved.chat.push(placed);
    workspaces.set(worktree, saved);
    workspaceChanged(worktree);
    return session;
  }

  function setupOf(worktree: string): SetupState | null {
    return tracked.find((w) => w.path === worktree)?.setup ?? null;
  }

  // As the core: the worktree keeps its setup's state, and every listener hears of the change.
  function setSetup(worktree: string, setup: SetupState) {
    tracked.find((w) => w.path === worktree)!.setup = setup;
    setupListeners.forEach((listener) => listener({ worktree, setup }));
  }

  // As the core: removing a worktree ends its sessions first.
  function untrack(worktree: string) {
    for (const session of [...running]) {
      const own = [...spawned, ...spawnedRuns].some((s) => s.session === session && s.worktree === worktree);
      if (own || agentWorktrees.get(session) === worktree) running.delete(session);
    }
    tracked.splice(tracked.findIndex((w) => w.path === worktree), 1);
    workspaces.delete(worktree);
    stoppedRuns.splice(0, stoppedRuns.length, ...stoppedRuns.filter((s) => s.worktree !== worktree));
  }

  function commentsChanged(worktree: string) {
    commentListeners.forEach((listener) => listener(worktree));
  }

  // As the core after each DiffChanged: each comment not resolved moves to where its text is now,
  // and is flagged where it was when its text is gone. Renames are not followed.
  function placeComments(worktree: string) {
    let moved = false;
    for (const comment of comments.get(worktree) ?? []) {
      if (comment.state === "Resolved") continue;
      const contents = files[worktree]?.[comment.path];
      const found = typeof contents === "string" ? findLines(fileLines(contents), comment.text, comment.startLine) : null;
      const startLine = found ?? comment.startLine;
      const possiblyAddressed = found === null;
      if (startLine === comment.startLine && possiblyAddressed === comment.possiblyAddressed) continue;
      comment.endLine += startLine - comment.startLine;
      Object.assign(comment, { startLine, possiblyAddressed });
      moved = true;
    }
    if (moved) commentsChanged(worktree);
  }

  function changes(worktree: string, base: DiffBase): FakeChange[] {
    const bases = diffs[worktree];
    if (!bases) throw `grove does not track a worktree at ${worktree}`;
    const found = bases[base];
    if (!found) {
      throw `the repo at ${worktree} has no origin/HEAD, main or master to use as the base branch`;
    }
    return found;
  }

  return {
    async addRepo(path) {
      const found = repos[path];
      if (!found) throw `${path} is not a git repo`;
      if (tracked.some((w) => w.path === found[0].path)) {
        throw `the repo at ${found[0].path} is already added`;
      }
      tracked.push(
        ...found.map((w) => ({
          displayName: w.branch ?? folderName(w.path),
          issueLink: null,
          baseBranch: "main",
          nestedUnder: null,
          setup: null,
          ...w,
        })),
      );
    },
    async pickFolder() {
      foldersPicked++;
      const picked = nextFolder;
      nextFolder = null;
      return picked;
    },
    willPickFolder(path) {
      nextFolder = path;
    },
    get foldersPicked() {
      return foldersPicked;
    },
    nestWorktree(worktree, from) {
      const found = tracked.find((w) => w.path === worktree);
      if (!found) throw `grove does not track ${worktree}`;
      found.nestedUnder ??= from;
    },
    async listWorktrees() {
      return tracked.map((w) => ({ ...w }));
    },
    async diff(worktree, base) {
      return changes(worktree, base).map((change) => change.file);
    },
    async fileVersions(worktree, base, file) {
      const found = changes(worktree, base).find((change) => change.file.newPath === file.newPath);
      if (!found) throw `${file.newPath} has no change`;
      return found.versions;
    },
    onDiffChanged(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    onAgentState(listener) {
      stateListeners.add(listener);
      return () => stateListeners.delete(listener);
    },
    onWarning(listener) {
      warningListeners.add(listener);
      warnings.forEach(listener);
      return () => warningListeners.delete(listener);
    },
    async retryWatcher(worktree) {
      watcherRetries.push(worktree);
      checkTracked(worktree);
      const reason = watcherFailures.get(worktree);
      if (reason !== undefined) throw reason;
    },
    // `Done` becomes `Idle` and keeps its source, as in the core.
    async focus(session) {
      focused.push(session);
      const worktree = agentWorktrees.get(session);
      if (worktree !== undefined && running.has(session)) lastFocused.set(worktree, session);
      const last = agentStates.get(session);
      if (last?.state === "Done") changeState({ ...last, state: "Idle" });
    },
    async openInEditor(editor, worktree, file, line) {
      opened.push({ editor, worktree, file, line });
    },
    async revealInFinder(worktree) {
      checkTracked(worktree);
      revealed.push(worktree);
    },
    // As in the core: only web links open.
    async openLinkInDefaultBrowser(link) {
      if (!/^https?:\/\/./i.test(link)) {
        throw `${JSON.stringify(link)} is not an http or https link, so grove does not open it`;
      }
      openedLinks.push(link);
    },
    // As in the core: one shell per worktree.
    async spawnShell(worktree, cols, rows) {
      checkTracked(worktree);
      const open = spawned.find((s) => s.worktree === worktree && running.has(s.session));
      if (open) return open.session;
      const session = start(worktree, cols, rows);
      spawned.push({ session, worktree, cols, rows });
      return session;
    },
    async spawnRun(worktree, name, cols, rows) {
      checkTracked(worktree);
      const repo = mainOf(tracked.find((w) => w.path === worktree)!).path;
      if (!runSessionsOf(repo).some((run) => run.name === name)) {
        throw `the repo at ${repo} has no run session ${JSON.stringify(name)}`;
      }
      const session = start(worktree, cols, rows);
      spawnedRuns.push({ session, worktree, name, cols, rows });
      dropStoppedRun(worktree, name);
      return session;
    },
    onSessionExited(listener) {
      exitListeners.add(listener);
      return () => exitListeners.delete(listener);
    },
    async spawnAgent(worktree, agent, panel) {
      return spawnAgentInPanel(worktree, agent, panel);
    },
    async sessions(worktree) {
      checkTracked(worktree);
      const listed: SessionInfo[] = [
        ...spawnedAgents
          .filter((s) => s.worktree === worktree && (running.has(s.session) || switching.has(s.session)))
          .map(({ session, agent }) => {
            const mode = modes.get(session)!;
            return { session, running: true, kind: "Agent" as const, agent, mode };
          }),
        ...spawned
          .filter((s) => s.worktree === worktree && running.has(s.session))
          .map(({ session }) => ({ session, running: true, kind: "Shell" as const })),
        ...spawnedRuns
          .filter((s) => s.worktree === worktree && running.has(s.session))
          .map(({ session, name }) => ({ session, running: true, kind: "Run" as const, name })),
        ...stoppedRuns
          .filter((s) => s.worktree === worktree)
          .map(({ session, name }) => ({ session, running: false, kind: "Run" as const, name })),
      ];
      return listed.sort((a, b) => Number(a.session) - Number(b.session));
    },
    async subscribe(session, onMessages) {
      const history = chat(session);
      subscribed.push(session);
      messageSinks.set(session, onMessages);
      return [...history];
    },
    // The core adds the user's message to the conversation before any reply can come.
    async sendMessage(session, text) {
      chat(session);
      sent.push({ session, text });
      addMessages(session, [{ type: "User", text }]);
    },
    async interrupt(session) {
      chat(session);
      interrupted.push(session);
    },
    // As in the core: only a request of the current turn that has no answer yet, with a reply that fits it.
    async answer(session, request, reply) {
      const history = chat(session);
      const at = history.findIndex(
        (m) => (m.type === "ApprovalRequested" || m.type === "QuestionAsked") && m.request === request,
      );
      const after = at < 0 ? [] : history.slice(at);
      const asked = after[0];
      const ended = after.some((m) => m.type === "TurnEnded" || (m.type === "Answered" && m.request === request));
      if (!asked || ended) throw `agent session ${session} has no pending request ${request}`;
      const fits =
        reply.type === "Deny" ||
        (asked.type === "QuestionAsked"
          ? reply.type === "Answers" && reply.answers.length === asked.questions.length
          : reply.type !== "Answers");
      if (!fits) throw `the reply does not fit request ${request}`;
      answered.push({ session, request, reply });
      addMessages(session, [{ type: "Answered", request, reply }]);
    },
    async models(session) {
      chat(session);
      const agent = spawnedAgents.find((s) => s.session === session)!.agent;
      return agentModels[agent];
    },
    async setModel(session, model, effort) {
      chat(session);
      modelsSet.push({ session, model, effort });
    },
    async modelChoice(session) {
      chat(session);
      const last = modelsSet.filter((set) => set.session === session).at(-1);
      return last ? { model: last.model, effort: last.effort } : null;
    },
    onAccount(listener) {
      accountListeners.add(listener);
      accounts.forEach(listener);
      return () => accountListeners.delete(listener);
    },
    async write(session, bytes) {
      check(session);
      writes.push({ session, bytes });
    },
    async resize(session, cols, rows) {
      check(session);
      resizes.push({ session, cols, rows });
      sizes.set(session, { cols, rows });
    },
    // A run session that exited returns its final screen, and no frames follow.
    async attach(session, onFrame) {
      if (!stoppedRuns.some((s) => s.session === session && s.kept)) {
        check(session);
        sinks.set(session, onFrame);
      }
      return {
        seq: lastSeq.get(session) ?? 0,
        ...sizes.get(session)!,
        bytes: printed.get(session)!,
      };
    },
    async ack(session, bytes) {
      acks.push({ session, bytes });
    },
    async detach(session) {
      sinks.delete(session);
    },
    async kill(session) {
      check(session);
      killed.push(session);
      sessionExited(session);
    },
    diffChanged(worktree) {
      listeners.forEach((listener) => listener(worktree));
      placeComments(worktree);
    },
    agentStateChanged: changeState,
    warn(warning) {
      const sent = { ...warning, id: warnings.length };
      warnings.push(sent);
      warningListeners.forEach((listener) => listener(sent));
    },
    failWatcherRetry(worktree, reason) {
      if (reason === null) watcherFailures.delete(worktree);
      else watcherFailures.set(worktree, reason);
    },
    watcherRetries,
    accountChanged(change) {
      accounts.set(change.agent, change);
      accountListeners.forEach((listener) => listener(change));
    },
    messages: (session, ...messages) => addMessages(session, messages.map(withTime)),
    opened,
    revealed,
    openedLinks,
    spawned,
    spawnedAgents,
    sent,
    interrupted,
    answered,
    agentModels,
    modelsSet,
    subscribed,
    writes,
    resizes,
    acks,
    focused,
    killed,
    attached: (session) => sinks.has(session),
    output(session, bytes) {
      const before = printed.get(session)!;
      const after = new Uint8Array(before.length + bytes.length);
      after.set(before);
      after.set(bytes, before.length);
      printed.set(session, after);
      const sink = sinks.get(session);
      if (!sink) return;
      const seq = (lastSeq.get(session) ?? 0) + 1;
      lastSeq.set(session, seq);
      sink({ seq, bytes });
    },
    setSnapshot(session, bytes) {
      printed.set(session, bytes);
    },
    sendFrame(session, frame) {
      sinks.get(session)?.(frame);
    },

    // Files panel (#124).
    async listFiles(worktree) {
      return Object.keys(filesOf(worktree)).sort();
    },
    async readFile(worktree, file) {
      readFiles.push({ worktree, file });
      const contents = filesOf(worktree)[file];
      if (contents === undefined) throw `could not read ${worktree}/${file}: No such file or directory`;
      return typeof contents === "string" ? new TextEncoder().encode(contents) : contents;
    },
    async writeFile(worktree, file, text) {
      filesOf(worktree)[file] = text;
      writtenFiles.push({ worktree, file, text });
    },
    async lineChanges(worktree, file) {
      return changes(worktree, "Head").find((change) => change.file.newPath === file)?.lines ?? [];
    },
    setFile(worktree, file, contents) {
      filesOf(worktree)[file] = contents;
    },
    readFiles,
    writtenFiles,

    // Attention queue (#53). As in the core: by rank, then by when each entered its state.
    async attentionQueue() {
      return [...agentStates.values()]
        .filter((change) => attentionRank(change) !== undefined)
        .sort(
          (a, b) =>
            attentionRank(a)! - attentionRank(b)! || entered.get(a.session)! - entered.get(b.session)!,
        )
        .map((change) => ({ session: change.session, worktree: agentWorktrees.get(change.session)! }));
    },

    // Workspaces (#122). As in the core: a panel whose session no longer runs in the worktree is left out.
    async workspace(worktree) {
      checkTracked(worktree);
      return workspaceOf(worktree);
    },
    async setWorkspace(worktree, workspace) {
      checkTracked(worktree);
      if (workspace.selected !== null && !workspace.panels.includes(workspace.selected)) {
        throw "invalid workspace: its selected tab is not one of its tabs";
      }
      workspaces.set(worktree, structuredClone(workspace));
      workspacesSet.push({ worktree, workspace: structuredClone(workspace) });
      workspaceChanged(worktree);
    },
    onWorkspaceChanged(listener) {
      workspaceListeners.add(listener);
      return () => workspaceListeners.delete(listener);
    },
    workspacesSet,
    // The agent change notice (#4).
    onAgentChange(listener) {
      agentChangeListeners.add(listener);
      return () => agentChangeListeners.delete(listener);
    },
    agentChangesWorkspace(worktree, by, workspace) {
      checkTracked(worktree);
      beforeAgentChange.set(worktree, structuredClone(workspaces.get(worktree) ?? DEFAULT_WORKSPACE));
      workspaces.set(worktree, structuredClone(workspace));
      agentChangeListeners.forEach((listener) => listener({ kind: "Workspace", worktree, by }));
      workspaceChanged(worktree);
    },
    agentChangesProfile(by) {
      agentChangeListeners.forEach((listener) => listener({ kind: "Profiles", by }));
      profilesChanged();
    },
    async undoWorkspace(worktree) {
      checkTracked(worktree);
      const before = beforeAgentChange.get(worktree);
      if (!before) throw `no agent change to the workspace of ${worktree} is left to undo`;
      beforeAgentChange.delete(worktree);
      workspaces.set(worktree, before);
      undone.push(worktree);
      workspaceChanged(worktree);
    },
    async saveWorkspaceToProfile(worktree) {
      checkTracked(worktree);
      const at = tracked.findIndex((w) => w.path === worktree);
      const repo = tracked.slice(0, at + 1).filter((w) => w.isMain).pop()!.path;
      if (!repoProfiles.get(repo)) throw `the repo at ${repo} uses no profile`;
      savedToProfile.push(worktree);
      profilesChanged();
    },
    undone,
    savedToProfile,
    spawnAgentWithoutPanel: startAgentSession,
    holdWorkspaceChanges() {
      heldWorkspaceChanges = [];
      return () => {
        const held = heldWorkspaceChanges ?? [];
        heldWorkspaceChanges = null;
        held.forEach(workspaceChanged);
      };
    },

    // Worktree management (#52). As in the core, with folders under /grovecode/worktrees.
    async createWorktree(repo, worktree) {
      const own = repoAt(repo);
      const { branch } = worktree;
      if (!/^[\w./-]+$/.test(branch) || branch.includes("..") || branch.startsWith("-")) {
        throw `${JSON.stringify(branch)} is not a valid branch name: fatal: '${branch}' is not a valid branch name`;
      }
      const path = `/grovecode/worktrees/${folderName(repo)}/${branch.replaceAll("/", "-")}`;
      if (tracked.some((w) => w.path === path)) throw `${path} is already taken`;
      if (worktree.baseBranch !== null) checkBranch(own[0], worktree.baseBranch);
      const added: Worktree = {
        path,
        branch,
        isMain: false,
        displayName: worktree.displayName ?? branch,
        issueLink: worktree.issueLink,
        baseBranch: worktree.baseBranch ?? own[0].baseBranch,
        nestedUnder: null,
        setup: null,
      };
      tracked.splice(tracked.indexOf(own[own.length - 1]) + 1, 0, added);
      files[path] = {};
      diffs[path] = { Head: [], MergeBase: [] };
      // The repo's profile has the setup script, which runs until a test finishes it.
      if (profiles.find((p) => p.name === repoProfiles.get(repo))?.setup) {
        setSetup(path, { state: "Running" });
      }
      return { ...added };
    },
    async setBaseBranch(worktree, base) {
      const found = tracked.find((w) => w.path === worktree);
      if (!found) throw `grove does not track a worktree at ${worktree}`;
      checkBranch(found, base);
      found.baseBranch = base;
    },
    async forgetWorktree(worktree) {
      linked(worktree);
      untrack(worktree);
    },
    async deleteCheck(worktree) {
      const { uncommitted, unpushed, merged } = statusOf(worktree);
      return { uncommitted, unpushed, branch: linked(worktree).branch, merged };
    },
    async deleteWorktree(worktree, confirmed, deleteBranch) {
      const { branch, baseBranch } = linked(worktree);
      const status = statusOf(worktree);
      if (!confirmed && (status.uncommitted || status.unpushed > 0)) {
        throw `${worktree} has ${unsaved(status)}; confirm to delete it`;
      }
      if (deleteBranch && branch !== null && !status.merged) {
        throw `branch ${JSON.stringify(branch)} is not merged into ${JSON.stringify(baseBranch)}, so grove keeps it`;
      }
      deleted.push({ worktree, confirmed, deleteBranch });
      untrack(worktree);
      if (branch === null) return { outcome: "Untouched" };
      if (!deleteBranch) return { outcome: "Kept", name: branch, merged: status.merged };
      if (status.branchNotDeleted !== null) {
        return { outcome: "NotDeleted", name: branch, reason: status.branchNotDeleted };
      }
      return { outcome: "Deleted", name: branch };
    },
    async profileNames() {
      for (const [name, text] of profileTexts) {
        const problem = profileProblem(text);
        if (problem) {
          throw `the profile /home/.grovecode/profiles/${profileFileName(name)} is not valid at line ${problem.line}: ${problem.message}`;
        }
      }
      return profiles.map((p) => p.name).sort();
    },
    async suggestProfile(repo) {
      const present = files[repo] ?? {};
      for (const profile of [...profiles].sort((a, b) => (a.name < b.name ? -1 : 1))) {
        const marker = profile.suggestFor.find((m) => m in present);
        if (marker) return { profile: profile.name, marker };
      }
      return null;
    },
    async repoProfile(repo) {
      repoAt(repo);
      return repoProfiles.get(repo) ?? null;
    },
    async setRepoProfile(repo, profile) {
      repoAt(repo);
      if (profile !== null && !profiles.some((p) => p.name === profile)) {
        throw `there is no valid profile named ${JSON.stringify(profile)}`;
      }
      repoProfiles.set(repo, profile);
      repoSettingsChanged(repo);
    },
    // Profile panel (#57). The fake core checks TOML roughly, line by line.
    async profileText(name) {
      const text = profileTexts.get(name);
      if (text === undefined) throw `there is no profile named ${JSON.stringify(name)}`;
      return { path: `/home/.grovecode/profiles/${profileFileName(name)}`, text };
    },
    async setProfileText(name, text) {
      if (!profileTexts.has(name)) {
        throw { line: null, message: `there is no profile named ${JSON.stringify(name)}` } satisfies TextProblem;
      }
      const named = nameIn(text);
      const problem =
        profileProblem(text) ??
        (named !== null && named.name !== name
          ? { line: named.line, message: `name: it must stay ${JSON.stringify(name)}: renaming a profile is not supported` }
          : null);
      if (problem) throw problem;
      profileTexts.set(name, text);
      profileTextsSet.push({ name, text });
      profilesChanged();
    },
    async repoOverridesText(repo) {
      repoAt(repo);
      return overrides.get(repo) ?? "";
    },
    // As in the core, which keeps the overrides as values, comments are dropped.
    async setRepoOverridesText(repo, text) {
      repoAt(repo);
      const named = nameIn(text);
      const problem = tomlProblem(text) ?? (named && { line: named.line, message: "unknown field `name`" });
      if (problem) throw problem;
      overrides.set(repo, text.split("\n").filter((line) => !line.trim().startsWith("#")).join("\n"));
      overridesSet.push({ repo, text });
      repoSettingsChanged(repo);
    },
    async runSessions(repo) {
      repoAt(repo);
      return runSessionsOf(repo);
    },
    spawnedRuns,
    restoreStoppedRun(worktree, name) {
      checkTracked(worktree);
      const session = String(sessions++);
      stoppedRuns.push({ session, worktree, name, kept: false });
      return session;
    },
    exit(session) {
      check(session);
      sessionExited(session);
    },
    onProfilesChanged(listener) {
      profilesListeners.add(listener);
      return () => profilesListeners.delete(listener);
    },
    onRepoSettingsChanged(listener) {
      repoSettingsListeners.add(listener);
      return () => repoSettingsListeners.delete(listener);
    },
    changeProfileOnDisk(name, text) {
      profileTexts.set(name, text);
      profilesChanged();
    },
    profileTextsSet,
    overridesSet,
    onQuitRequested(listener) {
      quitListeners.add(listener);
      return () => quitListeners.delete(listener);
    },
    async quit() {
      quits++;
    },
    setStatus(worktree, status) {
      statuses.set(worktree, { ...statusOf(worktree), ...status });
    },
    deleted,
    requestQuit: () => quitListeners.forEach((listener) => listener()),
    onFullscreenChanged(listener) {
      fullscreenListeners.add(listener);
      listener(fullscreen);
      return () => fullscreenListeners.delete(listener);
    },
    setFullscreen(on) {
      fullscreen = on;
      fullscreenListeners.forEach((listener) => listener(on));
    },
    onNotificationClicked(listener) {
      notificationListeners.add(listener);
      return () => notificationListeners.delete(listener);
    },
    clickNotification: (clicked) => notificationListeners.forEach((listener) => listener(clicked)),
    quitCount: () => quits,
    async spawnAgentAfterSetup(worktree, agent, panel) {
      if (setupOf(worktree)?.state === "Running") {
        await new Promise<void>((resolve) => setupWaiters.set(worktree, [...(setupWaiters.get(worktree) ?? []), resolve]));
      }
      const setup = setupOf(worktree);
      if (setup?.state === "Failed") {
        const ended = setup.code === null ? "was stopped by a signal" : `exited with code ${setup.code}`;
        throw `the setup script in ${worktree} ${ended}, so no agent session started`;
      }
      return spawnAgentInPanel(worktree, agent, panel);
    },
    onSetup(listener) {
      setupListeners.add(listener);
      return () => setupListeners.delete(listener);
    },
    finishSetup(worktree, code) {
      setSetup(worktree, code === 0 ? { state: "Succeeded" } : { state: "Failed", code });
      setupWaiters.get(worktree)?.forEach((resume) => resume());
      setupWaiters.delete(worktree);
    },
    // Terminal mode (#127). As in the core: the old process ends, and a new one takes its id with
    // the same conversation. Its output and its subscription start over.
    async setMode(session, mode) {
      const current = modes.get(session);
      if (!current) throw `session ${session} is not an agent session`;
      const state = agentStates.get(session)?.state;
      if (state === "Working" || state === "NeedsInput") {
        throw `agent session ${session} cannot switch mode while ${state}`;
      }
      modesSet.push({ session, mode });
      if (mode === current) return;
      if (heldModeSwitches) {
        const held = heldModeSwitches;
        running.delete(session);
        switching.add(session);
        await new Promise<void>((resume) => held.push(resume));
        switching.delete(session);
      }
      modes.set(session, mode);
      pastes.delete(session);
      messageSinks.delete(session);
      sinks.delete(session);
      lastSeq.delete(session);
      printed.set(session, new Uint8Array());
      running.add(session);
    },
    modesSet,
    holdModeSwitches() {
      heldModeSwitches = [];
      return () => {
        const held = heldModeSwitches ?? [];
        heldModeSwitches = null;
        held.forEach((resume) => resume());
      };
    },

    // Review comments (#54). As in the core.
    async addComment(worktree, { path, startLine, endLine, body }) {
      checkTracked(worktree);
      const contents = filesOf(worktree)[path];
      if (contents === undefined) throw `could not read ${worktree}/${path}: No such file or directory`;
      const lines = fileLines(typeof contents === "string" ? contents : new TextDecoder().decode(contents));
      if (startLine < 1 || endLine < startLine || endLine > lines.length) {
        throw `${worktree}/${path} has no lines ${startLine} to ${endLine}`;
      }
      const text = lines.slice(startLine - 1, endLine).join("\n");
      const id = nextComment++;
      const comment: ReviewComment = { id, path, startLine, endLine, text, body, state: "Open", possiblyAddressed: false };
      comments.set(worktree, [...(comments.get(worktree) ?? []), comment]);
      commentsChanged(worktree);
      return id;
    },
    async comments(worktree) {
      checkTracked(worktree);
      return (comments.get(worktree) ?? []).filter((c) => c.state !== "Resolved").map((c) => ({ ...c }));
    },
    async resolveComment(id) {
      for (const [worktree, all] of comments) {
        const comment = all.find((c) => c.id === id);
        if (!comment) continue;
        if (comment.state === "Resolved") return;
        comment.state = "Resolved";
        commentsChanged(worktree);
        return;
      }
      throw `there is no review comment ${id}`;
    },
    async sendReviewBatch(worktree, to) {
      checkTracked(worktree);
      const session = to ?? lastFocused.get(worktree);
      if (session === undefined || agentWorktrees.get(session) !== worktree || !running.has(session)) {
        throw `no agent session in ${worktree} to send the review batch to`;
      }
      const state = agentStates.get(session);
      if (state?.state !== "Idle" && state?.state !== "Done") {
        const now = !state ? "before its first state" : `while ${state.state === "NeedsInput" ? `NeedsInput { kind: ${state.kind} }` : state.state}`;
        throw `agent session ${session} cannot take a review batch ${now}`;
      }
      const chat = modes.get(session) === "Chat";
      if (!chat && !pastes.get(session)) {
        throw `agent session ${session} has bracketed paste off, so it cannot take a review batch`;
      }
      const open = (comments.get(worktree) ?? []).filter((c) => c.state === "Open");
      if (open.length === 0) throw `${worktree} has no open review comments to send`;
      const batch: BatchComment[] = open.map(({ path, startLine, endLine, text, body }) => ({ path, startLine, endLine, text, body }));
      if (chat) addMessages(session, [{ type: "ReviewBatch", comments: batch }]);
      open.forEach((c) => (c.state = "Sent"));
      reviewBatches.push({ worktree, to: session });
      commentsChanged(worktree);
    },
    onCommentsChanged(listener) {
      commentListeners.add(listener);
      return () => commentListeners.delete(listener);
    },
    async lastFocusedAgent(worktree) {
      return lastFocused.get(worktree) ?? null;
    },
    async bracketedPaste(session) {
      check(session);
      if (modes.get(session) === "Chat") throw `session ${session} is an agent session in chat mode, which has no terminal`;
      return pastes.get(session) ?? false;
    },
    setBracketedPaste(session, on) {
      pastes.set(session, on);
    },
    reviewBatches,
  };
}

// What deleting a worktree would lose, as the core says it.
function unsaved({ uncommitted, unpushed }: FakeWorktreeStatus): string {
  const commits = unpushed === 0 ? [] : [unpushed === 1 ? "1 commit on no remote" : `${unpushed} commits on no remote`];
  return [...(uncommitted ? ["uncommitted changes"] : []), ...commits].join(" and ");
}

// A profile's file with the fields the fake knows.
function profileFile({ name, suggestFor, setup }: FakeProfile): string {
  const lines = [`name = ${JSON.stringify(name)}`, `suggest_for = ${JSON.stringify(suggestFor)}`];
  if (setup) lines.push(`setup = ${JSON.stringify(setup)}`);
  return `${lines.join("\n")}\n`;
}

// As the core names a new profile's file, as in `next-js-app.toml`.
function profileFileName(name: string): string {
  return `${name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "")}.toml`;
}

// The first line that is not blank, a comment, a table header or `key = value`. Rougher than
// the core's TOML parser, but it points at a line the same way.
function tomlProblem(text: string): TextProblem | null {
  const at = text.split("\n").findIndex((raw) => {
    const line = raw.trim();
    return !(line === "" || line.startsWith("#") || /^\[\[?[\w.-]+\]\]?$/.test(line) || /^[\w.-]+\s*=\s*\S/.test(line));
  });
  return at < 0 ? null : { line: at + 1, message: "expected `=`" };
}

// Why `text` is not a valid profile: not TOML, or no `name`. null when it is.
function profileProblem(text: string): TextProblem | null {
  return tomlProblem(text) ?? (nameIn(text) === null ? { line: 1, message: "missing field `name`" } : null);
}

// The `[[run_session]]` tables in a profile or overrides text, read roughly. null when it has none,
// as the core reads a missing list, so the overrides keep the profile's.
function runSessionsIn(text: string): RunSession[] | null {
  const tables = text.split(/^\s*\[\[run_session\]\]\s*$/m).slice(1);
  if (tables.length === 0) return null;
  return tables.map((table) => {
    const body = table.split(/^\s*\[/m)[0];
    const value = (key: string) => new RegExp(`^\\s*${key}\\s*=\\s*(.+)$`, "m").exec(body)?.[1].trim();
    const port = value("port");
    return {
      name: JSON.parse(value("name") ?? '""') as string,
      command: JSON.parse(value("command") ?? '""') as string,
      port: port === undefined ? null : Number(port),
    };
  });
}

// The top-level `name = "..."` and its line. null when there is none before the first table.
function nameIn(text: string): { name: string; line: number } | null {
  const lines = text.split("\n");
  for (const [i, line] of lines.entries()) {
    if (line.trim().startsWith("[")) break;
    const found = /^\s*name\s*=\s*"([^"]*)"/.exec(line);
    if (found) return { name: found[1], line: i + 1 };
  }
  return null;
}

function folderName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}

// A state's place in the attention queue, as `attention_rank` in the core. undefined for a state
// that does not wait on the user.
function attentionRank(change: AgentStateChanged | undefined): number | undefined {
  if (change?.state === "NeedsInput") return change.kind === "Approval" ? 0 : 1;
  if (change?.state === "Failed") return 2;
  if (change?.state === "Done") return 3;
  return undefined;
}
