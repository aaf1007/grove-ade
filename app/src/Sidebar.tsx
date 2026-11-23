import {
  Bell,
  ChevronDown,
  ChevronRight,
  Ellipsis,
  FolderGit2,
  Loader,
  PanelLeft,
  Plus,
  Search,
  Settings2,
  SlidersHorizontal,
} from "lucide-react";
import { type Ref, useEffect, useEffectEvent, useId, useImperativeHandle, useRef, useState } from "react";
import { flushSync } from "react-dom";

import type {
  Account,
  Agent,
  AgentSessionRecord,
  AgentState as State,
  AgentStateChanged,
  BranchOutcome,
  Bridge,
  SessionId,
  SetupState,
  Worktree,
} from "./bridge";
import { Actions, Dialog } from "./Dialog";
import { useSetting } from "./settings";
import { keyHint, shortcut } from "./shortcuts";
import { sameState, stateWord } from "./ui/AgentState";
import { IconButton } from "./ui/IconButton";
import { Menu, type MenuHandle, type MenuItem } from "./ui/Menu";
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
  /** Puts focus on the sidebar switch in the top row. */
  focusSidebarSwitch(): void;
  /** Opens New session's place picker, as the sidebar's button does. */
  newSession(): void;
  /** Shows the sidebar and puts focus in Search. */
  focusSearch(): void;
}

/** What a worktree's menu does. */
type WorktreeAction = "start" | WorktreeDialogAction;

/** The worktree actions that open a dialog. A session row's menu has them too. */
type WorktreeDialogAction = "base" | "forget" | "delete";

/**
 * The dialog the sidebar shows: a new worktree for a repo, a worktree action from a menu, or
 * Delete session.
 */
type OpenDialog =
  | { kind: "new"; repo: Worktree }
  | { kind: WorktreeDialogAction; worktree: Worktree }
  | { kind: "deleteSession"; session: AgentSessionRecord };

/**
 * Lists each project, then its agent sessions as the core lists them, ended ones too, each
 * subagent session under the session that spawned it. Below them, dimmed, the project's worktrees
 * that have no session. Adds repos, selects a session or a worktree, and deletes a session. Each
 * project's + opens New session for that project. The menu of a worktree with no session starts Claude Code in it, changes its base branch, forgets it
 * or deletes it. The menu of a session in a worktree does the last three for that worktree.
 *
 * Above the list: New session, Search, Needs you and Profiles. Below it: each agent's account and
 * the appearance menu.
 */
export function Sidebar({
  bridge,
  selected,
  onSelect,
  agentStates,
  shownAgents,
  onStartAgent,
  onStartAgentAfterSetup,
  onNewSession,
  onFocusAgent,
  onSessionResumed,
  onSessionsDeleted,
  onDeleting,
  onRemoved,
  onWorktrees,
  onSessions,
  waiting,
  onJump,
  onOpenProfiles,
  onShowSidebar,
  onHideSidebar,
  accounts,
  appearance,
  onAppearance,
  ref,
}: {
  bridge: Bridge;
  selected: string | null;
  /** null when the selected worktree is forgotten or deleted. */
  onSelect(path: string | null): void;
  /** Each agent session's latest state, by session. Newer than the core's list. */
  agentStates: Record<SessionId, AgentStateChanged>;
  /** The agent sessions whose panels the selected worktree shows. */
  shownAgents: SessionId[];
  onStartAgent(worktree: string): void;
  /** Starts an agent session in a worktree once its setup script succeeds. */
  onStartAgentAfterSetup(worktree: string): void;
  /** Starts an agent session in the place New session picked and puts focus in its panel. */
  onNewSession(worktree: string): void;
  /** Selects the agent session's worktree, shows its panel and puts focus in it. */
  onFocusAgent(agent: AgentSession): void;
  /** The ended agent session was resumed, so its panel takes the new process. */
  onSessionResumed(session: SessionId): void;
  /** Agent sessions were deleted, a session with its subagent sessions. */
  onSessionsDeleted(sessions: SessionId[]): void;
  /** A delete of the worktree started or ended. */
  onDeleting(worktree: Worktree, running: boolean): void;
  /** A worktree was forgotten or deleted, which ended its sessions. */
  onRemoved(path: string): void;
  /** Called with each new list of worktrees, from the core's first one on. */
  onWorktrees?(worktrees: Worktree[]): void;
  /** Called with every agent session's record each time the core's list arrives. */
  onSessions?(sessions: AgentSessionRecord[]): void;
  /** How many agent sessions are in the attention queue. */
  waiting: number;
  /** Focuses the head of the attention queue. */
  onJump(): void;
  /** Opens the selected worktree's Profile panel. */
  onOpenProfiles(): void;
  /** Shows the sidebar when it is collapsed. Cmd-K calls it before it puts focus in Search. */
  onShowSidebar?(): void;
  /** Collapses the sidebar. The title bar's switch shows it again. */
  onHideSidebar(): void;
  /** Each agent's login: null when nobody is logged in, missing until a session reports it. */
  accounts: Partial<Record<Agent, Account | null>>;
  appearance: Appearance;
  onAppearance(update: (appearance: Appearance) => Appearance): void;
  /** Lets the chat area's Add repo use the sidebar's own. */
  ref?: Ref<SidebarHandle>;
}) {
  const [worktrees, setWorktrees] = useState<Worktree[]>([]);
  // Every agent session, as the core last listed them.
  const [sessions, setSessions] = useState<AgentSessionRecord[]>([]);
  // False until the core's first list arrives, so an empty list before it means nothing.
  const [listed, setListed] = useState(false);
  const [query, setQuery] = useState("");
  // The repos folded shut, by main worktree path. It survives a restart.
  const [foldedRepos, setFoldedRepos] = useSetting<string[]>("grove.foldedRepos", [], isPathList);
  const searchBox = useRef<HTMLInputElement>(null);
  // Each setup state the core sent since the sidebar opened. It is newer than any list.
  const [setups, setSetups] = useState<Record<string, SetupState>>({});
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState<OpenDialog | null>(null);

  const reportWorktrees = useEffectEvent((list: Worktree[]) => onWorktrees?.(list));
  useEffect(() => {
    if (listed) reportWorktrees(worktrees);
  }, [listed, worktrees]);
  const reportSessions = useEffectEvent((records: AgentSessionRecord[]) => onSessions?.(records));
  useEffect(() => {
    if (listed) reportSessions(sessions);
  }, [listed, sessions]);

  useEffect(
    () => bridge.onSetup(({ worktree, setup }) => setSetups((known) => ({ ...known, [worktree]: setup }))),
    [bridge],
  );

  // The number of the latest lists asked for. Older ones that answer late are dropped.
  const latestList = useRef(0);
  function refresh() {
    const asked = ++latestList.current;
    Promise.all([bridge.listWorktrees(), bridge.agentSessions()])
      .then(([list, records]) => {
        if (asked !== latestList.current) return;
        setWorktrees(list);
        setSessions(records);
        setListed(true);
      })
      .catch((e: unknown) => setError(String(e)));
  }

  // The core outlives the webview, so a reloaded window starts from what the core tracks.
  const listAtStart = useEffectEvent(refresh);
  useEffect(() => listAtStart(), [bridge]);

  // A subagent session can start in a worktree it adds, so the worktrees are listed again too.
  const onSessionsChanged = useEffectEvent(refresh);
  useEffect(() => bridge.onAgentSessionsChanged(onSessionsChanged), [bridge]);

  // A new state comes without a list, and only the list has the time it was entered. So a state the
  // list does not have yet lists the sessions again.
  const relistSessions = useEffectEvent(refresh);
  // Allow one reconciliation per event snapshot, even if the next list still disagrees.
  const relistedFor = useRef<typeof agentStates | null>(null);
  useEffect(() => {
    if (relistedFor.current === agentStates) return;
    const behind = sessions.some((record) => {
      const heard = agentStates[record.session];
      return heard !== undefined && (record.state === undefined || !sameState(heard, record));
    });
    if (behind) {
      relistedFor.current = agentStates;
      relistSessions();
    }
  }, [agentStates, sessions]);

  const now = useNow();

  // An ended session resumes first: until then the core cannot give its conversation.
  async function selectSession(record: AgentSessionRecord) {
    if (record.ended) {
      try {
        await bridge.resumeAgentSession(record.session);
        setError(null);
        onSessionResumed(record.session);
      } catch (e) {
        setError(String(e));
        return;
      }
    }
    const { session, worktree, agent } = record;
    onFocusAgent({ session, worktree, agent, panel: session });
  }

  function sessionDeleted(deleted: SessionId[]) {
    setDialog(null);
    refresh();
    onSessionsDeleted(deleted);
  }

  // `repo` opens, so the new worktree's selected row shows.
  function created(repo: string, worktree: Worktree, startAgent: boolean) {
    setDialog(null);
    setFoldedRepos((folded) => folded.filter((path) => path !== repo));
    refresh();
    onSelect(worktree.path);
    if (startAgent) onStartAgentAfterSetup(worktree.path);
  }

  function removed(worktree: Worktree, outcome?: BranchOutcome) {
    // A dialog about the worktree or one of its sessions closes, since it is gone. One the user
    // opened since stays.
    setDialog((open) => (open && dialogWorktree(open) === worktree.path ? null : open));
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

  function toggleRepo(path: string) {
    setFoldedRepos((folded) => toggled(folded, path));
  }

  const repos = byRepo(worktrees);
  // New session starts in the selected worktree's project, or else in the first project.
  const newSessionProject = repos.find((repo) => repo.some((w) => w.path === selected)) ?? repos[0];
  const newSessionMenu = useRef<MenuHandle>(null);

  // New session's places in `project`. New worktree… makes one there.
  const placesIn = (project: Worktree[]) =>
    places(project, startIn, () => setDialog({ kind: "new", repo: project[0] }));

  // A worktree whose setup script runs gets its session once the setup succeeds, as `spawn_agent`
  // does not wait for it.
  function startIn(worktree: Worktree) {
    onSelect(worktree.path);
    const setup = setups[worktree.path] ?? worktree.setup;
    if (setup?.state === "Running") onStartAgentAfterSetup(worktree.path);
    else onNewSession(worktree.path);
  }

  const sidebarSwitch = useRef<HTMLButtonElement>(null);
  useImperativeHandle(ref, () => ({
    addRepo: () => void addRepo(),
    focusSidebarSwitch: () => sidebarSwitch.current?.focus(),
    newSession() {
      if (!newSessionProject) return;
      // Shown first, since a hidden picker takes no focus.
      flushSync(() => onShowSidebar?.());
      newSessionMenu.current?.open();
    },
    focusSearch() {
      // Shown first, since a hidden box takes no focus.
      flushSync(() => onShowSidebar?.());
      searchBox.current?.focus();
    },
  }));

  // A session's row, then under it the subagent sessions it spawned that `shown` has.
  function sessionItem(record: AgentSessionRecord, shown: AgentSessionRecord[], byPath: Map<string, Worktree>) {
    const subagents = shown.filter((other) => other.parent === record.session);
    const listedState = record.state === undefined ? undefined : record;
    const heard = agentStates[record.session];
    // The time the state was entered shows only once the core lists that same state.
    const since = heard === undefined || (listedState !== undefined && sameState(heard, listedState));
    const worktree = byPath.get(record.worktree)!;
    return (
      <li key={record.session}>
        <SessionRow
          record={record}
          worktree={worktree}
          state={heard ?? listedState}
          age={since ? ageOf(now - record.sinceUnixMs) : ""}
          current={shownAgents.includes(record.session)}
          onClick={() => void selectSession(record)}
          onDelete={() => setDialog({ kind: "deleteSession", session: record })}
          onWorktreeAction={(kind) => setDialog({ kind, worktree })}
        />
        {subagents.length > 0 && (
          <ul className="agent-sessions">{subagents.map((subagent) => sessionItem(subagent, shown, byPath))}</ul>
        )}
      </li>
    );
  }

  // A worktree with no agent session.
  function worktreeItem(worktree: Worktree) {
    const setup = setups[worktree.path] ?? worktree.setup;
    return (
      <li key={worktree.path} title={worktree.path}>
        <WorktreeRow
          worktree={worktree}
          selected={worktree.path === selected}
          setupRunning={setup?.state === "Running"}
          onClick={() => onSelect(worktree.path)}
          onPick={(kind) => {
            if (kind !== "start") return setDialog({ kind, worktree });
            onSelect(worktree.path);
            onStartAgent(worktree.path);
          }}
        />
        {setup && <SetupStatus setup={setup} />}
      </li>
    );
  }

  const shownRepos = repos.flatMap((repo) => {
    const byPath = new Map(repo.map((worktree) => [worktree.path, worktree]));
    const own = sessions.filter((record) => byPath.has(record.worktree));
    const shownSessions = own.filter((record) => sessionMatches(record, byPath.get(record.worktree)!, query));
    const withNoSession = repo.filter(
      (worktree) => !own.some((record) => record.worktree === worktree.path) && matches(worktree, query),
    );
    return shownSessions.length + withNoSession.length > 0
      ? [{ repo, byPath, sessions: shownSessions, withNoSession }]
      : [];
  });
  return (
    <aside className="sidebar" aria-label="Worktrees">
      {/* The window's top row: room for the window controls, then the sidebar switch. Its empty
          part drags the window, as a title bar does. */}
      <div className="sidebar-top" data-tauri-drag-region="deep">
        <IconButton
          ref={sidebarSwitch}
          icon={PanelLeft}
          label="Sidebar"
          aria-pressed="true"
          onClick={onHideSidebar}
        />
      </div>
      <nav className="sidebar-nav" aria-label="Sidebar">
        <Menu
          ref={newSessionMenu}
          label="New session"
          icon={Plus}
          heading="Start in"
          items={newSessionProject ? placesIn(newSessionProject) : []}
          trigger={
            <>
              <Plus aria-hidden="true" />
              New session
              <kbd aria-hidden="true">{keyHint(shortcut("newSession").keys)}</kbd>
            </>
          }
          triggerProps={{ className: "nav-row", "aria-keyshortcuts": shortcut("newSession").keys, disabled: !newSessionProject }}
        />
        <label className="nav-row search-row">
          <Search aria-hidden="true" />
          <input
            ref={searchBox}
            type="search"
            aria-label="Search"
            aria-keyshortcuts={shortcut("search").keys}
            placeholder="Search"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Escape") setQuery("");
            }}
          />
          <kbd aria-hidden="true">{keyHint(shortcut("search").keys)}</kbd>
        </label>
        <button className="nav-row" aria-keyshortcuts={shortcut("jump").keys} onClick={onJump}>
          <Bell aria-hidden="true" />
          Needs you{" "}
          <span className={waiting > 0 ? "count waiting" : "count"}>{waiting}</span>
          <kbd aria-hidden="true">{keyHint(shortcut("jump").keys)}</kbd>
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
        {shownRepos.map(({ repo, byPath, sessions: shownSessions, withNoSession }) => {
          const head = repo[0];
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
                <Menu
                  label={`New session in ${name}`}
                  icon={Plus}
                  heading="Start in"
                  align="end"
                  items={placesIn(repo)}
                />
              </div>
              {open && (
                <ul>
                  {/* A subagent session shows under its parent, unless Search hides the parent. */}
                  {shownSessions
                    .filter((record) => !shownSessions.some((parent) => parent.session === record.parent))
                    .map((record) => sessionItem(record, shownSessions, byPath))}
                  {withNoSession.map(worktreeItem)}
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
          sessionsInWorktree={sessionsIn(sessions, dialog.worktree)}
          onForgotten={() => removed(dialog.worktree)}
          onCancel={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "deleteSession" && (
        <DeleteSessionDialog
          bridge={bridge}
          record={dialog.session}
          deleting={withSubagents(sessions, dialog.session.session)}
          onDeleted={sessionDeleted}
          onCancel={() => setDialog(null)}
        />
      )}
      {dialog?.kind === "delete" && (
        <DeleteDialog
          bridge={bridge}
          worktree={dialog.worktree}
          sessionsInWorktree={sessionsIn(sessions, dialog.worktree)}
          onDeleting={(running) => onDeleting(dialog.worktree, running)}
          onDeleted={(outcome) => removed(dialog.worktree, outcome)}
          onCancel={() => setDialog(null)}
        />
      )}
    </aside>
  );
}

/**
 * The places New session offers in `project`: the local checkout on its current branch, each
 * worktree, then New worktree…, which opens the new worktree dialog. A place may already have
 * sessions. #64 and #65 add their places here.
 */
function places(project: Worktree[], startIn: (worktree: Worktree) => void, newWorktree: () => void): MenuItem[] {
  const worktreePlaces = project.map((worktree) => {
    const label = worktree.isMain ? "Local checkout" : worktree.displayName;
    // The branch shows under the name, unless the name already is the branch.
    const hint = worktree.branch !== null && worktree.branch !== label ? worktree.branch : undefined;
    return { key: worktree.path, label, hint, onSelect: () => startIn(worktree) };
  });
  return [...worktreePlaces, { key: "new-worktree", label: "New worktree…", onSelect: newWorktree }];
}

const isPathList = (value: unknown): value is string[] =>
  Array.isArray(value) && value.every((path) => typeof path === "string");

/** The worktree the dialog is about, by path. null for a new worktree's. */
function dialogWorktree(dialog: OpenDialog): string | null {
  if (dialog.kind === "new") return null;
  return dialog.kind === "deleteSession" ? dialog.session.worktree : dialog.worktree.path;
}

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

/** Whether Search's `query` finds the session, by its title or its worktree, ignoring case. */
function sessionMatches(record: AgentSessionRecord, worktree: Worktree, query: string): boolean {
  return matches(worktree, query) || record.title.toLowerCase().includes(query.trim().toLowerCase());
}

/** How many agent sessions the core lists in `worktree`, ended ones too. They go when it does. */
function sessionsIn(records: AgentSessionRecord[], worktree: Worktree): number {
  return records.filter((record) => record.worktree === worktree.path).length;
}

/** `session`, then every subagent session below it, as the core deletes them. */
function withSubagents(records: AgentSessionRecord[], session: SessionId): SessionId[] {
  const found = [session];
  for (let at = 0; at < found.length; at++) {
    for (const record of records) {
      if (record.parent === found[at] && !found.includes(record.session)) found.push(record.session);
    }
  }
  return found;
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

/**
 * An agent session's row, one line: its state's mark, its title, the worktree badge when it runs in a
 * worktree of its own, and its age. Its menu takes the age's place on hover or focus. An ended one is
 * dimmed and has no mark. Its name and its tooltip say the state in words, and the tooltip names the
 * branch. Its menu deletes the session and, in a worktree of its own, has that worktree's actions
 * in a group named for the worktree. The local checkout's are not offered here.
 */
function SessionRow({
  record,
  worktree,
  state,
  age,
  current,
  onClick,
  onDelete,
  onWorktreeAction,
}: {
  record: AgentSessionRecord;
  worktree: Worktree;
  state: State | undefined;
  /** How long it has been in its state, as "6m", or "" when that is not known yet. */
  age: string;
  /** Its panel shows. */
  current: boolean;
  onClick(): void;
  onDelete(): void;
  /** An action on the worktree it runs in was picked. */
  onWorktreeAction(action: WorktreeDialogAction): void;
}) {
  const word = state && stateWord(state);
  const classes = ["session-item", current && "selected", record.ended && "dimmed"].filter(Boolean).join(" ");
  const mark = record.ended || !state ? null : MARKS[state.state];
  const where = [word, worktree.branch ?? worktree.displayName].filter(Boolean).join(" · ");
  return (
    <div className={classes}>
      <button
        className="session-row"
        aria-current={current}
        aria-label={word ? `${record.title}, ${word}` : record.title}
        title={`${record.title}\n${where}`}
        onClick={onClick}
      >
        <span aria-hidden="true" className={mark ? `session-mark mark-${mark}` : "session-mark"}>
          {mark === "spinner" && <Loader />}
        </span>
        <span className="session-title">{record.title}</span>
        {!worktree.isMain && (
          <span role="img" aria-label="Worktree" title="Runs in a worktree of its own" className="session-badge">
            <FolderGit2 aria-hidden="true" />
          </span>
        )}
        <span className="session-age">{age}</span>
      </button>
      <Menu
        label={`Actions for ${record.title}`}
        icon={Ellipsis}
        align="end"
        items={[
          { key: "deleteSession", label: "Delete session", onSelect: onDelete },
          ...(worktree.isMain
            ? []
            : SESSION_ROW_WORKTREE_ACTIONS.map(([action, label]) => ({
                key: action,
                label,
                group: `Worktree ${worktree.displayName}`,
                onSelect: () => onWorktreeAction(action),
              }))),
        ]}
      />
    </div>
  );
}

/** A session row's actions on its worktree. Each says "worktree", since the row is a session's. */
const SESSION_ROW_WORKTREE_ACTIONS: [WorktreeDialogAction, string][] = [
  ["base", "Change base branch"],
  ["forget", "Forget worktree"],
  ["delete", "Delete worktree"],
];

/** The mark each state shows before a session's title. Idle and Exited show none. */
const MARKS: Record<State["state"], "spinner" | "accent" | "red" | null> = {
  Working: "spinner",
  NeedsInput: "accent",
  Done: "accent",
  Failed: "red",
  Idle: null,
  Exited: null,
};

/** How often the ages on the rows move on. */
const AGE_TICK_MS = 60_000;

/** The time now, moved on every `AGE_TICK_MS`. */
function useNow(): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), AGE_TICK_MS);
    return () => clearInterval(timer);
  }, []);
  return now;
}

/** A time span as "now" under a minute, then whole minutes, hours or days: "6m", "2h", "3d". */
function ageOf(spanMs: number): string {
  const minutes = Math.floor(spanMs / 60_000);
  if (minutes < 1) return "now";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return hours < 24 ? `${hours}h` : `${Math.floor(hours / 24)}d`;
}

/**
 * Deletes an agent session and the subagent sessions below it, for good, once the user confirms.
 * `deleting` is the session, then its subagent sessions.
 */
function DeleteSessionDialog({
  bridge,
  record,
  deleting,
  onDeleted,
  onCancel,
}: {
  bridge: Bridge;
  record: AgentSessionRecord;
  deleting: SessionId[];
  onDeleted(deleted: SessionId[]): void;
  onCancel(): void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function remove() {
    setBusy(true);
    try {
      await bridge.deleteAgentSession(record.session);
      onDeleted(deleting);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const subagents = deleting.length - 1;
  const alsoDeleted =
    subagents === 0
      ? ""
      : subagents === 1
        ? " Its subagent session is deleted too."
        : ` Its ${subagents} subagent sessions are deleted too.`;
  return (
    <Dialog label="Delete session" onSubmit={remove} onCancel={onCancel}>
      <div className="form">
        <p>{`grove ends “${record.title}” and deletes its conversation for good.${alsoDeleted}`}</p>
        {error && <p role="alert" className="error">{error}</p>}
      </div>
      <Actions submit="Delete session" busy={busy} onCancel={onCancel} />
    </Dialog>
  );
}

/**
 * A worktree with no agent session, dimmed: its name, cut with an ellipsis, then on the right its
 * menu and, while its setup script runs, the Setup running dot, which describes the row's button.
 */
function WorktreeRow({
  worktree,
  selected,
  setupRunning,
  onClick,
  onPick,
}: {
  worktree: Worktree;
  selected: boolean;
  /** Shows "Setup running". */
  setupRunning: boolean;
  onClick(): void;
  onPick(action: WorktreeAction): void;
}) {
  const dot = useId();
  return (
    <div className={selected ? "wt-row dimmed selected" : "wt-row dimmed"}>
      <button
        className="wt-name"
        aria-current={selected}
        aria-describedby={setupRunning ? dot : undefined}
        onClick={onClick}
      >
        <span className="wt-title">
          {worktree.displayName}
          {worktree.isMain && " (main worktree)"}
        </span>
      </button>
      <WorktreeMenu worktree={worktree} setupRunning={setupRunning} onPick={onPick} />
      {setupRunning && <SetupDot id={dot} />}
    </div>
  );
}

/** A worktree row's 7px Working dot while its setup script runs, named "Setup running". */
function SetupDot({ id }: { id: string }) {
  return <span id={id} role="img" aria-label="Setup running" title="Setup running" className="status-dot status-working" />;
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
