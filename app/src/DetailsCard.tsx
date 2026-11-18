import { Copy, FolderOpen, SlidersHorizontal } from "lucide-react";
import { type ReactNode, useEffect, useEffectEvent, useRef, useState } from "react";

import type { Agent, Bridge, Editor, FileDiff, Mode, RunSession, SessionId, StateSource, Worktree } from "./bridge";
import type { ModelChoice } from "./Picker";
import { useSetting } from "./settings";
import { AGENT_NAMES, byRepo } from "./Sidebar";
import { ChangeBaseDialog } from "./WorktreeDialogs";
import { Button } from "./ui/Button";
import { Card } from "./ui/Card";
import { IconButton } from "./ui/IconButton";
import { Menu } from "./ui/Menu";
import "./DetailsCard.css";

/** The card's id, which the button that opens it names in aria-controls. */
export const DETAILS_CARD_ID = "details-card";

// An open popup: a menu or picker, by the button that opened it, or a dialog.
const OTHER_POPUP = '[aria-haspopup][aria-expanded="true"], [role="dialog"]';

const ROWS = ["Worktree", "Open in", "Scripts", "Servers", "Branch", "Changes", "Session"] as const;
type Row = (typeof ROWS)[number];

const EDITORS: [Editor, string][] = [
  ["Zed", "Zed"],
  ["VsCode", "VS Code"],
];

/** The focused agent session, as the card shows it. */
export interface FocusedSession {
  agent: Agent;
  /** undefined until its panel knows it, and in terminal mode. null on the agent's own default. */
  model: ModelChoice | null | undefined;
  /** undefined until its panel knows it. */
  mode: Mode | undefined;
  /** Where its latest state came from. undefined before its first state. */
  source: StateSource | undefined;
}

/** A run session grove started or found in the worktree. */
export interface RunTab {
  name: string;
  session: SessionId;
  /** False once its process ended, or when a restart brought it back stopped. */
  running: boolean;
  /** Where it serves, from the repo's settings when it started. null after a restart. */
  port: number | null;
}

/**
 * Where the selected worktree is, what runs in it and what changed: its folder, the editors to
 * open it in, its run sessions and their ports, its branch and base, its changes since HEAD, and
 * the focused agent session. Each row can be hidden, and stays hidden. Escape or a click outside
 * closes it, except on the button that opens it, which closes it itself.
 */
export function DetailsCard({
  bridge,
  worktree,
  runs,
  session,
  onStartRun,
  onOpenDiff,
  onClose,
}: {
  bridge: Bridge;
  worktree: string;
  runs: RunTab[];
  /** null when the worktree has no agent session. */
  session: FocusedSession | null;
  onStartRun(name: string): void;
  onOpenDiff(): void;
  onClose(): void;
}) {
  const [hidden, setHidden] = useSetting<Row[]>("details-card-hidden-rows", [], isRowList);
  const [tracked, setTracked] = useState<{ worktree: Worktree; repo: string } | null>(null);
  // Bumped after the base branch changes, to list the worktree again.
  const [listings, setListings] = useState(0);
  const [runSessions, setRunSessions] = useState<RunSession[] | null>(null);
  // Bumped when a profile or the repo's settings change, to list its run sessions again.
  const [settingsChanges, setSettingsChanges] = useState(0);
  const [changes, setChanges] = useState<{ files: FileDiff[] } | { error: string } | null>(null);
  const [diffChanges, setDiffChanges] = useState(0);
  const [changingBase, setChangingBase] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const card = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let current = true;
    bridge.listWorktrees().then(
      (list) => {
        const repo = byRepo(list).find((worktrees) => worktrees.some((w) => w.path === worktree));
        const found = repo?.find((w) => w.path === worktree);
        if (current && repo && found) setTracked({ worktree: found, repo: repo[0].path });
      },
      (e: unknown) => current && setError(String(e)),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, listings]);

  const repo = tracked?.repo;
  useEffect(() => {
    const changed = () => setSettingsChanges((n) => n + 1);
    const stopProfiles = bridge.onProfilesChanged(changed);
    const stopRepo = bridge.onRepoSettingsChanged((changedRepo) => changedRepo === repo && changed());
    return () => {
      stopProfiles();
      stopRepo();
    };
  }, [bridge, repo]);

  useEffect(() => {
    if (repo === undefined) return;
    let current = true;
    bridge.runSessions(repo).then(
      (listed) => current && setRunSessions(listed),
      (e: unknown) => current && setError(String(e)),
    );
    return () => {
      current = false;
    };
  }, [bridge, repo, settingsChanges]);

  useEffect(
    () => bridge.onDiffChanged((changed) => changed === worktree && setDiffChanges((n) => n + 1)),
    [bridge, worktree],
  );

  useEffect(() => {
    let current = true;
    bridge.diff(worktree, "Head").then(
      (files) => current && setChanges({ files }),
      (e: unknown) => current && setChanges({ error: String(e) }),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, diffChanges]);

  const onPointerDown = useEffectEvent((event: PointerEvent) => {
    const target = event.target as Element;
    if (card.current?.contains(target) || target.closest(`[aria-controls="${DETAILS_CARD_ID}"]`)) return;
    onClose();
  });

  // The card listens before anything else, so while a menu, picker or dialog is open, here or
  // elsewhere, it leaves Escape to that popup.
  const onKeyDown = useEffectEvent((event: KeyboardEvent) => {
    if (event.key !== "Escape" || document.querySelector(OTHER_POPUP)) return;
    if (card.current?.contains(document.activeElement)) {
      document.querySelector<HTMLElement>(`[aria-controls="${DETAILS_CARD_ID}"]`)?.focus();
    }
    onClose();
  });

  // The Change base dialog is modal, so while it is open the card leaves its keys and clicks
  // alone. In the capture phase, so the Escape or click that closes the dialog has passed by the
  // time the card listens again.
  useEffect(() => {
    if (changingBase) return;
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [changingBase]);

  function act(action: Promise<void>) {
    action.then(
      () => setError(null),
      (e: unknown) => setError(String(e)),
    );
  }

  const running = new Map(runs.filter((run) => run.running).map((run) => [run.name, run]));
  // A running run session keeps its row until it exits, also when the settings dropped it or
  // cannot be read.
  const listed = runSessions ?? [];
  const scripts = [...listed, ...runs.filter((run) => run.running && !listed.some(({ name }) => name === run.name))];
  const servers = scripts.flatMap(({ name, port }) => {
    const live = running.get(name);
    const served = live?.port ?? port;
    return live && served !== null ? [{ name, port: served }] : [];
  });

  const rows: Record<Row, ReactNode> = {
    Worktree: (
      <div className="details-line">
        <span className="details-code details-path" title={worktree}>
          {worktree}
        </span>
        <IconButton
          icon={Copy}
          label="Copy path"
          size="xs"
          onClick={() => act(navigator.clipboard.writeText(worktree))}
        />
        <IconButton
          icon={FolderOpen}
          label="Reveal in Finder"
          size="xs"
          onClick={() => act(bridge.revealInFinder(worktree))}
        />
      </div>
    ),
    "Open in": (
      <div className="details-line">
        {EDITORS.map(([editor, label]) => (
          <Button key={editor} size="xs" onClick={() => act(bridge.openInEditor(editor, worktree, "", 1))}>
            {label}
          </Button>
        ))}
      </div>
    ),
    Scripts:
      scripts.length === 0 ? (
        <span className="details-faint">
          {runSessions === null ? "Loading" : "No run sessions in this repo's profile"}
        </span>
      ) : (
        <ul className="details-list">
          {scripts.map(({ name }) => {
            const run = runs.find((tab) => tab.name === name);
            const live = running.get(name);
            return (
              <li key={name} aria-label={name} className="details-line">
                <span className="details-code">{name}</span>
                <span className={live ? "details-state running" : "details-state"}>
                  {live ? "Running" : run ? "Stopped" : "Not started"}
                </span>
                {live ? (
                  <Button size="xs" aria-label={`Stop ${name}`} onClick={() => act(bridge.kill(live.session))}>
                    Stop
                  </Button>
                ) : (
                  <Button size="xs" aria-label={`Start ${name}`} onClick={() => onStartRun(name)}>
                    Start
                  </Button>
                )}
              </li>
            );
          })}
        </ul>
      ),
    Servers:
      servers.length === 0 ? (
        <span className="details-faint">No server running</span>
      ) : (
        <ul className="details-list">
          {servers.map(({ name, port }) => (
            <li key={name} className="details-line">
              <span className="details-code">{name}</span>
              <span className="details-code details-muted">localhost:{port}</span>
            </li>
          ))}
        </ul>
      ),
    Branch: tracked && (
      <div className="details-line details-wrap">
        <span className="details-code" title={tracked.worktree.branch ?? undefined}>
          {tracked.worktree.branch ?? "Detached HEAD"}
        </span>
        <span className="details-faint">base</span>
        <span className="details-code">{tracked.worktree.baseBranch ?? "None"}</span>
        <Button size="xs" variant="ghost" onClick={() => setChangingBase(true)}>
          Change base
        </Button>
      </div>
    ),
    Changes:
      changes === null ? (
        <span className="details-faint">Loading</span>
      ) : "error" in changes ? (
        <span className="details-faint">{changes.error}</span>
      ) : changes.files.length === 0 ? (
        <span className="details-faint">No changes</span>
      ) : (
        <Button size="xs" variant="ghost" title="Open the Diff panel" onClick={onOpenDiff}>
          <span>
            {changes.files.length} {changes.files.length === 1 ? "file" : "files"}
          </span>
          <span className="details-add">+{sum(changes.files, "added")}</span>
          <span className="details-del">-{sum(changes.files, "removed")}</span>
        </Button>
      ),
    Session:
      session === null ? (
        <span className="details-faint">No agent session</span>
      ) : (
        <div className="details-line details-wrap">
          <span>{AGENT_NAMES[session.agent]}</span>
          {session.model && <span className="details-muted">{session.model.model.name}</span>}
          {session.model?.effort && <span className="details-muted">{session.model.effort} effort</span>}
          {session.mode && <span className="details-muted">{session.mode} mode</span>}
          {session.source && <span className="details-faint">State from {session.source}</span>}
        </div>
      ),
  };

  const shown = ROWS.filter((row) => !hidden.includes(row));

  return (
    <div className="details-holder">
      <div ref={card} id={DETAILS_CARD_ID} className="details-card">
        <Card
          title="Details"
          actions={
            <Menu
              label="Rows"
              icon={SlidersHorizontal}
              heading="Show"
              align="end"
              items={ROWS.map((row) => ({
                key: row,
                label: row,
                checked: !hidden.includes(row),
                onSelect: () =>
                  setHidden((rows) => (rows.includes(row) ? rows.filter((r) => r !== row) : [...rows, row])),
              }))}
            />
          }
        >
          {shown.length === 0 && <p className="details-faint">Every row is hidden. Rows shows them again.</p>}
          {shown.map((row) => (
            <div key={row} role="group" aria-label={row} className="details-row">
              <span className="details-label">{row}</span>
              <div className="details-value">{rows[row]}</div>
            </div>
          ))}
          {error && (
            <p role="alert" className="details-error">
              {error}
            </p>
          )}
        </Card>
      </div>
      {changingBase && tracked && (
        <ChangeBaseDialog
          bridge={bridge}
          worktree={tracked.worktree}
          onSaved={() => {
            setChangingBase(false);
            setListings((n) => n + 1);
          }}
          onCancel={() => setChangingBase(false)}
        />
      )}
    </div>
  );
}

function sum(files: FileDiff[], field: "added" | "removed"): number {
  return files.reduce((total, file) => total + file[field], 0);
}

function isRowList(value: unknown): value is Row[] {
  return Array.isArray(value) && value.every((row) => (ROWS as readonly unknown[]).includes(row));
}
