import {
  AppWindow,
  Bot,
  ChevronDown,
  FileDiff as FileDiffIcon,
  FolderOpen,
  GitBranch,
  Laptop,
  type LucideIcon,
  Play,
  Plus,
  Server,
  Square,
} from "lucide-react";
import { type ReactNode, useEffect, useEffectEvent, useRef, useState } from "react";

import type { Agent, Bridge, Editor, FileDiff, Mode, SessionId, RunSession, StateSource, Worktree } from "./bridge";
import type { ModelChoice } from "./Picker";
import { AGENT_NAMES, byRepo } from "./Sidebar";
import { ChangeBaseDialog } from "./WorktreeDialogs";
import { IconButton } from "./ui/IconButton";
import { Menu } from "./ui/Menu";
import "./DetailsCard.css";

/** The card's id, which the button that opens it names in aria-controls. */
export const DETAILS_CARD_ID = "details-card";

// An open popup: a menu or picker, by the button that opened it, or a dialog.
const OTHER_POPUP = '[aria-haspopup][aria-expanded="true"], [role="dialog"]';

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
 * Where the selected worktree is, what runs in it and what changed, one row each: its folder, the
 * editors to open it in, a row per run session that starts or stops it, its branch, its changes
 * since the base branch, its servers and the focused agent session. A row whose data the worktree
 * does not have is left out. Escape or a click outside closes it, except on the button that opens
 * it, which closes it itself.
 */
export function DetailsCard({
  bridge,
  worktree,
  runs,
  session,
  onStartRun,
  onOpenDiff,
  onOpenProfile,
  onClose,
}: {
  bridge: Bridge;
  worktree: string;
  runs: RunTab[];
  /** null when the worktree has no agent session. */
  session: FocusedSession | null;
  onStartRun(name: string): void;
  onOpenDiff(): void;
  onOpenProfile(): void;
  onClose(): void;
}) {
  const [tracked, setTracked] = useState<{
    worktree: Worktree;
    repo: string;
  } | null>(null);
  // Bumped after the base branch changes, to list the worktree again.
  const [listings, setListings] = useState(0);
  const [runSessions, setRunSessions] = useState<RunSession[] | null>(null);
  // Bumped when a profile or the repo's settings change, to list its run sessions again.
  const [settingsChanges, setSettingsChanges] = useState(0);
  // The diff keeps the base it counts against, so a base that just changed shows no old counts.
  const [diff, setDiff] = useState<{ base: string | null; result: { files: FileDiff[] } | { error: string } } | null>(
    null,
  );
  const [diffChanges, setDiffChanges] = useState(0);
  const [changingBase, setChangingBase] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const card = useRef<HTMLElement>(null);

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

  // Changes count against the base branch. A worktree with none, such as the main worktree,
  // counts its uncommitted changes.
  const base = tracked ? (tracked.worktree.baseBranch ?? null) : undefined;
  useEffect(() => {
    if (base === undefined) return;
    let current = true;
    bridge.diff(worktree, base === null ? "Head" : "MergeBase").then(
      (files) => current && setDiff({ base, result: { files } }),
      (e: unknown) => current && setDiff({ base, result: { error: String(e) } }),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, base, diffChanges]);

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
  const scripts: {
    name: string;
    command: string | null;
    port: number | null;
  }[] = [
    ...listed,
    ...runs
      .filter((run) => run.running && !listed.some(({ name }) => name === run.name))
      .map(({ name, port }) => ({ name, command: null, port })),
  ];
  const servers = scripts.flatMap(({ name, port }) => {
    const live = running.get(name);
    const served = live?.port ?? port;
    return live && served !== null ? [`:${served} ${name}`] : [];
  });
  const sessionDetails = session && [session.model?.model.name, session.model?.effort, session.mode].filter(Boolean);
  const changes = diff && diff.base === base ? diff.result : null;
  const diffError = changes && "error" in changes ? changes.error : null;
  const alert = error ?? diffError;

  return (
    <div className="details-holder">
      <section ref={card} id={DETAILS_CARD_ID} className="details-card" aria-label="Details">
        <ul className="details-rows">
          <li aria-label="Worktree" className="details-row">
            <Laptop aria-hidden="true" />
            <span className="details-path" title={worktree}>
              {worktree}
            </span>
            <IconButton
              icon={FolderOpen}
              label="Reveal in Finder"
              size="xs"
              className="details-end"
              onClick={() => act(bridge.revealInFinder(worktree))}
            />
          </li>
          <li aria-label="Open in">
            <Menu
              label="Open in"
              icon={AppWindow}
              items={EDITORS.map(([editor, name]) => ({
                key: editor,
                label: name,
                onSelect: () => act(bridge.openInEditor(editor, worktree, "", 1)),
              }))}
              trigger={<RowContent icon={AppWindow} label="Open in" end={<Chevron />} />}
            />
          </li>
          {scripts.map(({ name, command }) => {
            const live = running.has(name);
            const stopped = runs.some((run) => run.name === name);
            return (
              <li key={name} aria-label={`Run ${name}`}>
                <button
                  type="button"
                  className="details-row"
                  aria-label={live ? `Stop ${name}` : `Start ${name}`}
                  onClick={() => {
                    const run = running.get(name);
                    if (run) act(bridge.kill(run.session));
                    else onStartRun(name);
                  }}
                >
                  <RowContent
                    icon={live ? Square : Play}
                    label={`Run ${name}`}
                    end={
                      live ? (
                        <span className="details-running">Running</span>
                      ) : stopped ? (
                        <span className="details-faint">Stopped</span>
                      ) : (
                        command && <span className="details-faint details-code">{command}</span>
                      )
                    }
                  />
                </button>
              </li>
            );
          })}
          <li aria-label="Add run session">
            <button type="button" className="details-row" onClick={onOpenProfile}>
              <RowContent icon={Plus} label="Add run session" />
            </button>
          </li>
          <li role="separator" className="details-divider" />
          {tracked && (
            <li aria-label="Branch">
              <Menu
                label="Branch"
                icon={GitBranch}
                items={[
                  {
                    key: "base",
                    label: "Change base",
                    onSelect: () => setChangingBase(true),
                  },
                ]}
                trigger={
                  <RowContent
                    icon={GitBranch}
                    label={<span className="details-code">{tracked.worktree.branch ?? "Detached HEAD"}</span>}
                    end={<Chevron />}
                  />
                }
              />
            </li>
          )}
          {changes && "files" in changes && (
            <li aria-label="Changes">
              <button type="button" className="details-row" title="Open the Diff panel" onClick={onOpenDiff}>
                <RowContent
                  icon={FileDiffIcon}
                  label={
                    <>
                      Changes
                      <span className="details-muted">vs {base ?? "HEAD"}</span>
                    </>
                  }
                  end={
                    <span className="details-code">
                      <span className="details-add">+{sum(changes.files, "added")}</span>{" "}
                      <span className="details-del">−{sum(changes.files, "removed")}</span>
                    </span>
                  }
                />
              </button>
            </li>
          )}
          {servers.length > 0 && (
            <li aria-label="Servers" className="details-row">
              <RowContent
                icon={Server}
                label="Servers"
                end={<span className="details-muted details-code">{servers.join(", ")}</span>}
              />
            </li>
          )}
          {session && (
            <li aria-label="Session" className="details-row" title={session.source && `State from ${session.source}`}>
              <RowContent
                icon={Bot}
                label={AGENT_NAMES[session.agent]}
                end={
                  sessionDetails?.length ? <span className="details-muted">{sessionDetails.join(" · ")}</span> : null
                }
              />
            </li>
          )}
        </ul>
        {alert && (
          <p role="alert" className="details-error">
            {alert}
          </p>
        )}
      </section>
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

/** A row's icon, its label, and what sits at its right edge. */
function RowContent({ icon: Icon, label, end }: { icon: LucideIcon; label: ReactNode; end?: ReactNode }) {
  return (
    <>
      <Icon aria-hidden="true" />
      <span className="details-label">{label}</span>
      {end && <span className="details-end">{end}</span>}
    </>
  );
}

function Chevron() {
  return <ChevronDown aria-hidden="true" className="details-chevron" />;
}

function sum(files: FileDiff[], field: "added" | "removed"): number {
  return files.reduce((total, file) => total + file[field], 0);
}
