import { Bell, ChevronDown, ChevronRight, Ellipsis, Plus, Search, Settings2, SlidersHorizontal } from "lucide-react";
import { type Ref, useEffect, useEffectEvent, useId, useImperativeHandle, useRef, useState } from "react";
import { flushSync } from "react-dom";

import type {
  Account,
  Agent,
  AgentState as State,
  AgentStateChanged,
  BranchOutcome,
  Bridge,
  SessionId,
  SetupState,
  Worktree,
} from "./bridge";
import { useSetting } from "./settings";
import { stateWord } from "./ui/AgentState";
import { IconButton } from "./ui/IconButton";
import { Menu } from "./ui/Menu";
import { Segmented } from "./ui/Segmented";
import { APP_ICONS, type Appearance, PALETTES, THEMES } from "./theme";
import { ChangeBaseDialog, DeleteDialog, ForgetDialog, NewWorktreeDialog } from "./WorktreeDialogs";

/** An agent session the user started, and the worktree it runs in. */
export interface AgentSession {
  session: SessionId;
  worktree: string;
  agent: Agent;
  /** The session the panel started with. It names the panel when another agent takes its place. */
  panel: SessionId;
}

export const AGENT_NAMES: Record<Agent, string> = { Claude: "Claude Code", Codex: "Codex" };

/**
 * What each agent session is called: its agent's name, numbered from the second session of that
 * agent in the same worktree, as "Claude Code", then "Claude Code 2". Never the session id.
 */
export function sessionNames(agents: AgentSession[]): Record<SessionId, string> {
  const counts = new Map<string, number>();
  return Object.fromEntries(
    agents.map(({ session, worktree, agent }) => {
      const key = JSON.stringify([worktree, agent]);
      const count = (counts.get(key) ?? 0) + 1;
      counts.set(key, count);
      return [session, count === 1 ? AGENT_NAMES[agent] : `${AGENT_NAMES[agent]} ${count}`];
    }),
  );
}

/** What the window can ask of the sidebar. */
export interface SidebarHandle {
  /** Opens the folder picker and adds the folder picked, as the sidebar's Add repo does. */
  addRepo(): void;
}

/** What a worktree's menu does. */
type WorktreeAction = "start" | "base" | "forget" | "delete";

/** The dialog the sidebar shows: a new worktree for a repo, or a worktree's menu action. */
type OpenDialog =
  | { kind: "new"; repo: Worktree }
  | { kind: Exclude<WorktreeAction, "start">; worktree: Worktree };

/**
 * Lists every tracked worktree under its repo with its agent sessions, adds repos and selects a
 * worktree. Each repo's + adds a worktree, and each worktree's menu starts Claude Code in it,
 * changes its base branch, forgets it or deletes it.
 *
 * Above the list: New worktree, Search, Needs you and Profiles. Below it: each agent's account and
 * the appearance menu.
 */
export function Sidebar({
  bridge,
  selected,
  onSelect,
  agents,
  agentStates,
  shownAgents,
  onStartAgent,
  onStartAgentAfterSetup,
  onShowAgent,
  onFocusAgent,
  onDeleting,
  onRemoved,
  onWorktrees,
  waiting,
  onJump,
  onOpenProfiles,
  onShowSidebar,
  accounts,
  appearance,
  onAppearance,
  ref,
}: {
  bridge: Bridge;
  selected: string | null;
  /** null when the selected worktree is forgotten or deleted. */
  onSelect(path: string | null): void;
  /** Every agent session started, in order. */
  agents: AgentSession[];
  /** Each agent session's latest state, by session. */
  agentStates: Record<SessionId, AgentStateChanged>;
  /** The agent sessions whose panels the selected worktree shows. */
  shownAgents: SessionId[];
  onStartAgent(worktree: string): void;
  /** Starts an agent session in a new worktree once its setup script succeeds. */
  onStartAgentAfterSetup(worktree: string): void;
  onShowAgent(agent: AgentSession): void;
  /** Shows the agent session's panel and puts focus in it. */
  onFocusAgent(agent: AgentSession): void;
  /** A delete of the worktree started or ended. */
  onDeleting(worktree: Worktree, running: boolean): void;
  /** A worktree was forgotten or deleted, which ended its sessions. */
  onRemoved(path: string): void;
  /** Called with each new list of worktrees, from the core's first one on. */
  onWorktrees?(worktrees: Worktree[]): void;
  /** How many agent sessions are in the attention queue. */
  waiting: number;
  /** Focuses the head of the attention queue. */
  onJump(): void;
  /** Opens the selected worktree's Profile panel. */
  onOpenProfiles(): void;
  /** Shows the sidebar when it is collapsed. Cmd-K calls it before it puts focus in Search. */
  onShowSidebar?(): void;
  /** Each agent's login: null when nobody is logged in, missing until a session reports it. */
  accounts: Partial<Record<Agent, Account | null>>;
  appearance: Appearance;
  onAppearance(update: (appearance: Appearance) => Appearance): void;
  /** Lets the chat area's Add repo use the sidebar's own. */
  ref?: Ref<SidebarHandle>;
}) {
  const [worktrees, setWorktrees] = useState<Worktree[]>([]);
  // False until the core's first list arrives, so an empty list before it means nothing.
  const [listed, setListed] = useState(false);
  const [query, setQuery] = useState("");
  // The repos folded shut, by main worktree path. It survives a restart.
  const [foldedRepos, setFoldedRepos] = useSetting<string[]>("grove.foldedRepos", [], isPathList);
  // The worktrees whose agent session rows are folded shut, by path.
  const [foldedSessions, setFoldedSessions] = useState<string[]>([]);
  const searchBox = useRef<HTMLInputElement>(null);
  // Each setup state the core sent since the sidebar opened. It is newer than any list.
  const [setups, setSetups] = useState<Record<string, SetupState>>({});
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState<OpenDialog | null>(null);

  const reportWorktrees = useEffectEvent((list: Worktree[]) => onWorktrees?.(list));
  useEffect(() => {
    if (listed) reportWorktrees(worktrees);
  }, [listed, worktrees]);

  useEffect(
    () => bridge.onSetup(({ worktree, setup }) => setSetups((known) => ({ ...known, [worktree]: setup }))),
    [bridge],
  );

  // The number of the latest list asked for. An older one that answers late is dropped.
  const latestList = useRef(0);
  function refresh() {
    const asked = ++latestList.current;
    bridge
      .listWorktrees()
      .then((list) => {
        if (asked !== latestList.current) return;
        setWorktrees(list);
        setListed(true);
      })
      .catch((e: unknown) => setError(String(e)));
  }

  // The core outlives the webview, so a reloaded window starts from what the core tracks.
  const listAtStart = useEffectEvent(refresh);
  useEffect(() => listAtStart(), [bridge]);

  // A subagent that starts in another worktree nests that worktree, and no event says so. The
  // first state of a session the sidebar has not seen lists the worktrees again.
  const seenSessions = useRef<Set<SessionId> | null>(null);
  const refreshOnNewSession = useEffectEvent((sessions: SessionId[]) => {
    const seen = seenSessions.current;
    seenSessions.current = new Set(sessions);
    if (seen && sessions.some((session) => !seen.has(session))) refresh();
  });
  useEffect(() => refreshOnNewSession(Object.keys(agentStates)), [agentStates]);

  // `repo` opens, so the new worktree's selected row shows.
  function created(repo: string, worktree: Worktree, startAgent: boolean) {
    setDialog(null);
    setFoldedRepos((folded) => folded.filter((path) => path !== repo));
    refresh();
    onSelect(worktree.path);
    if (startAgent) onStartAgentAfterSetup(worktree.path);
  }

  function removed(worktree: Worktree, outcome?: BranchOutcome) {
    // A dialog about the worktree closes, since it is gone. One the user opened since stays.
    setDialog((open) => (open && open.kind !== "new" && open.worktree.path === worktree.path ? null : open));
    refresh();
    setError(
      outcome?.outcome === "NotDeleted"
        ? `Deleted ${worktree.displayName}, but branch ${outcome.name} is still there: ${outcome.reason}`
        : null,
    );
    if (worktree.path === selected) onSelect(null);
    onRemoved(worktree.path);
    setSetups((known) => {
      const rest = { ...known };
      delete rest[worktree.path];
      return rest;
    });
  }

  // Adds the folder the user picks. Cancelling the picker does nothing.
  async function addRepo() {
    try {
      const picked = await bridge.pickFolder();
      if (picked === null) return;
      await bridge.addRepo(picked);
      setError(null);
    } catch (e) {
      setError(String(e));
      // The chat area's Add repo works with the sidebar collapsed, and the error shows here.
      onShowSidebar?.();
    }
    refresh();
  }

  useImperativeHandle(ref, () => ({ addRepo: () => void addRepo() }));

  function toggleRepo(path: string) {
    setFoldedRepos((folded) => toggled(folded, path));
  }

  function toggleSessions(path: string) {
    setFoldedSessions((folded) => toggled(folded, path));
  }

  const repos = byRepo(worktrees);
  // New worktree adds to the selected worktree's repo, or else to the first repo.
  const newWorktreeRepo = (repos.find((repo) => repo.some((w) => w.path === selected)) ?? repos[0])?.[0];
  function newWorktree() {
    if (newWorktreeRepo) setDialog({ kind: "new", repo: newWorktreeRepo });
  }

  // Cmd-N and Cmd-K, unless a dialog is open or a terminal or an editor took the key.
  const onKey = useEffectEvent((event: KeyboardEvent) => {
    if (event.defaultPrevented || dialog || !event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
    const key = event.key.toLowerCase();
    if (key === "n" && newWorktreeRepo) {
      event.preventDefault();
      newWorktree();
    } else if (key === "k") {
      event.preventDefault();
      // Shown first, since a hidden box takes no focus.
      flushSync(() => onShowSidebar?.());
      searchBox.current?.focus();
    }
  });
  useEffect(() => {
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const names = sessionNames(agents);
  // A worktree's row, then under it its agent sessions and the worktrees nested under it. A nested
  // worktree has only its sessions under it, so worktrees nest one level deep.
  function row(worktree: Worktree, nestedRows: Worktree[], isNested: boolean) {
    const own = agents.filter((agent) => agent.worktree === worktree.path);
    const setup = setups[worktree.path] ?? worktree.setup;
    const children = own.length + nestedRows.length;
    const childrenOpen = !foldedSessions.includes(worktree.path);
    return (
      <li key={worktree.path} title={worktree.path} className={isNested ? "nested-worktree" : undefined}>
        <WorktreeRow
          worktree={worktree}
          selected={worktree.path === selected}
          state={mostUrgent(own.map((agent) => agentStates[agent.session]))}
          setupRunning={setup?.state === "Running"}
          sessionsOpen={children > 0 ? childrenOpen : null}
          onToggleSessions={() => toggleSessions(worktree.path)}
          onClick={() => (own.length === 1 ? onShowAgent(own[0]) : onSelect(worktree.path))}
          onPick={(kind) => {
            if (kind !== "start") return setDialog({ kind, worktree });
            onSelect(worktree.path);
            onStartAgent(worktree.path);
          }}
        />
        {setup && <SetupStatus setup={setup} />}
        {children > 0 && childrenOpen && (
          <ul className="agent-sessions">
            {own.map((agent) => (
              <li key={agent.session}>
                <button
                  className="session-row"
                  aria-current={shownAgents.includes(agent.session)}
                  onClick={() => onFocusAgent(agent)}
                >
                  <span className="session-name">{names[agent.session]}</span>
                  <StatusDot state={agentStates[agent.session]} />
                </button>
              </li>
            ))}
            {nestedRows.map((child) => row(child, [], true))}
          </ul>
        )}
      </li>
    );
  }
  const shownRepos = repos.flatMap((repo) => {
    const shown = repo.filter((worktree) => matches(worktree, query));
    return shown.length > 0 ? [{ head: repo[0], worktrees: shown, nested: nestingParents(repo) }] : [];
  });
  return (
    <aside className="sidebar" aria-label="Worktrees">
      <nav className="sidebar-nav" aria-label="Sidebar">
        <button
          className="nav-row"
          aria-keyshortcuts="Meta+N"
          disabled={!newWorktreeRepo}
          onClick={newWorktree}
        >
          <Plus aria-hidden="true" />
          New worktree
          <kbd aria-hidden="true">⌘N</kbd>
        </button>
        <label className="nav-row search-row">
          <Search aria-hidden="true" />
          <input
            ref={searchBox}
            type="search"
            aria-label="Search"
            aria-keyshortcuts="Meta+K"
            placeholder="Search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") setQuery("");
            }}
          />
          <kbd aria-hidden="true">⌘K</kbd>
        </label>
        <button className="nav-row" aria-keyshortcuts="Meta+J" onClick={onJump}>
          <Bell aria-hidden="true" />
          Needs you{" "}
          <span className={waiting > 0 ? "count waiting" : "count"}>{waiting}</span>
          <kbd aria-hidden="true">⌘J</kbd>
        </button>
        <button className="nav-row" disabled={!selected} onClick={onOpenProfiles}>
          <SlidersHorizontal aria-hidden="true" />
          Profiles
        </button>
      </nav>
      <div className="tree-head">
        <span>Repos</span>
        <IconButton icon={Plus} size="xs" label="Add repo" onClick={() => void addRepo()} />
      </div>
      <div className="sidebar-tree">
        {error && <p role="alert">{error}</p>}
        {shownRepos.map(({ head, worktrees: repo, nested }) => {
          const name = folderName(head.path);
          // Search shows the matching rows of a folded repo too, and keeps it from folding.
          const searching = query.trim() !== "";
          const open = searching || !foldedRepos.includes(head.path);
          return (
            <section key={head.path} aria-label={name}>
              <div className="repo-folder">
                <button
                  className="repo-toggle"
                  aria-expanded={open}
                  disabled={searching}
                  onClick={() => toggleRepo(head.path)}
                >
                  <span className="repo-name">{name}</span>
                  {open ? <ChevronDown aria-hidden="true" /> : <ChevronRight aria-hidden="true" />}
                </button>
                <IconButton
                  icon={Plus}
                  size="xs"
                  className="repo-add"
                  label={`New worktree in ${name}`}
                  title="New worktree"
                  onClick={() => setDialog({ kind: "new", repo: head })}
                />
              </div>
              {open && (
                <ul>
                  {repo
                    // A nested worktree shows under its parent, unless Search hides the parent.
                    .filter((worktree) => !repo.some((parent) => parent.path === nested.get(worktree.path)))
                    .map((worktree) => {
                      const nestedRows = repo.filter((child) => nested.get(child.path) === worktree.path);
                      return row(worktree, nestedRows, false);
                    })}
                </ul>
              )}
            </section>
          );
        })}
      </div>
      <footer className="sidebar-foot" role="group" aria-label="Account and settings">
        <Accounts accounts={accounts} />
        <AppearanceMenu appearance={appearance} onAppearance={onAppearance} />
      </footer>
      {dialog?.kind === "new" && (
        <NewWorktreeDialog
          bridge={bridge}
          repo={dialog.repo}
          onCreated={(worktree, startAgent) => created(dialog.repo.path, worktree, startAgent)}
          onCancel={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "base" && (
        <ChangeBaseDialog
          bridge={bridge}
          worktree={dialog.worktree}
          onSaved={() => {
            setDialog(null);
            refresh();
          }}
          onCancel={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "forget" && (
        <ForgetDialog
          bridge={bridge}
          worktree={dialog.worktree}
          onForgotten={() => removed(dialog.worktree)}
          onCancel={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "delete" && (
        <DeleteDialog
          bridge={bridge}
          worktree={dialog.worktree}
          onDeleting={(running) => onDeleting(dialog.worktree, running)}
          onDeleted={(outcome) => removed(dialog.worktree, outcome)}
          onCancel={() => setDialog(null)}
        />
      )}
    </aside>
  );
}

const isPathList = (value: unknown): value is string[] =>
  Array.isArray(value) && value.every((path) => typeof path === "string");

/** `list` with `item` taken out if it was in, or put in if it was not. */
function toggled(list: string[], item: string): string[] {
  return list.includes(item) ? list.filter((other) => other !== item) : [...list, item];
}

/** Whether Search's `query` finds the worktree, by display name or branch, ignoring case. */
function matches(worktree: Worktree, query: string): boolean {
  const needle = query.trim().toLowerCase();
  return (
    needle === "" ||
    [worktree.displayName, worktree.branch ?? ""].some((text) => text.toLowerCase().includes(needle))
  );
}

/** Each agent's login that a session has reported, Claude's first. */
function Accounts({ accounts }: { accounts: Partial<Record<Agent, Account | null>> }) {
  const known = (Object.keys(AGENT_NAMES) as Agent[]).flatMap((agent) => {
    const account = accounts[agent];
    return account === undefined ? [] : [{ agent, account }];
  });
  return (
    <ul className="accounts">
      {known.map(({ agent, account }) => (
        <li key={agent} title={AGENT_NAMES[agent]}>
          {describeAccount(account)}
        </li>
      ))}
    </ul>
  );
}

function describeAccount(account: Account | null): string {
  if (account === null) return "Not logged in";
  const who = account.email ?? account.organization ?? "Logged in";
  const how = account.plan
    ? `${account.plan} plan`
    : account.apiKeySource && `API key from ${account.apiKeySource}`;
  return how ? `${who} · ${how}` : who;
}

/** The settings button and the menu over it, with the palette, theme and app icon switches. */
function AppearanceMenu({
  appearance,
  onAppearance,
}: {
  appearance: Appearance;
  onAppearance(update: (appearance: Appearance) => Appearance): void;
}) {
  const [open, setOpen] = useState(false);
  const menu = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);

  // A click outside or Escape closes it. When focus is in the menu, Escape puts it back on the
  // button, since the focused option goes away.
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!menu.current?.contains(event.target as Node)) setOpen(false);
    };
    const escape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      if (menu.current?.contains(document.activeElement)) button.current?.focus();
      setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", escape);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", escape);
    };
  }, [open]);

  return (
    <div className="appearance" ref={menu}>
      <IconButton ref={button} icon={Settings2} label="Settings" aria-expanded={open} onClick={() => setOpen(!open)} />
      {open && (
        <div className="pop">
          <h4>Palette</h4>
          <Segmented
            label="Palette"
            options={PALETTES}
            value={appearance.palette}
            onChange={(palette) => onAppearance((a) => ({ ...a, palette }))}
          />
          <h4>Theme</h4>
          <Segmented
            label="Theme"
            options={THEMES}
            value={appearance.theme}
            onChange={(theme) => onAppearance((a) => ({ ...a, theme }))}
          />
          <h4>App icon</h4>
          <Segmented
            label="App icon"
            options={APP_ICONS}
            value={appearance.appIcon}
            onChange={(appIcon) => onAppearance((a) => ({ ...a, appIcon }))}
          />
        </div>
      )}
    </div>
  );
}

// The attention queue's order, most urgent first.
const URGENCY = ["Approval", "Question", "Failed", "Done", "Working", "Idle", "Exited"];

/** The most urgent of `states`, in attention queue order. undefined when none is known yet. */
function mostUrgent(states: (State | undefined)[]): State | undefined {
  const known = states.filter((state) => state !== undefined);
  return known.sort((a, b) => URGENCY.indexOf(stateWord(a)) - URGENCY.indexOf(stateWord(b)))[0];
}

/**
 * A worktree's row: its name, which fades out when cut, then on the right a chevron that folds its
 * agent sessions, its menu and its status dot. The dot's word describes the row's button.
 */
function WorktreeRow({
  worktree,
  selected,
  state,
  setupRunning,
  sessionsOpen,
  onToggleSessions,
  onClick,
  onPick,
}: {
  worktree: Worktree;
  selected: boolean;
  state: State | undefined;
  /** Shows "Setup running" in place of the state. */
  setupRunning: boolean;
  /** Whether its agent session rows show. null when it has no agent session. */
  sessionsOpen: boolean | null;
  onToggleSessions(): void;
  onClick(): void;
  onPick(action: WorktreeAction): void;
}) {
  const dot = useId();
  return (
    <div className={selected ? "wt-row selected" : "wt-row"}>
      <button className="wt-name" aria-current={selected} aria-describedby={dot} onClick={onClick}>
        <span className="wt-title">
          {worktree.displayName}
          {worktree.isMain && " (main worktree)"}
        </span>
      </button>
      {sessionsOpen !== null && (
        <IconButton
          icon={sessionsOpen ? ChevronDown : ChevronRight}
          size="xs"
          className="wt-chevron"
          label={`Sessions of ${worktree.displayName}`}
          title={sessionsOpen ? "Hide agent sessions" : "Show agent sessions"}
          aria-expanded={sessionsOpen}
          onClick={onToggleSessions}
        />
      )}
      <WorktreeMenu worktree={worktree} setupRunning={setupRunning} onPick={onPick} />
      <StatusDot id={dot} state={state} word={setupRunning ? "Setup running" : undefined} />
    </div>
  );
}

/**
 * A 7px dot in the color of the most urgent state, named by the state's word. Idle and Exited
 * show none. A running setup script shows as Working, named `word`.
 */
function StatusDot({ id, state, word }: { id?: string; state: State | undefined; word?: string }) {
  const shown = word ? "Working" : state && stateWord(state);
  if (!shown || shown === "Idle" || shown === "Exited") return null;
  const name = word ?? shown;
  return (
    <span id={id} role="img" aria-label={name} title={name} className={`status-dot status-${shown.toLowerCase()}`} />
  );
}

/**
 * A failed setup script. A running one shows as the Working dot on the worktree's row, named
 * "Setup running", and one that succeeded shows nothing.
 */
function SetupStatus({ setup }: { setup: SetupState }) {
  if (setup.state !== "Failed") return null;
  const ended = setup.code === null ? "was stopped by a signal" : `exited with code ${setup.code}`;
  return <p className="wt-setup state Failed">{`Setup failed: ${ended}`}</p>;
}

/**
 * A worktree's menu. A main worktree is forgotten only with its repo, so its menu has neither.
 * `spawn_agent` does not wait for setup, so Start Claude Code waits for it instead.
 */
function WorktreeMenu({
  worktree,
  setupRunning,
  onPick,
}: {
  worktree: Worktree;
  setupRunning: boolean;
  onPick(action: WorktreeAction): void;
}) {
  const items: [WorktreeAction, string][] = [
    ...(setupRunning ? [] : [["start", "Start Claude Code"] as [WorktreeAction, string]]),
    ["base", "Change base branch"],
    ...(worktree.isMain
      ? []
      : ([
          ["forget", "Forget"],
          ["delete", "Delete"],
        ] as [WorktreeAction, string][])),
  ];
  return (
    <Menu
      label={`Actions for ${worktree.displayName}`}
      icon={Ellipsis}
      align="end"
      items={items.map(([action, label]) => ({ key: action, label, onSelect: () => onPick(action) }))}
    />
  );
}

/**
 * The worktree each nested worktree of one repo shows under, by path: the top of its chain. A
 * worktree of another repo, or a loop, nests nothing. The core writes neither, but an older state
 * file may hold one. The main worktree never nests.
 */
export function nestingParents(repo: Worktree[]): Map<string, string> {
  const byPath = new Map(repo.map((worktree) => [worktree.path, worktree]));
  const parentOf = (worktree: Worktree) =>
    worktree.isMain || worktree.nestedUnder === null ? undefined : byPath.get(worktree.nestedUnder);
  const nested = new Map<string, string>();
  for (const worktree of repo) {
    const seen = new Set([worktree.path]);
    let top = parentOf(worktree);
    while (top) {
      const up = parentOf(top);
      if (!up) break;
      if (seen.has(top.path)) {
        top = undefined;
        break;
      }
      seen.add(top.path);
      top = up;
    }
    if (top && !seen.has(top.path)) nested.set(worktree.path, top.path);
  }
  return nested;
}

// The core lists worktrees repo by repo, main worktree first, so each main worktree starts a repo.
export function byRepo(worktrees: Worktree[]): Worktree[][] {
  const repos: Worktree[][] = [];
  for (const worktree of worktrees) {
    if (worktree.isMain || repos.length === 0) repos.push([]);
    repos[repos.length - 1].push(worktree);
  }
  return repos;
}

export function folderName(path: string): string {
  return path.split("/").filter(Boolean).pop() ?? path;
}
