import { Ellipsis, Plus } from "lucide-react";
import { useEffect, useId, useState, type FormEvent } from "react";

import type {
  Agent,
  AgentState as State,
  AgentStateChanged,
  BranchOutcome,
  Bridge,
  DiffBase,
  SessionId,
  SetupState,
  Worktree,
} from "./bridge";
import { AgentState, stateWord } from "./ui/AgentState";
import { Button } from "./ui/Button";
import { IconButton } from "./ui/IconButton";
import { Menu } from "./ui/Menu";
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

/** What a worktree's menu does. */
type WorktreeAction = "base" | "forget" | "delete";

/** The dialog the sidebar shows: a new worktree for a repo, or a worktree's menu action. */
type OpenDialog = { kind: "new"; repo: Worktree } | { kind: WorktreeAction; worktree: Worktree };

/**
 * Lists every tracked worktree under its repo with its agent sessions, adds repos, selects a
 * worktree, and starts an agent session in the selected one. Each repo's + adds a worktree, and
 * each worktree's menu changes its base branch, forgets it or deletes it.
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
  onDeleting,
  onRemoved,
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
  /** A delete of the worktree started or ended. */
  onDeleting(worktree: Worktree, running: boolean): void;
  /** A worktree was forgotten or deleted, which ended its sessions. */
  onRemoved(path: string): void;
}) {
  const [path, setPath] = useState("");
  const [worktrees, setWorktrees] = useState<Worktree[]>([]);
  // Each setup state the core sent since the sidebar opened. It is newer than any list.
  const [setups, setSetups] = useState<Record<string, SetupState>>({});
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState<OpenDialog | null>(null);

  useEffect(
    () => bridge.onSetup(({ worktree, setup }) => setSetups((known) => ({ ...known, [worktree]: setup }))),
    [bridge],
  );

  // The core outlives the webview, so a reloaded window starts from what the core tracks.
  useEffect(() => {
    bridge
      .listWorktrees()
      .then(setWorktrees)
      .catch((e: unknown) => setError(String(e)));
  }, [bridge]);

  function refresh() {
    bridge
      .listWorktrees()
      .then(setWorktrees)
      .catch((e: unknown) => setError(String(e)));
  }

  function created(worktree: Worktree, startAgent: boolean) {
    setDialog(null);
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

  async function addRepo(event: FormEvent) {
    event.preventDefault();
    try {
      await bridge.addRepo(path);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
    setWorktrees(await bridge.listWorktrees());
  }

  const names = sessionNames(agents);
  const lineCounts = useLineCounts(bridge, worktrees);
  return (
    <aside className="sidebar" aria-label="Worktrees">
      <form onSubmit={addRepo}>
        <input
          aria-label="Repo path"
          placeholder="/path/to/repo"
          value={path}
          onChange={(event) => setPath(event.target.value)}
        />
        <Button type="submit" size="sm">
          Add repo
        </Button>
      </form>
      {error && <p role="alert">{error}</p>}
      {byRepo(worktrees).map((repo) => (
        <section key={repo[0].path} aria-label={folderName(repo[0].path)}>
          <div className="repo-head">
            <h2>{folderName(repo[0].path)}</h2>
            <IconButton
              icon={Plus}
              size="xs"
              label={`New worktree in ${folderName(repo[0].path)}`}
              title="New worktree"
              onClick={() => setDialog({ kind: "new", repo: repo[0] })}
            />
          </div>
          <ul>
            {repo.map((worktree) => {
              const own = agents.filter((agent) => agent.worktree === worktree.path);
              const setup = setups[worktree.path] ?? worktree.setup;
              const top = mostUrgent(own.map((agent) => agentStates[agent.session]));
              return (
                <li key={worktree.path} title={worktree.path}>
                  <div className="wt-row">
                    <WorktreeRow
                      worktree={worktree}
                      selected={worktree.path === selected}
                      state={top}
                      setupRunning={setup?.state === "Running"}
                      counts={lineCounts[worktree.path]}
                      onClick={() => (own.length === 1 ? onShowAgent(own[0]) : onSelect(worktree.path))}
                    />
                    <WorktreeMenu worktree={worktree} onPick={(kind) => setDialog({ kind, worktree })} />
                  </div>
                  {setup && <SetupStatus setup={setup} />}
                  {own.length > 1 && (
                    <ul className="agent-sessions">
                      {own.map((agent) => (
                        <li key={agent.session}>
                          <button
                            className="session-row"
                            aria-current={shownAgents.includes(agent.session)}
                            onClick={() => onShowAgent(agent)}
                          >
                            <span className="session-name">{names[agent.session]}</span>{" "}
                            {agentStates[agent.session] && <AgentState state={agentStates[agent.session]} />}
                          </button>
                        </li>
                      ))}
                    </ul>
                  )}
                  {/* `spawn_agent` does not wait for setup, so the button waits instead. */}
                  {worktree.path === selected && setup?.state !== "Running" && (
                    <Button
                      className="start-agent"
                      variant="ghost"
                      size="xs"
                      icon={Plus}
                      onClick={() => onStartAgent(worktree.path)}
                    >
                      Start Claude Code
                    </Button>
                  )}
                </li>
              );
            })}
          </ul>
        </section>
      ))}
      {dialog?.kind === "new" && (
        <NewWorktreeDialog
          bridge={bridge}
          repo={dialog.repo}
          onCreated={created}
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

/** The lines a worktree's diff adds and removes. */
interface LineCounts {
  added: number;
  removed: number;
}

/**
 * Each worktree's line counts, by path, from its whole-branch diff, or its Head diff when it has
 * no base branch. It asks for a worktree's counts again each time its diff may have changed.
 */
function useLineCounts(bridge: Bridge, worktrees: Worktree[]): Record<string, LineCounts> {
  const [counts, setCounts] = useState<Record<string, LineCounts>>({});
  // Asks again only when a worktree comes, goes or changes its base branch, not on every list.
  // The core sends no DiffChanged for a new base branch, so its name is part of the key.
  const bases = JSON.stringify(worktrees.map((w) => [w.path, w.baseBranch]));

  useEffect(() => {
    const baseOf = new Map(
      (JSON.parse(bases) as [string, string | null][]).map(([path, branch]): [string, DiffBase] => [
        path,
        branch ? "MergeBase" : "Head",
      ]),
    );
    // Each worktree's latest ask, so an older answer that arrives late gets dropped.
    const asks = new Map<string, number>();
    let live = true;
    function count(worktree: string) {
      const base = baseOf.get(worktree);
      if (!base) return;
      const ask = (asks.get(worktree) ?? 0) + 1;
      asks.set(worktree, ask);
      bridge.diff(worktree, base).then(
        (files) => {
          if (!live || asks.get(worktree) !== ask) return;
          const sum = (key: "added" | "removed") => files.reduce((total, file) => total + file[key], 0);
          setCounts((known) => ({ ...known, [worktree]: { added: sum("added"), removed: sum("removed") } }));
        },
        // A diff fails when the worktree's folder is gone. The row then shows no counts.
        () => {
          if (!live || asks.get(worktree) !== ask) return;
          setCounts((known) => {
            const rest = { ...known };
            delete rest[worktree];
            return rest;
          });
        },
      );
    }
    baseOf.forEach((_, worktree) => count(worktree));
    const stop = bridge.onDiffChanged(count);
    return () => {
      live = false;
      stop();
    };
  }, [bridge, bases]);

  return counts;
}

// The attention queue's order, most urgent first.
const URGENCY = ["Approval", "Question", "Failed", "Done", "Working", "Idle", "Exited"];

/** The most urgent of `states`, in attention queue order. undefined when none is known yet. */
function mostUrgent(states: (State | undefined)[]): State | undefined {
  const known = states.filter((state) => state !== undefined);
  return known.sort((a, b) => URGENCY.indexOf(stateWord(a)) - URGENCY.indexOf(stateWord(b)))[0];
}

/**
 * A worktree's row shows its name with its state chip, then its branch and line counts. The name
 * is the button's name, and the rest describes it.
 */
function WorktreeRow({
  worktree,
  selected,
  state,
  setupRunning,
  counts,
  onClick,
}: {
  worktree: Worktree;
  selected: boolean;
  state: State | undefined;
  /** Shows "Setup running" in place of the state. */
  setupRunning: boolean;
  counts: LineCounts | undefined;
  onClick(): void;
}) {
  const id = useId();
  return (
    <button
      className="wt-name"
      aria-current={selected}
      aria-labelledby={`${id}-title`}
      aria-describedby={`${id}-state ${id}-meta`}
      onClick={onClick}
    >
      <span className="wt-line">
        <span id={`${id}-title`} className="wt-title">
          {worktree.displayName}
          {worktree.isMain && " (main worktree)"}
        </span>
        {(setupRunning || state) && (
          <span id={`${id}-state`} className="wt-state">
            {setupRunning ? (
              <AgentState state={{ state: "Working" }} word="Setup running" />
            ) : (
              state && <AgentState state={state} />
            )}
          </span>
        )}
      </span>
      <span id={`${id}-meta`} className="wt-meta">
        <span className="wt-branch">{worktree.branch ?? "detached HEAD"}</span>
        {counts && (counts.added > 0 || counts.removed > 0) && (
          <>
            <span className="add">+{counts.added}</span>
            <span className="del">−{counts.removed}</span>
          </>
        )}
      </span>
    </button>
  );
}

/**
 * A failed setup script. A running one shows as the chip on the worktree's row, and one that
 * succeeded shows nothing.
 */
function SetupStatus({ setup }: { setup: SetupState }) {
  if (setup.state !== "Failed") return null;
  const ended = setup.code === null ? "was stopped by a signal" : `exited with code ${setup.code}`;
  return <p className="wt-setup state Failed">{`Setup failed: ${ended}`}</p>;
}

/** A worktree's menu. A main worktree is forgotten only with its repo, so its menu has neither. */
function WorktreeMenu({ worktree, onPick }: { worktree: Worktree; onPick(action: WorktreeAction): void }) {
  const items: [WorktreeAction, string][] = worktree.isMain
    ? [["base", "Change base branch"]]
    : [
        ["base", "Change base branch"],
        ["forget", "Forget"],
        ["delete", "Delete"],
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
