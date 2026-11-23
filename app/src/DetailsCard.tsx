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
import { type ReactNode, useEffect, useLayoutEffect, useState } from "react";

import type {
  Agent,
  AgentSessionRecord,
  AgentStateChanged,
  Bridge,
  DiffBase,
  Editor,
  FileDiff,
  Mode,
  SessionId,
  RunSession,
  Worktree,
} from "./bridge";
import type { ModelChoice } from "./Picker";
import { AGENT_NAMES, byRepo } from "./Sidebar";
import { CHAT_MIN_WIDTH } from "./layout";
import { AgentState, sameState } from "./ui/AgentState";
import { ChangeBaseDialog } from "./WorktreeDialogs";
import { IconButton } from "./ui/IconButton";
import { Menu } from "./ui/Menu";
import "./DetailsCard.css";

/** The card's id, which the button that opens it names in aria-controls. */
export const DETAILS_CARD_ID = "details-card";

/** The column's width, as `--details-card-width` in DetailsCard.css. */
const DETAILS_WIDTH = 320;

const EDITORS: [Editor, string][] = [
  ["Zed", "Zed"],
  ["VsCode", "VS Code"],
];

/** The focused agent session, as the card shows it. */
export interface FocusedSession {
  session: SessionId;
  agent: Agent;
  /** undefined until its panel knows it, and in terminal mode. null on the agent's own default. */
  model: ModelChoice | null | undefined;
  /** undefined until its panel knows it. */
  mode: Mode | undefined;
  /** Its latest state and where it came from. undefined before its first state. */
  state: AgentStateChanged | undefined;
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
 * The focused agent session at the top: its title, its state since when, and where the state came
 * from. Then where the selected worktree is, what runs in it and what changed, one row each: its
 * folder, the editors to open it in, a row per run session that starts or stops it, its branch, its
 * changes since the base branch, its servers and the focused agent session. A row whose data the
 * worktree does not have is left out.
 *
 * It is a column at the chat area's right edge, as tall as the area, and the chat's text keeps out
 * of it. Only the button that opens it closes it.
 */
export function DetailsCard({
  bridge,
  worktree,
  runs,
  session,
  onStartRun,
  onOpenDiff,
  onOpenProfile,
}: {
  bridge: Bridge;
  worktree: string;
  runs: RunTab[];
  /** null when the worktree has no agent session. */
  session: FocusedSession | null;
  onStartRun(name: string): void;
  /** Opens the Diff on the base the Changes row counts against. */
  onOpenDiff(base: DiffBase): void;
  onOpenProfile(): void;
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
  // The focused session as the core lists it, for its title and the time it entered its state.
  const [record, setRecord] = useState<AgentSessionRecord | null>(null);
  const [error, setError] = useState<string | null>(null);

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

  // Listed again when the list changes, as when the session gets its title, and on each new state.
  // Only the answer to the latest ask counts, since an older one can arrive after it.
  const focused = session?.session;
  useEffect(() => {
    if (focused === undefined) return;
    let asks = 0;
    let listening = true;
    function list() {
      const ask = ++asks;
      bridge.agentSessions().then(
        (all) => listening && ask === asks && setRecord(all.find((listed) => listed.session === focused) ?? null),
        (e: unknown) => listening && ask === asks && setError(String(e)),
      );
    }
    const stopSessions = bridge.onAgentSessionsChanged(list);
    const stopStates = bridge.onAgentState((change) => change.session === focused && list());
    list();
    return () => {
      listening = false;
      stopSessions();
      stopStates();
    };
  }, [bridge, focused]);

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
  const focusedRecord = record?.session === focused ? record : null;
  // The newest state the window heard of, or before any the state the core lists. The time it was
  // entered shows only once the core lists that same state, so a new state never shows an old time.
  const listedState = focusedRecord?.state ? focusedRecord : undefined;
  const shownState = session?.state ?? listedState;
  const since = shownState && listedState && sameState(shownState, listedState) ? listedState.sinceUnixMs : null;

  return (
    <div className="details-holder">
      <section id={DETAILS_CARD_ID} className="details-card" aria-label="Details">
        {session && (
          <div role="group" aria-label="Agent session" className="details-session">
            {focusedRecord && (
              <h2 className="details-title" title={focusedRecord.title}>
                {focusedRecord.title}
              </h2>
            )}
            {shownState && (
              <p className="details-state">
                <AgentState state={shownState} />
                {since !== null && <span className="details-muted">since {clockTime(since)}</span>}
              </p>
            )}
            {session.state && <p className="details-meta">State from {session.state.source}</p>}
          </div>
        )}
        <ul className="details-rows">
          <li aria-label="Worktree" className="details-row">
            <Laptop aria-hidden="true" />
            <span className="details-path" title={worktree}>
              {/* Cut in the middle: the start gives way, the folder that names the worktree stays. */}
              <span className="details-path-start">{worktree.slice(0, worktree.lastIndexOf("/"))}</span>
              <span className="details-path-end">{worktree.slice(worktree.lastIndexOf("/"))}</span>
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
                        command && <span className="details-faint">{command}</span>
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
                    label={tracked.worktree.branch ?? "Detached HEAD"}
                    end={<Chevron />}
                  />
                }
              />
            </li>
          )}
          {changes && "files" in changes && (
            <li aria-label="Changes">
              <button
                type="button"
                className="details-row"
                title="Open the Diff panel"
                onClick={() => onOpenDiff(base ? "MergeBase" : "Head")}
              >
                <RowContent
                  icon={FileDiffIcon}
                  label={
                    <>
                      Changes
                      <span className="details-muted">vs {base ?? "HEAD"}</span>
                    </>
                  }
                  end={
                    <span>
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
                end={<span className="details-muted">{servers.join(", ")}</span>}
              />
            </li>
          )}
          {session && (
            <li aria-label="Session" className="details-row">
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

/**
 * Whether the chat area has room for the Details column beside `panels` agent panels of at least
 * `CHAT_MIN_WIDTH` each. It is measured before the browser paints, so a chat area too narrow never
 * shows the column, and again each time the area resizes. The column sits inside the area, so it
 * leaves the measured width alone and cannot make the answer flip back and forth.
 */
export function useDetailsRoom(chatArea: HTMLElement | null, panels: number): boolean {
  const [width, setWidth] = useState<number | null>(null);
  useLayoutEffect(() => {
    if (!chatArea) return;
    const measure = () => setWidth(chatArea.getBoundingClientRect().width);
    measure();
    const sizes = new ResizeObserver(measure);
    sizes.observe(chatArea);
    return () => sizes.disconnect();
  }, [chatArea]);
  return width !== null && width - DETAILS_WIDTH >= Math.max(panels, 1) * CHAT_MIN_WIDTH;
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

/** A time as "14:05" today, and as "Nov 19, 14:05" on another day. */
function clockTime(unixMs: number): string {
  const at = new Date(unixMs);
  const time = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
  if (at.toDateString() === new Date().toDateString()) return time;
  return `${at.toLocaleDateString([], { month: "short", day: "numeric" })}, ${time}`;
}

function sum(files: FileDiff[], field: "added" | "removed"): number {
  return files.reduce((total, file) => total + file[field], 0);
}
