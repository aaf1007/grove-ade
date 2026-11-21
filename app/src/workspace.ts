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

/** Whose workspace: an agent session's own, or the one a new agent session in a worktree starts from. */
export type WorkspaceOwner = { session: SessionId } | { worktree: string };

/** The key of `owner`'s workspace in what `useWorkspaces` holds. */
export function workspaceKey(owner: WorkspaceOwner): string {
  return "session" in owner ? `session ${owner.session}` : `worktree ${owner.worktree}`;
}

/**
 * Each workspace the window showed, by `workspaceKey`: what the core last gave, with every `change`
 * not saved yet on top. An agent session's workspace is saved with each change, and loaded again
 * when the core's copy changes, as when a subagent session starts. Each has one queue for its saves
 * and its loads, so none of them saves over another. A save sends what the window then shows.
 *
 * A worktree's starting workspace is what a place with no agent session selected shows. Only a
 * session's workspace is saved, so a change to it stays in the window. `errors` says, by
 * `workspaceKey`, why a workspace's last load or save failed.
 */
export function useWorkspaces(bridge: Bridge) {
  const [workspaces, setWorkspaces] = useState<Record<string, Workspace>>({});
  const [errors, setErrors] = useState<Record<string, string>>({});
  // Ahead of `workspaces` until React renders, so changes made in one event build on each other.
  const latest = useRef(workspaces);
  const loads = useRef(new Map<string, Promise<Workspace>>());
  // How many times each workspace was forgotten. A load that started before the latest is stale.
  const forgets = useRef(new Map<string, number>());
  // Each workspace's queue.
  const queues = useRef(new Map<string, Promise<unknown>>());
  // The changes not saved yet, in order.
  const unsaved = useRef<{ key: string; update: (workspace: Workspace) => Workspace }[]>([]);

  function setError(key: string, error: string | null) {
    setErrors((known) => {
      const rest = { ...known };
      if (error === null) delete rest[key];
      else rest[key] = error;
      return rest;
    });
  }

  function put(key: string, workspace: Workspace) {
    latest.current = { ...latest.current, [key]: workspace };
    setWorkspaces(latest.current);
  }

  // Runs `step` once the workspace's earlier steps are done, failed or not.
  function enqueue<T>(key: string, step: () => Promise<T>): Promise<T> {
    const done = (queues.current.get(key) ?? Promise.resolve()).then(step);
    queues.current.set(
      key,
      done.catch(() => {}),
    );
    return done;
  }

  // Whether the workspace was forgotten since `forgotten` was read.
  function forgottenSince(key: string, forgotten: number) {
    return (forgets.current.get(key) ?? 0) !== forgotten;
  }

  const fromCore = (owner: WorkspaceOwner) =>
    "session" in owner ? bridge.workspace(owner.session) : bridge.startingWorkspace(owner.worktree);

  function load(owner: WorkspaceOwner): Promise<Workspace> {
    const key = workspaceKey(owner);
    let loading = loads.current.get(key);
    if (!loading) {
      const forgotten = forgets.current.get(key) ?? 0;
      const stale = () => forgottenSince(key, forgotten);
      loading = fromCore(owner).then(
        (workspace) => {
          if (stale()) throw `grove dropped this layout while it loaded`;
          setError(key, null);
          if (!latest.current[key]) put(key, workspace);
          return latest.current[key];
        },
        (e: unknown) => {
          if (stale()) throw e;
          loads.current.delete(key);
          setError(key, `Could not load this layout: ${String(e)}`);
          throw e;
        },
      );
      loads.current.set(key, loading);
    }
    return loading;
  }

  async function change(owner: WorkspaceOwner, update: (workspace: Workspace) => Workspace) {
    const key = workspaceKey(owner);
    if (!latest.current[key]) {
      try {
        await load(owner);
      } catch {
        // `load` has said why.
        return;
      }
    }
    const next = update(latest.current[key]);
    // A change that changes nothing saves nothing.
    if (JSON.stringify(next) === JSON.stringify(latest.current[key])) return;
    if (!("session" in owner)) return put(key, next);
    const unsavedChange = { key, update };
    unsaved.current.push(unsavedChange);
    put(key, next);
    await enqueue(key, () => {
      unsaved.current = unsaved.current.filter((other) => other !== unsavedChange);
      return bridge.setWorkspace(owner.session, latest.current[key]);
    }).then(
      () => setError(key, null),
      (e: unknown) => setError(key, `Could not save this layout: ${String(e)}`),
    );
  }

  // Loads the core's copy of the session's workspace again, and puts the unsaved changes on top.
  // A workspace not loaded yet loads when it is first shown.
  function reload(session: SessionId) {
    const key = workspaceKey({ session });
    if (!latest.current[key]) return;
    const forgotten = forgets.current.get(key) ?? 0;
    enqueue(key, async () => {
      const core = await bridge.workspace(session);
      if (forgottenSince(key, forgotten)) return;
      const mine = unsaved.current.filter((other) => other.key === key);
      put(
        key,
        mine.reduce((next, { update }) => update(next), core),
      );
    }).catch((e: unknown) => {
      if (!forgottenSince(key, forgotten)) setError(key, `Could not load this layout: ${String(e)}`);
    });
  }

  const onWorkspaceChanged = useEffectEvent(reload);
  useEffect(() => bridge.onWorkspaceChanged(onWorkspaceChanged), [bridge]);

  // Drops a workspace, so one that gets its key again loads fresh.
  function forget(owner: WorkspaceOwner) {
    const key = workspaceKey(owner);
    const rest = { ...latest.current };
    delete rest[key];
    latest.current = rest;
    setWorkspaces(rest);
    unsaved.current = unsaved.current.filter((other) => other.key !== key);
    loads.current.delete(key);
    forgets.current.set(key, (forgets.current.get(key) ?? 0) + 1);
    setError(key, null);
  }

  /**
   * Drops what the window holds of agent sessions the core no longer lists: their workspaces, and
   * their panels in other workspaces. The core dropped those panels too, so nothing is saved.
   */
  function dropSessions(gone: SessionId[]) {
    for (const session of gone) forget({ session });
    const kept = Object.entries(latest.current).map(([key, workspace]) => {
      const chat = workspace.chat.filter((panel) => !gone.includes(panel.session));
      return [key, chat.length === workspace.chat.length ? workspace : { ...workspace, chat }] as const;
    });
    latest.current = Object.fromEntries(kept);
    setWorkspaces(latest.current);
  }

  /** The workspace as the window shows it now, with the changes React has not rendered yet. */
  const current = (owner: WorkspaceOwner): Workspace | undefined => latest.current[workspaceKey(owner)];

  return { workspaces, errors, load, change, forget, dropSessions, current };
}

/** Shows the session's panel. A session with no panel gets one after the last. */
export function showSession(workspace: Workspace, session: SessionId, agent: Agent): Workspace {
  const chat = [...workspace.chat];
  const at = chat.findIndex((panel) => panel.session === session);
  chat[at < 0 ? chat.length : at] = { agent, session, shown: true };
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
