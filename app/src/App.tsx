import { type CSSProperties, useEffect, useEffectEvent, useRef, useState } from "react";

import type {
  Account,
  Agent,
  AgentStateChanged,
  AttentionEntry,
  Bridge,
  Mode,
  NotificationClicked,
  PanelKind,
  SessionId,
  Workspace,
  Worktree,
} from "./bridge";
import { ChatPanel } from "./ChatPanel";
import { DetailsCard, type FocusedSession, type RunTab } from "./DetailsCard";
import { DiffView } from "./DiffView";
import { FilesPanel } from "./FilesPanel";
import { useLayout } from "./layout";
import { PanelTabs, tabId } from "./PanelTabs";
import type { ModelChoice } from "./Picker";
import { ProfilePanel } from "./ProfilePanel";
import { QuitGuard } from "./QuitGuard";
import { AGENT_NAMES, type AgentSession, Sidebar, byRepo, folderName, sessionNames } from "./Sidebar";
import { Splitter } from "./Splitter";
import { TerminalPanel } from "./TerminalPanel";
import { useAppearance } from "./theme";
import { TitleBar } from "./TitleBar";
import { Warnings } from "./Warnings";
import {
  closeTab,
  hideSession,
  MAX_SHOWN_AGENT_PANELS,
  type MenuPanel,
  openPanel,
  showSession,
  useWorkspaces,
} from "./workspace";
import { Button } from "./ui/Button";

/**
 * The window: a title bar over the sidebar, the chat area and the panel area. The selected
 * worktree's workspace says which agent panels the chat area shows side by side, which tabs the
 * panel area has, and whether its terminal is open.
 */
export function App({ bridge }: { bridge: Bridge }) {
  const { layout, sidebar, panels, toggleSidebar, togglePanels, resizeSidebar, resizePanels } =
    useLayout();
  const { appearance, setAppearance } = useAppearance();
  const [selected, setSelected] = useState<string | null>(null);
  // Every tracked worktree, as the sidebar last listed them.
  const [worktrees, setWorktrees] = useState<Worktree[]>([]);
  const {
    workspaces,
    errors: workspaceErrors,
    load: loadWorkspace,
    change: changeWorkspace,
    forget: forgetWorkspace,
    startAgent: startAgentInWorkspace,
  } = useWorkspaces(bridge);
  // Every worktree selected so far. Each keeps its Files panel, with its open files and edits.
  const [visited, setVisited] = useState<string[]>([]);
  if (selected && !visited.includes(selected)) setVisited([...visited, selected]);
  // Each worktree's shell session, by worktree path.
  const [shells, setShells] = useState<Record<string, SessionId>>({});
  // Each worktree's run session tabs, by worktree path: one per run session name, in the order they
  // came.
  const [runs, setRuns] = useState<Record<string, RunTab[]>>({});
  // The run session each worktree's terminal panel shows, by worktree path. A worktree left out
  // shows its shell.
  const [shownRuns, setShownRuns] = useState<Record<string, string>>({});
  // Every session whose process ended. A quick command can end before its start returns.
  const exited = useRef(new Set<SessionId>());
  // Every agent session started, in order.
  const [agents, setAgents] = useState<AgentSession[]>([]);
  // Each agent session's latest state, by session. An agent session has none until its first.
  const [agentStates, setAgentStates] = useState<Record<SessionId, AgentStateChanged>>({});
  // Each agent's login: null when nobody is logged in, missing until a session reports it.
  const [accounts, setAccounts] = useState<Partial<Record<Agent, Account | null>>>({});
  // Why the last shell, agent or run session started in a worktree failed, by `startKey`.
  const [startErrors, setStartErrors] = useState<Record<string, string>>({});
  // The worktrees being deleted. Cmd-Q waits for each one's delete to end.
  const [deleting, setDeleting] = useState<Worktree[]>([]);
  // The shells, agent and run sessions that are starting, by `startKey`, so a double click starts
  // one.
  const starting = useRef(new Set<string>());
  // How many times each worktree was removed, so a start that returns after a removal leaves the
  // one made again at its path alone.
  const removals = useRef(new Map<string, number>());
  // The latest ask for each worktree's sessions, by worktree. An answer to an older ask is stale.
  const listing = useRef(new Map<string, object>());
  // A workspace can hold sessions started before the window reloaded. Each gets its panel.
  const unknown = Object.entries(workspaces).flatMap(([worktree, workspace]) =>
    workspace.chat.flatMap(({ agent, session }) =>
      session !== null && !agents.some((started) => started.session === session)
        ? [{ session, worktree, agent, panel: session }]
        : [],
    ),
  );
  if (unknown.length > 0) setAgents([...agents, ...unknown]);
  // The agent session whose panel has focus.
  const focusedAgent = useRef<SessionId | null>(null);
  // The agent session whose panel last had focus, by worktree, for the Details card.
  const [lastFocused, setLastFocused] = useState<Record<string, SessionId>>({});
  // Each agent session's mode and model, as its panel reports them, by session.
  const [sessionDetails, setSessionDetails] = useState<
    Record<SessionId, { mode?: Mode; model?: ModelChoice | null }>
  >({});
  const [detailsOpen, setDetailsOpen] = useState(false);
  // The attention queue, head first, as the core last listed it.
  const [attentionQueue, setAttentionQueue] = useState<AttentionEntry[]>([]);
  // The last jump's session, and a count that grows with each jump so its panel takes focus again.
  const [jumped, setJumped] = useState<{ session: SessionId; request: number }>({
    session: "",
    request: 0,
  });

  useEffect(
    () =>
      bridge.onAgentState((change) => {
        setAgentStates((states) => ({ ...states, [change.session]: change }));
        // The user is looking at the panel, so the turn waits on no one.
        if (change.state === "Done" && change.session === focusedAgent.current) {
          bridge.focus(change.session).catch(() => {});
        }
      }),
    [bridge],
  );

  // The core orders the queue, so the UI asks for it again on each state change. Only the answer
  // to the latest ask counts, since an older one can arrive after it. It fails only once grove is
  // quitting, so there is nothing to report.
  useEffect(() => {
    let asks = 0;
    let listening = true;
    function refresh() {
      const ask = ++asks;
      bridge
        .attentionQueue()
        .then((queue) => listening && ask === asks && setAttentionQueue(queue))
        .catch(() => {});
    }
    const stop = bridge.onAgentState(refresh);
    refresh();
    return () => {
      listening = false;
      stop();
    };
  }, [bridge]);

  const onJumpKey = useEffectEvent((event: KeyboardEvent) => {
    if (!isJumpKey(event)) return;
    event.preventDefault();
    jump();
  });

  // In the capture phase, so the key works with focus in a terminal or an editor too.
  useEffect(() => {
    window.addEventListener("keydown", onJumpKey, true);
    return () => window.removeEventListener("keydown", onJumpKey, true);
  }, []);

  // A failed load says why, and the next change to the worktree tries again.
  const onSelected = useEffectEvent((worktree: string) => {
    void loadWorkspace(worktree).catch(() => {});
    findSessions(worktree);
  });

  useEffect(() => {
    if (selected) void onSelected(selected);
  }, [selected]);

  // A run session that exits keeps its tab, which says it stopped.
  useEffect(
    () =>
      bridge.onSessionExited((session) => {
        exited.current.add(session);
        setRuns((all) =>
          Object.fromEntries(
            Object.entries(all).map(([worktree, tabs]) => [
              worktree,
              tabs.map((tab) => (tab.session === session ? { ...tab, running: false } : tab)),
            ]),
          ),
        );
      }),
    [bridge],
  );

  useEffect(
    () =>
      bridge.onAccount((change) => setAccounts((known) => ({ ...known, [change.agent]: change.account }))),
    [bridge],
  );

  // A shell starts at 80x24, and its panel resizes it once it has measured itself.
  // A run session's kind holds its name, so each run session starts and fails on its own.
  async function start(
    kind: StartKind,
    worktree: string,
    spawn: () => Promise<SessionId>,
    started: (session: SessionId) => void,
  ) {
    const key = startKey(kind, worktree);
    if (starting.current.has(key)) return;
    starting.current.add(key);
    try {
      started(await spawn());
      setStartErrors((errors) => {
        const rest = { ...errors };
        delete rest[key];
        return rest;
      });
    } catch (e) {
      setStartErrors((errors) => ({ ...errors, [key]: String(e) }));
    } finally {
      starting.current.delete(key);
    }
  }

  // A shell or run session the core has in the worktree, such as one a restart brought back, gets
  // its panel. The core is asked once per worktree: from then on the window starts and ends the
  // worktree's sessions itself. A failed ask says why, and the next select asks again.
  function findSessions(worktree: string) {
    if (listing.current.has(worktree)) return;
    const ask = {};
    listing.current.set(worktree, ask);
    bridge.sessions(worktree).then(
      (sessions) => {
        if (listing.current.get(worktree) !== ask) return;
        const shell = sessions.find((info) => info.kind === "Shell");
        if (shell) setShells((open) => (open[worktree] ? open : { ...open, [worktree]: shell.session }));
        // One that exited while the ask was out was listed running.
        const listed = sessions.flatMap((info) =>
          info.kind === "Run"
            ? [
                {
                  name: info.name,
                  session: info.session,
                  running: info.running && !exited.current.has(info.session),
                  port: null,
                },
              ]
            : [],
        );
        // A tab the window already has wins over the listed one of its name.
        setRuns((all) => ({
          ...all,
          [worktree]: listed.reduce(
            (tabs, tab) => (tabs.some((known) => known.name === tab.name) ? tabs : [...tabs, tab]),
            all[worktree] ?? [],
          ),
        }));
      },
      (e: unknown) => {
        if (listing.current.get(worktree) !== ask) return;
        listing.current.delete(worktree);
        setStartErrors((errors) => ({ ...errors, [startKey("shell", worktree)]: String(e) }));
      },
    );
  }

  function openShell(worktree: string) {
    return start(
      "shell",
      worktree,
      () => bridge.spawnShell(worktree, 80, 24),
      (session) => setShells((open) => ({ ...open, [worktree]: session })),
    );
  }

  // Shows the run session's tab in the worktree's terminal panel, and the panel too.
  function showRun(worktree: string, name: string) {
    setShownRuns((shown) => ({ ...shown, [worktree]: name }));
    open(worktree, "Terminal");
  }

  // The port the repo's settings give the worktree's run session `name`. The tab keeps it, so the
  // Details card still shows it after the settings drop the run session.
  async function runPortOf(worktree: string, name: string): Promise<number | null> {
    const repo = byRepo(await bridge.listWorktrees()).find((worktrees) => worktrees.some((w) => w.path === worktree));
    if (!repo) return null;
    return (await bridge.runSessions(repo[0].path)).find((run) => run.name === name)?.port ?? null;
  }

  // Starts the worktree's run session `name` and shows its tab. One already running there is only
  // shown. A new one takes the place of a stopped one of that name. A failed start shows its tab
  // too, stopped, with why.
  async function startRun(worktree: string, name: string) {
    if (runs[worktree]?.some((tab) => tab.name === name && tab.running)) return showRun(worktree, name);
    const removalsBefore = removals.current.get(worktree) ?? 0;
    const removed = () => (removals.current.get(worktree) ?? 0) !== removalsBefore;
    // The tab does not wait for the port. A port that cannot be read stays null.
    const portRead = runPortOf(worktree, name).catch(() => null);
    await start(
      `run ${name}`,
      worktree,
      () => bridge.spawnRun(worktree, name, 80, 24),
      (session) => {
        if (removed()) return;
        void portRead.then((port) => {
          if (removed()) return;
          setRuns((all) => ({
            ...all,
            [worktree]: (all[worktree] ?? []).map((tab) => (tab.session === session ? { ...tab, port } : tab)),
          }));
        });
        setRuns((all) => {
          const tabs = all[worktree] ?? [];
          const tab = { name, session, running: !exited.current.has(session), port: null };
          const replaced = tabs.some((known) => known.name === name);
          return {
            ...all,
            [worktree]: replaced ? tabs.map((known) => (known.name === name ? tab : known)) : [...tabs, tab],
          };
        });
      },
    );
    if (!removed()) showRun(worktree, name);
  }

  function showShell(worktree: string) {
    setShownRuns((shown) => {
      const rest = { ...shown };
      delete rest[worktree];
      return rest;
    });
  }

  // The core puts the new session in agent panel `at` (a new one after the last when null), and
  // its panel is drawn before the start ends, so a second click starts nothing more. Without the
  // workspace the panel could not be drawn, so none starts.
  async function startAgent(
    worktree: string,
    agent: Agent,
    at: (workspace: Workspace) => number | null,
    afterSetup = false,
  ) {
    try {
      await loadWorkspace(worktree);
    } catch {
      // The load has said why.
      return;
    }
    const spawn = (panel: number | null) =>
      afterSetup ? bridge.spawnAgentAfterSetup(worktree, agent, panel) : bridge.spawnAgent(worktree, agent, panel);
    return start(
      "agent",
      worktree,
      () => startAgentInWorkspace(worktree, agent, at, spawn, { afterSetup }),
      () => {},
    );
  }

  // From the sidebar: the first shown panel not started yet, or else a new one.
  function firstEmptyPanel(workspace: Workspace) {
    const at = workspace.chat.findIndex((panel) => panel.shown && panel.session === null);
    return at < 0 ? null : at;
  }

  // Splitting starts the other agent, so one can implement while the other reviews.
  function split({ worktree, agent }: AgentSession) {
    const other: Agent = agent === "Claude" ? "Codex" : "Claude";
    return startAgent(worktree, other, () => null);
  }

  // Something else may have filled the panel while the session started. Then the core puts it in a
  // new one.
  function startPanel(worktree: string, at: number, agent: Agent) {
    return startAgent(worktree, agent, () => at);
  }

  // Before the first message the panel can take another agent: a new session takes its place,
  // and the old one ends. The panel stays, with its draft. The core starts the new session in a
  // panel of its own, which goes.
  function changeAgent(old: AgentSession, agent: Agent) {
    return start(
      "agent",
      old.worktree,
      () =>
        startAgentInWorkspace(old.worktree, agent, () => null, (panel) => bridge.spawnAgent(old.worktree, agent, panel)),
      (session) => {
        // The start drew the new session in a panel of its own.
        setAgents((started) =>
          started.flatMap((a) =>
            a.session === session ? [] : a.session === old.session ? [{ ...a, session, agent }] : [a],
          ),
        );
        void changeWorkspace(old.worktree, (workspace) => ({
          ...workspace,
          chat: workspace.chat.flatMap((panel) =>
            panel.session === session ? [] : panel.session === old.session ? [{ ...panel, agent, session }] : [panel],
          ),
        }));
        // The panel keeps focus through the swap, so no focus event tells the core.
        if (focusedAgent.current === old.session) focus(session, old.worktree);
        // Fails only when the session has already exited.
        bridge.kill(old.session).catch(() => {});
      },
    );
  }

  function showAgent({ session, worktree, agent }: AgentSession) {
    setSelected(worktree);
    void changeWorkspace(worktree, (workspace) => showSession(workspace, session, agent));
  }

  // Shows agent session `session` and puts focus in its panel, which calls `focus`. A session
  // started before the window reloaded, or a subagent, has no panel yet, so it gets one here.
  // Nothing says which agent the session runs, so its panel is named Claude.
  function focusSession({ session, worktree }: { session: SessionId; worktree: string }) {
    const known = agents.find((started) => started.session === session);
    const agent: AgentSession = known ?? { session, worktree, agent: "Claude", panel: session };
    if (!known) setAgents((started) => [...started, agent]);
    showAgent(agent);
    setJumped(({ request }) => ({ session, request: request + 1 }));
  }

  // Focuses the head of the attention queue.
  function jump() {
    const head = attentionQueue[0];
    if (head) focusSession(head);
  }

  // A session with no panel that waits on no one has ended, or its worktree is gone, since the
  // notification. Then the click only brings grove forward, which the shell did.
  const onNotificationClicked = useEffectEvent((clicked: NotificationClicked) => {
    const has = (entry: { session: SessionId }) => entry.session === clicked.session;
    if (agents.some(has) || attentionQueue.some(has)) focusSession(clicked);
  });

  useEffect(() => bridge.onNotificationClicked(onNotificationClicked), [bridge]);

  // focus fails only once grove is quitting, so there is nothing to report.
  function focus(session: SessionId, worktree: string) {
    focusedAgent.current = session;
    setLastFocused((last) => ({ ...last, [worktree]: session }));
    bridge.focus(session).catch(() => {});
  }

  // Opening a panel shows the panel area too.
  function open(worktree: string, panel: MenuPanel) {
    void changeWorkspace(worktree, (workspace) => openPanel(workspace, panel));
    if (!layout.panelsOpen) togglePanels();
  }

  function toggleTerminal(worktree: string) {
    if (workspaces[worktree]?.terminal) {
      void changeWorkspace(worktree, (workspace) => ({ ...workspace, terminal: false }));
    } else {
      open(worktree, "Terminal");
    }
  }

  // Closing kills the shell, since nothing else can reach it yet. Kill fails only when the shell
  // has already exited, so there is nothing to report.
  function closeShell(worktree: string) {
    bridge.kill(shells[worktree]).catch(() => {});
    setShells((open) => {
      const rest = { ...open };
      delete rest[worktree];
      return rest;
    });
  }

  // The core ended a forgotten or deleted worktree's sessions. Dropping all it kept for the
  // worktree means one made again at its path starts empty.
  function clearRemovedWorktree(worktree: string) {
    removals.current.set(worktree, (removals.current.get(worktree) ?? 0) + 1);
    const ended = new Set(agents.filter((agent) => agent.worktree === worktree).map((agent) => agent.session));
    setAgents((started) => started.filter((agent) => agent.worktree !== worktree));
    setAgentStates((states) =>
      Object.fromEntries(Object.entries(states).filter(([session]) => !ended.has(session))),
    );
    setShells((open) => {
      const rest = { ...open };
      delete rest[worktree];
      return rest;
    });
    setRuns((all) => {
      const rest = { ...all };
      delete rest[worktree];
      return rest;
    });
    showShell(worktree);
    setStartErrors((errors) =>
      Object.fromEntries(Object.entries(errors).filter(([key]) => worktreeOfStartKey(key) !== worktree)),
    );
    setVisited((seen) => seen.filter((path) => path !== worktree));
    listing.current.delete(worktree);
    forgetWorkspace(worktree);
  }

  function reportDetails(session: SessionId, details: { mode?: Mode; model?: ModelChoice | null }) {
    setSessionDetails((known) => ({ ...known, [session]: { ...known[session], ...details } }));
  }

  // The worktree's agent session whose panel last had focus, or else its first shown one. Its model
  // shows only in chat mode, where the panel knows it.
  function focusedSession(worktree: string): FocusedSession | null {
    const inWorktree = agents.filter((started) => started.worktree === worktree);
    const found =
      inWorktree.find((started) => started.session === lastFocused[worktree]) ??
      inWorktree.find((started) => places[started.session]?.shown);
    if (!found) return null;
    const { mode, model } = sessionDetails[found.session] ?? {};
    return {
      agent: found.agent,
      model: mode === "Chat" ? model : undefined,
      mode,
      source: agentStates[found.session]?.source,
    };
  }

  const workspace = selected ? workspaces[selected] : undefined;
  // Each agent session's place in its worktree's chat area, by session.
  const places: Record<SessionId, { at: number; shown: boolean }> = {};
  for (const [worktree, { chat }] of Object.entries(workspaces)) {
    chat.forEach(({ session, shown }, at) => {
      if (session !== null) places[session] = { at, shown: shown && worktree === selected };
    });
  }
  const shownCount = workspace?.chat.filter((panel) => panel.shown).length ?? 0;
  const shownAgents = agents.filter((a) => places[a.session]?.shown).map((a) => a.session);
  const names = sessionNames(agents);
  const placeholder = PLACEHOLDER_TABS.find((tab) => tab === workspace?.selected);
  // The run session each worktree's terminal panel shows, by name. undefined shows the shell.
  const shownRunName = selected ? shownRuns[selected] : undefined;
  const shellError = selected && !shownRunName && startErrors[startKey("shell", selected)];
  const runError = selected && shownRunName && startErrors[startKey(`run ${shownRunName}`, selected)];
  const shownRunOf = (worktree: string) => runs[worktree]?.find((tab) => tab.name === shownRuns[worktree]);
  const shownRun = selected ? shownRunOf(selected) : undefined;
  // The selected worktree's run session tabs, and one for a run session whose first start failed.
  const runTabs: { name: string; running: boolean }[] = selected ? [...(runs[selected] ?? [])] : [];
  if (shownRunName && !shownRun) runTabs.push({ name: shownRunName, running: false });
  const terminalShown = (worktree: string) => worktree === selected && layout.panelsOpen && !!workspace?.terminal;
  const agentError = selected && startErrors[startKey("agent", selected)];
  const workspaceError = selected && workspaceErrors[selected];
  const detailsSession = selected ? focusedSession(selected) : null;
  const selectedRepo = byRepo(worktrees).find((repo) => repo.some((w) => w.path === selected));
  const selectedWorktree = selectedRepo?.find((w) => w.path === selected);
  const regions = ["regions"];
  if (!layout.sidebarOpen) regions.push("no-sidebar");
  if (!layout.panelsOpen) regions.push("no-panels");
  const widths = {
    "--sidebar-w": `${sidebar.width}px`,
    "--panels-w": `${panels.width}px`,
  } as CSSProperties;

  return (
    <>
      <div className="app">
        <QuitGuard
          bridge={bridge}
          agentStates={agentStates}
          deleting={deleting.map((worktree) => worktree.displayName)}
        />
        <div className={regions.join(" ")} style={widths}>
          {/* The title bar spans the chat area and the panel area. The sidebar runs beside it. */}
          <div className="window-top">
            <TitleBar
              repo={selectedRepo ? folderName(selectedRepo[0].path) : null}
              worktree={selectedWorktree?.displayName ?? null}
              sidebarOpen={layout.sidebarOpen}
              panelsOpen={layout.panelsOpen}
              terminalOpen={workspace?.terminal ?? null}
              onToggleSidebar={toggleSidebar}
              onTogglePanels={togglePanels}
              onToggleTerminal={() => selected && toggleTerminal(selected)}
              detailsOpen={selected ? detailsOpen : null}
              onToggleDetails={() => setDetailsOpen(!detailsOpen)}
            />
            <Warnings bridge={bridge} />
          </div>
          {/* Collapsed regions stay mounted, so they keep their state and their terminals. */}
          <div className="sidebar-region" hidden={!layout.sidebarOpen}>
            <Sidebar
              bridge={bridge}
              selected={selected}
              onSelect={(worktree) => {
                // The selected worktree went away, and the Details card with it.
                if (worktree === null) setDetailsOpen(false);
                setSelected(worktree);
              }}
              onDeleting={(worktree, running) =>
                setDeleting((all) =>
                  running ? [...all, worktree] : all.filter((other) => other.path !== worktree.path),
                )
              }
              onRemoved={clearRemovedWorktree}
              agents={agents}
              agentStates={agentStates}
              shownAgents={shownAgents}
              onStartAgent={(worktree) => startAgent(worktree, "Claude", firstEmptyPanel)}
              onStartAgentAfterSetup={(worktree) =>
                startAgent(worktree, "Claude", firstEmptyPanel, true)
              }
              onShowAgent={showAgent}
              onWorktrees={setWorktrees}
              waiting={attentionQueue.length}
              onJump={jump}
              onOpenProfiles={() => selected && open(selected, "Profile")}
              accounts={accounts}
              appearance={appearance}
              onAppearance={setAppearance}
            />
          </div>
          {layout.sidebarOpen && (
            <Splitter
              label="Resize sidebar"
              side="before"
              region={sidebar}
              onResize={resizeSidebar}
            />
          )}
          <main className="chat-area">
            {selected && detailsOpen && (
              <DetailsCard
                key={selected}
                bridge={bridge}
                worktree={selected}
                runs={runs[selected] ?? []}
                session={detailsSession}
                onStartRun={(name) => void startRun(selected, name)}
                onOpenDiff={() => open(selected, "Diff")}
                onClose={() => setDetailsOpen(false)}
              />
            )}
            {(agentError || workspaceError) && (
              <p role="alert" className="chat-error">
                {agentError || workspaceError}
              </p>
            )}
            {/* Every agent panel stays mounted, with its draft. Its place in the workspace sets its order. */}
            <div className="chat-panels" hidden={shownCount === 0}>
              {agents.map((started) => (
                <ChatPanel
                  key={started.panel}
                  bridge={bridge}
                  session={started.session}
                  agent={started.agent}
                  sessionName={names[started.session]}
                  worktree={started.worktree}
                  shown={places[started.session]?.shown ?? false}
                  order={places[started.session]?.at}
                  state={agentStates[started.session]}
                  focusRequest={jumped.session === started.session ? jumped.request : 0}
                  onOpenDiff={() => open(started.worktree, "Diff")}
                  onChangeAgent={(agent) => changeAgent(started, agent)}
                  onSplit={shownCount < MAX_SHOWN_AGENT_PANELS ? () => split(started) : undefined}
                  onHide={
                    shownCount > 1
                      ? () => changeWorkspace(started.worktree, (w) => hideSession(w, started.session))
                      : undefined
                  }
                  onMode={(mode) => reportDetails(started.session, { mode })}
                  onModelChoice={(model) => reportDetails(started.session, { model })}
                  onFocus={() => focus(started.session, started.worktree)}
                  onBlur={() => {
                    if (focusedAgent.current === started.session) focusedAgent.current = null;
                  }}
                />
              ))}
              {selected &&
                workspace?.chat.map(
                  (panel, at) =>
                    panel.shown &&
                    panel.session === null && (
                      <div key={`unstarted ${at}`} className="chat-panel" style={{ order: at }}>
                        <div className="pane-head">
                          <b>{AGENT_NAMES[panel.agent]}</b>
                        </div>
                        <div className="empty-chat">
                          <h2>No agent session yet</h2>
                          <p>
                            <Button size="sm" onClick={() => startPanel(selected, at, panel.agent)}>
                              Start session
                            </Button>
                          </p>
                        </div>
                      </div>
                    ),
                )}
            </div>
            {(!selected || workspace?.chat.every((panel) => !panel.shown)) && (
              <div className="empty-chat">
                <h2>{selected ? "No agent session in this worktree" : "No worktree selected"}</h2>
                <p>
                  {selected
                    ? "Start Claude Code from the worktree in the sidebar."
                    : "Add a repo and pick a worktree in the sidebar."}
                </p>
              </div>
            )}
          </main>
          {layout.panelsOpen && (
            <Splitter
              label="Resize panel area"
              side="after"
              region={panels}
              onResize={resizePanels}
            />
          )}
          <section className="panel-area" aria-label="Panel area" hidden={!layout.panelsOpen}>
            {selected && workspace && (
              <PanelTabs
                workspace={workspace}
                onSelect={(tab) => changeWorkspace(selected, (w) => ({ ...w, selected: tab }))}
                onClose={(tab) => changeWorkspace(selected, (w) => closeTab(w, tab))}
                onOpen={(panel) => open(selected, panel)}
              />
            )}
            <div
              className="panel-body"
              id={tabId("Files", "panel")}
              role="tabpanel"
              aria-labelledby={tabId("Files", "tab")}
              hidden={workspace?.selected !== "Files"}
            >
              {visited.map((worktree) => (
                <div key={worktree} className="files-holder" hidden={worktree !== selected}>
                  <FilesPanel bridge={bridge} worktree={worktree} />
                </div>
              ))}
            </div>
            <div
              className="panel-body"
              id={tabId("Diff", "panel")}
              role="tabpanel"
              aria-labelledby={tabId("Diff", "tab")}
              hidden={workspace?.selected !== "Diff"}
            >
              {/* Keyed, so another worktree starts with its own base, file and cursor. */}
              {selected && workspace?.panels.includes("Diff") && (
                <DiffView
                  key={selected}
                  bridge={bridge}
                  worktree={selected}
                  agentStates={agentStates}
                  onSent={(session) => {
                    // The batch shows in its panel, so the panel shows too.
                    const started = agents.find((agent) => agent.session === session);
                    if (started) showAgent(started);
                  }}
                />
              )}
            </div>
            <div
              className="panel-body"
              id={tabId("Profile", "panel")}
              role="tabpanel"
              aria-labelledby={tabId("Profile", "tab")}
              hidden={workspace?.selected !== "Profile"}
            >
              {/* Each worktree with the tab open keeps its panel, with its unsaved edits. */}
              {visited
                .filter((worktree) => workspaces[worktree]?.panels.includes("Profile"))
                .map((worktree) => (
                  <div key={worktree} className="files-holder" hidden={worktree !== selected}>
                    <ProfilePanel
                      bridge={bridge}
                      worktree={worktree}
                      onStartRun={(name) => void startRun(worktree, name)}
                    />
                  </div>
                ))}
            </div>
            {placeholder && (
              <div
                className="panel-body"
                id={tabId(placeholder, "panel")}
                role="tabpanel"
                aria-labelledby={tabId(placeholder, "tab")}
              >
                <p className="panel-empty">The {placeholder} panel is not available yet.</p>
              </div>
            )}
            {!selected && <p className="panel-empty">Select a worktree to see its panels.</p>}
            {workspace?.selected === null && (
              <p className="panel-empty">No panel open. Use + to open one.</p>
            )}
            {selected && (
              <div
                className={shells[selected] || shownRun ? "bottom open" : "bottom"}
                hidden={!workspace?.terminal}
              >
                <div className="bottom-head">
                  {/* Tabs only once the worktree has a run session. Until then it holds the shell alone. */}
                  {runTabs.length ? (
                    <div className="terminal-tabs" role="tablist" aria-label="Terminal sessions">
                      <button
                        className="terminal-tab"
                        role="tab"
                        aria-selected={!shownRunName}
                        onClick={() => showShell(selected)}
                      >
                        Shell
                      </button>
                      {runTabs.map((tab) => (
                        <button
                          key={tab.name}
                          className="terminal-tab"
                          role="tab"
                          aria-selected={shownRunName === tab.name}
                          onClick={() => showRun(selected, tab.name)}
                        >
                          {tab.name}{" "}
                          <span className={tab.running ? "run-state running" : "run-state"}>
                            {tab.running ? "running" : "stopped"}
                          </span>
                        </button>
                      ))}
                    </div>
                  ) : (
                    <span>Shell</span>
                  )}
                  <span className="spacer" />
                  {shellError && <p role="alert">{shellError}</p>}
                  {runError && <p role="alert">{runError}</p>}
                  {shownRunName ? (
                    !shownRun?.running && (
                      <Button size="xs" onClick={() => void startRun(selected, shownRunName)}>
                        Start again
                      </Button>
                    )
                  ) : shells[selected] ? (
                    <Button size="xs" onClick={() => closeShell(selected)}>
                      Close shell
                    </Button>
                  ) : (
                    <Button size="xs" onClick={() => openShell(selected)}>
                      Open shell
                    </Button>
                  )}
                </div>
                {/* Every worktree's panels stay open. Only the selected worktree's chosen tab is shown. */}
                {Object.entries(shells).map(([worktree, session]) => (
                  <TerminalPanel
                    key={session}
                    bridge={bridge}
                    session={session}
                    shown={terminalShown(worktree) && !shownRuns[worktree]}
                  />
                ))}
                {Object.entries(runs).flatMap(([worktree, tabs]) =>
                  tabs.map((tab) => (
                    <TerminalPanel
                      key={tab.session}
                      bridge={bridge}
                      session={tab.session}
                      shown={terminalShown(worktree) && shownRunOf(worktree)?.session === tab.session}
                      ended={!tab.running}
                    />
                  )),
                )}
              </div>
            )}
          </section>
        </div>
      </div>
    </>
  );
}

// Tabs that show only a placeholder until their own issues.
const PLACEHOLDER_TABS: PanelKind[] = ["Browser", "Logs"];

type StartKind = "shell" | "agent" | `run ${string}`;

// JSON keeps any two kinds and worktrees apart, whatever a run session's name or a path holds.
function startKey(kind: StartKind, worktree: string): string {
  return JSON.stringify([kind, worktree]);
}

function worktreeOfStartKey(key: string): string {
  return (JSON.parse(key) as [StartKind, string])[1];
}

/** Cmd-J, as the title bar shows it. Plain Ctrl-J is a newline in a terminal, so it stays free. */
function isJumpKey(event: KeyboardEvent): boolean {
  return (
    event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "j"
  );
}
