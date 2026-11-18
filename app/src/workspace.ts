import { useEffect, useEffectEvent, useRef, useState } from "react";

import type { Agent, Bridge, PanelKind, SessionId, Workspace } from "./bridge";

/** What the + menu opens: a tab, or the terminal at the bottom. */
export type MenuPanel = PanelKind | "Terminal";

/** The + menu's entries, in order, each with the letter that opens it while the menu is open. */
export const MENU_PANELS: { panel: MenuPanel; letter: string }[] = [
  { panel: "Files", letter: "F" },
  { panel: "Diff", letter: "D" },
  { panel: "Terminal", letter: "T" },
  { panel: "Browser", letter: "B" },
  { panel: "Logs", letter: "L" },
  { panel: "Profile", letter: "P" },
];

/** Split stops offering another agent panel once this many are shown. */
export const MAX_SHOWN_AGENT_PANELS = 3;

/**
 * Each worktree's workspace, by worktree path: what the core last gave, with every `change` not
 * saved yet on top. Each worktree has one queue for its saves, its loads of the core's copy and
 * its agent session starts, so none of them saves over another. A save sends what the window then
 * shows. When the core's copy changes, as when it places a new agent session, it is loaded again.
 * The panel of a session that ended stays, though the core leaves it out, until the window
 * reloads. A running session's panel that the core leaves out goes. `errors` says, by worktree,
 * why its last load or save failed.
 */
export function useWorkspaces(bridge: Bridge) {
  const [workspaces, setWorkspaces] = useState<Record<string, Workspace>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  // Ahead of `workspaces` until React renders, so changes made in one event build on each other.
  const latest = useRef(workspaces);
  const loads = useRef(new Map<string, Promise<Workspace>>());
  // How many times each worktree was forgotten. A load that started before the latest is stale.
  const forgets = useRef(new Map<string, number>());
  // Each worktree's queue, by worktree path.
  const queues = useRef(new Map<string, Promise<unknown>>());
  // The changes not saved yet, in order.
  const unsaved = useRef<{ worktree: string; update: (workspace: Workspace) => Workspace }[]>([]);

  function setError(worktree: string, error: string | null) {
    setErrors((known) => {
      const rest = { ...known };
      if (error === null) delete rest[worktree];
      else rest[worktree] = error;
      return rest;
    });
  }

  function put(worktree: string, workspace: Workspace) {
    latest.current = { ...latest.current, [worktree]: workspace };
    setWorkspaces(latest.current);
  }

  // Runs `step` once the worktree's earlier steps are done, failed or not.
  function enqueue<T>(worktree: string, step: () => Promise<T>): Promise<T> {
    const done = (queues.current.get(worktree) ?? Promise.resolve()).then(step);
    queues.current.set(
      worktree,
      done.catch(() => {}),
    );
    return done;
  }

  // Whether the worktree was forgotten since `forgotten` was read.
  function forgottenSince(worktree: string, forgotten: number) {
    return (forgets.current.get(worktree) ?? 0) !== forgotten;
  }

  // Loads the core's copy and puts it, with the panels the window shows of sessions that no longer
  // run put back where they were, and the unsaved changes on top. Nothing is put once the worktree
  // was forgotten since `forgotten`.
  async function loadFromCore(worktree: string, forgotten: number) {
    const core = await bridge.workspace(worktree);
    const listed = new Set(core.chat.map((panel) => panel.session));
    const leftOut = (panel: Workspace["chat"][number]) => panel.session !== null && !listed.has(panel.session);
    // Asked only when the core leaves out a panel the window shows.
    const running = latest.current[worktree]?.chat.some(leftOut)
      ? new Set((await bridge.sessions(worktree)).filter((info) => info.running).map((info) => info.session))
      : new Set<SessionId>();
    if (forgottenSince(worktree, forgotten)) return;
    const chat = [...core.chat];
    latest.current[worktree]?.chat.forEach((panel, at) => {
      if (leftOut(panel) && !running.has(panel.session!)) chat.splice(Math.min(at, chat.length), 0, panel);
    });
    const mine = unsaved.current.filter((other) => other.worktree === worktree);
    put(
      worktree,
      mine.reduce((next, { update }) => update(next), { ...core, chat }),
    );
  }

  function load(worktree: string): Promise<Workspace> {
    let loading = loads.current.get(worktree);
    if (!loading) {
      const forgotten = forgets.current.get(worktree) ?? 0;
      const stale = () => forgottenSince(worktree, forgotten);
      loading = bridge.workspace(worktree).then(
        (workspace) => {
          if (stale()) throw `grove stopped tracking ${worktree} while its layout loaded`;
          setError(worktree, null);
          if (!latest.current[worktree]) put(worktree, workspace);
          return latest.current[worktree];
        },
        (e: unknown) => {
          if (stale()) throw e;
          loads.current.delete(worktree);
          setError(worktree, `Could not load this worktree's layout: ${String(e)}`);
          throw e;
        },
      );
      loads.current.set(worktree, loading);
    }
    return loading;
  }

  async function change(worktree: string, update: (workspace: Workspace) => Workspace) {
    if (!latest.current[worktree]) {
      try {
        await load(worktree);
      } catch {
        // `load` has said why.
        return;
      }
    }
    const unsavedChange = { worktree, update };
    unsaved.current.push(unsavedChange);
    put(worktree, update(latest.current[worktree]));
    await enqueue(worktree, () => {
      unsaved.current = unsaved.current.filter((other) => other !== unsavedChange);
      return bridge.setWorkspace(worktree, latest.current[worktree]);
    }).then(
      () => setError(worktree, null),
      (e: unknown) => setError(worktree, `Could not save this worktree's layout: ${String(e)}`),
    );
  }

  // A worktree not loaded yet loads when it is first shown.
  function reload(worktree: string) {
    if (!latest.current[worktree]) return;
    const forgotten = forgets.current.get(worktree) ?? 0;
    enqueue(worktree, () => loadFromCore(worktree, forgotten)).catch((e: unknown) =>
      setError(worktree, `Could not load this worktree's layout: ${String(e)}`),
    );
  }

  /**
   * Starts an agent session of `agent` with `spawn`, in the agent panel `at` picks from the
   * workspace the window shows now (null for a new panel after the last one). The core places it.
   * It starts once the worktree's earlier changes are saved, and later ones wait for it, then go on
   * top of the core's copy, which is loaded as soon as the session starts.
   *
   * A start after setup may wait minutes, so it holds back no save. A change saved meanwhile may
   * leave out the session's panel, which then goes back where it was asked for. A session that
   * ends before the core's copy is loaded keeps its panel too, until the window reloads.
   */
  async function startAgent(
    worktree: string,
    agent: Agent,
    at: (workspace: Workspace) => number | null,
    spawn: (panel: number | null) => Promise<SessionId>,
    { afterSetup = false } = {},
  ): Promise<SessionId> {
    const shown = latest.current[worktree] ?? (await load(worktree));
    const panel = at(shown);
    const forgotten = forgets.current.get(worktree) ?? 0;
    // The core counts only the panels it lists, which leave out those of ended sessions.
    const panelForCore = async () => {
      if (panel === null) return null;
      const listed = new Set((await bridge.workspace(worktree)).chat.map((p) => p.session));
      const unlisted = shown.chat.slice(0, panel).filter((p) => p.session !== null && !listed.has(p.session));
      return panel - unlisted.length;
    };
    // Loads the core's copy, then puts the session back where it was asked for if that copy left
    // it out. Says whether it did.
    const loadKeeping = async (session: SessionId) => {
      await loadFromCore(worktree, forgotten);
      const now = latest.current[worktree];
      if (forgottenSince(worktree, forgotten) || !now || now.chat.some((p) => p.session === session)) return false;
      const chat = [...now.chat];
      const placed = { agent, session, shown: true };
      if (panel !== null && chat[panel]?.session === null) chat[panel] = placed;
      else chat.push(placed);
      put(worktree, { ...now, chat });
      return true;
    };
    // Once the session runs, a failed load or save is the layout's error, not the start's.
    const layoutFailed = (failed: "load" | "save") => (e: unknown) => {
      if (!forgottenSince(worktree, forgotten)) setError(worktree, `Could not ${failed} this worktree's layout: ${String(e)}`);
    };
    if (!afterSetup) {
      return enqueue(worktree, async () => {
        const session = await spawn(await panelForCore());
        // Left out only when it already ended, and the core leaves its panel out anyway.
        await loadKeeping(session).catch(layoutFailed("load"));
        return session;
      });
    }
    const session = await spawn(await enqueue(worktree, panelForCore));
    await enqueue(worktree, async () => {
      const kept = await loadKeeping(session).catch(layoutFailed("load"));
      if (kept) await bridge.setWorkspace(worktree, latest.current[worktree]).catch(layoutFailed("save"));
    });
    return session;
  }

  const onWorkspaceChanged = useEffectEvent(reload);
  useEffect(() => bridge.onWorkspaceChanged(onWorkspaceChanged), [bridge]);

  // Drops a forgotten or deleted worktree's workspace, so one made again at its path loads fresh.
  function forget(worktree: string) {
    const rest = { ...latest.current };
    delete rest[worktree];
    latest.current = rest;
    setWorkspaces(rest);
    unsaved.current = unsaved.current.filter((other) => other.worktree !== worktree);
    loads.current.delete(worktree);
    forgets.current.set(worktree, (forgets.current.get(worktree) ?? 0) + 1);
    setError(worktree, null);
  }

  return { workspaces, errors, load, change, forget, startAgent };
}

/**
 * Shows the session's panel. A session with no panel takes the first shown panel not started yet,
 * or else a new one at the end.
 */
export function showSession(workspace: Workspace, session: SessionId, agent: Agent): Workspace {
  const chat = [...workspace.chat];
  let at = chat.findIndex((panel) => panel.session === session);
  if (at < 0) at = chat.findIndex((panel) => panel.shown && panel.session === null);
  if (at < 0) at = chat.length;
  chat[at] = { agent, session, shown: true };
  return { ...workspace, chat };
}

export function hideSession(workspace: Workspace, session: SessionId): Workspace {
  return {
    ...workspace,
    chat: workspace.chat.map((panel) => (panel.session === session ? { ...panel, shown: false } : panel)),
  };
}

/** Selects the tab, opening it first if it is not open, or opens the terminal. */
export function openPanel(workspace: Workspace, panel: MenuPanel): Workspace {
  if (panel === "Terminal") return { ...workspace, terminal: true };
  const panels = workspace.panels.includes(panel) ? workspace.panels : [...workspace.panels, panel];
  return { ...workspace, panels, selected: panel };
}

/** Closes the tab. Closing the selected one selects the last tab left. */
export function closeTab(workspace: Workspace, tab: PanelKind): Workspace {
  const panels = workspace.panels.filter((open) => open !== tab);
  const selected = workspace.selected === tab ? (panels.at(-1) ?? null) : workspace.selected;
  return { ...workspace, panels, selected };
}
