import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode, useState } from "react";
import { afterEach, beforeEach, expect, onTestFinished, test, vi } from "vitest";

import { App } from "./App";
import type { Agent, AgentState, StateSource, Workspace, Worktree } from "./bridge";
import { DEFAULT_WORKSPACE, type FakeBridge, fakeBridge } from "./fakeBridge";
import { keyHint, shortcut, type ShortcutId, SHORTCUTS } from "./shortcuts";
import { menuItems } from "./testing";
import detailsStyles from "./DetailsCard.css?raw";
import appStyles from "./styles.css?raw";

// The diff view and xterm.js need a real browser. These tests cover only which panels exist and
// which ones are shown. ChatPanel.test.tsx covers the agent panel itself.
// DiffView.test.tsx covers the Diff panel. The stub only says which base it was last opened on,
// and which file the app asked it to show.
vi.mock("./DiffView", () => ({
  DiffView: ({ opened, reveal }: { opened?: { base: string; request: number }; reveal?: string | null }) => (
    <>
      <p>Diff opened on {opened ? `${opened.base} ${opened.request}` : "nothing"}</p>
      {reveal && <p>Diff on {reveal}</p>}
    </>
  ),
}));
// FilesPanel.test.tsx covers the Files panel. The stub only says which worktree it shows.
vi.mock("./FilesPanel", () => ({
  FilesPanel: ({ worktree }: { worktree: string }) => <p>Files of {worktree}</p>,
}));
// ProfilePanel.test.tsx covers the Profile panel. The stub only offers to start two run sessions.
vi.mock("./ProfilePanel", () => ({
  ProfilePanel: ({ onStartRun }: { onStartRun: (name: string) => void }) => (
    <div>
      {["dev", "tests"].map((name) => (
        <button key={name} onClick={() => onStartRun(name)}>
          Start {name}
        </button>
      ))}
    </div>
  ),
}));
vi.mock("./TerminalPanel", () => ({
  TerminalPanel: ({
    session,
    shown,
    ended,
    onFocus,
    onBlur,
  }: {
    session: string;
    shown: boolean;
    ended?: boolean;
    onFocus?: () => void;
    onBlur?: () => void;
  }) => (
    <section
      aria-label={`Panel ${session}`}
      hidden={!shown}
      data-ended={ended ? "true" : undefined}
      tabIndex={-1}
      onFocus={onFocus}
      onBlur={onBlur}
    />
  ),
}));

// The draft is the panel's own state, so a test sees whether the panel stayed mounted.
function FakeChatPanel({
  session,
  agent,
  shown,
  focusRequest,
  onFocus,
  onBlur,
  onOpenDiff,
  onChangeAgent,
  onLoginFailed,
}: {
  session: string;
  agent: Agent;
  shown: boolean;
  focusRequest: number;
  onFocus?: () => void;
  onBlur?: () => void;
  onOpenDiff(path?: string): void;
  onChangeAgent?: (agent: Agent) => void;
  onLoginFailed?: (failed: boolean) => void;
}) {
  const [draft, setDraft] = useState("");
  return (
    <section
      aria-label={`Panel ${session}`}
      hidden={!shown}
      tabIndex={-1}
      // Stands in for the real panel's focus effect: a new request takes focus once shown.
      ref={(panel) => {
        if (panel && shown && focusRequest > 0 && panel.dataset.focused !== String(focusRequest)) {
          panel.dataset.focused = String(focusRequest);
          panel.focus();
        }
      }}
      onFocus={onFocus}
      onBlur={onBlur}
    >
      <b>{agent}</b>
      {/* The real panel's text column. */}
      <div className="thread" />
      <input aria-label="Draft" value={draft} onChange={(event) => setDraft(event.target.value)} />
      <button onClick={() => onOpenDiff()}>Review diff</button>
      <button onClick={() => onOpenDiff("b.rs")}>Open b.rs in the diff</button>
      <button onClick={() => onChangeAgent?.("Codex")}>Switch to Codex</button>
      {/* Stands in for the real panel's latest turn failing to log in, and a later one working. */}
      <button onClick={() => onLoginFailed?.(true)}>Fail to log in</button>
      <button onClick={() => onLoginFailed?.(false)}>Finish a turn</button>
    </section>
  );
}

vi.mock("./ChatPanel", () => ({ ChatPanel: FakeChatPanel }));

// jsdom has no matchMedia. Tests set the OS color scheme here.
let systemDark = true;
const schemeListeners = new Set<() => void>();

function setSystemDark(dark: boolean) {
  systemDark = dark;
  act(() => schemeListeners.forEach((listener) => listener()));
}

// jsdom lays nothing out. Tests give an element a box here, by a selector it matches, and
// `relayout` tells every ResizeObserver that sizes changed.
let boxes: [string, { left: number; right: number; top: number; bottom: number }][] = [];
// A chat area with room for the Details column beside one or two agent panels.
const WIDE_LANE = { left: 300, right: 1240, top: 52, bottom: 860 };
const resizeObservers = new Set<() => void>();

class FakeResizeObserver {
  callback: () => void;
  constructor(callback: () => void) {
    this.callback = callback;
  }
  // As a browser does, it reports once soon after it starts to watch.
  observe() {
    resizeObservers.add(this.callback);
    queueMicrotask(() => resizeObservers.has(this.callback) && this.callback());
  }
  disconnect() {
    resizeObservers.delete(this.callback);
  }
}

function relayout(next: typeof boxes) {
  boxes = next;
  act(() => resizeObservers.forEach((callback) => callback()));
}

beforeEach(() => {
  boxes = [[".chat-area", WIDE_LANE]];
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    const box = boxes.find(([selector]) => this.matches(selector))?.[1] ?? { left: 0, right: 0, top: 0, bottom: 0 };
    return { ...box, x: box.left, y: box.top, width: box.right - box.left, height: box.bottom - box.top } as DOMRect;
  });
  vi.stubGlobal("matchMedia", (media: string) => ({
    get matches() {
      return media === "(prefers-color-scheme: dark)" && systemDark;
    },
    media,
    addEventListener: (_: string, listener: () => void) => schemeListeners.add(listener),
    removeEventListener: (_: string, listener: () => void) => schemeListeners.delete(listener),
  }));
});

afterEach(() => {
  cleanup();
  boxes = [];
  resizeObservers.clear();
  localStorage.clear();
  systemDark = true;
  schemeListeners.clear();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const main = "/code/grove";
const feature = "/code/grove-wt/feat-36";

async function trackedBridge() {
  const bridge = fakeBridge({
    [main]: [
      { path: main, branch: "main", isMain: true },
      { path: feature, branch: "feat-36", isMain: false },
    ],
  });
  await bridge.addRepo(main);
  return bridge;
}

// Waits for the worktree's workspace, which says which panels it shows.
async function select(branch: string) {
  const row = await screen.findByRole("button", { name: branch });
  await act(async () => fireEvent.click(row));
}

// The sidebar row of the `nth` agent session on `branch`, in the order they started.
function sessionRow(branch: string, nth = 0): HTMLElement | undefined {
  return [...document.querySelectorAll<HTMLElement>(".session-row")].filter(
    (row) => row.querySelector(".session-branch")?.textContent === branch,
  )[nth];
}

// Waits for the sidebar to list the session, which it does once the core says its list changed.
async function findSessionRow(branch: string, nth = 0): Promise<HTMLElement> {
  await vi.waitFor(() => expect(sessionRow(branch, nth)).toBeTruthy());
  return sessionRow(branch, nth)!;
}

// The state the status dot on a session's row names. null when it has none, as for Idle.
function rowState(branch: string, nth = 0) {
  const row = sessionRow(branch, nth)?.closest(".session-item");
  return row?.querySelector(".status-dot")?.getAttribute("aria-label") ?? null;
}

// The selected worktree's name, as the title bar shows it.
function selectedWorktree() {
  return document.querySelector(".crumbs b")?.textContent ?? null;
}

// Starts another agent session in the worktree, as Split or a subagent would, outside the sidebar.
async function startAnother(bridge: FakeBridge, worktree: string) {
  await act(async () => void (await bridge.spawnAgent(worktree, "Claude", null)));
}

test("opening a shell starts one in the selected worktree and shows its panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  fireEvent.click(screen.getByRole("button", { name: "Open shell" }));

  expect(await screen.findByRole("region", { name: "Panel 0" })).toBeTruthy();
  expect(bridge.spawned).toEqual([{ session: "0", worktree: feature, cols: 80, rows: 24 }]);
});

test("a double click opens one shell", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  const open = screen.getByRole("button", { name: "Open shell" });
  fireEvent.click(open);
  fireEvent.click(open);

  await screen.findByRole("button", { name: "Close shell" });
  expect(bridge.spawned).toHaveLength(1);
});

test("a worktree's shell stays open, hidden, while another worktree is selected", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  fireEvent.click(screen.getByRole("button", { name: "Open shell" }));
  await screen.findByRole("region", { name: "Panel 0" });

  await select("main (main worktree)");

  const panel = screen.getByLabelText("Panel 0");
  expect(panel.hidden).toBe(true);
  expect(screen.getByRole("button", { name: "Open shell" })).toBeTruthy();

  await select("feat-36");

  expect(panel.hidden).toBe(false);
  expect(screen.queryByRole("button", { name: "Open shell" })).toBeNull();
});

// As after a restart, which brings the shell back. The core runs one shell per worktree.
test("a worktree's running shell shows in its terminal panel without Open shell", async () => {
  const bridge = await trackedBridge();
  const shell = await bridge.spawnShell(feature, 80, 24);
  render(<App bridge={bridge} />);

  await select("feat-36");

  expect((await screen.findByLabelText(`Panel ${shell}`)).hidden).toBe(false);
  expect(screen.getByRole("button", { name: "Close shell" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Open shell" })).toBeNull();
  expect(bridge.spawned).toHaveLength(1);
});

test("a shell that fails to open shows its error only on its own worktree", async () => {
  const bridge = await trackedBridge();
  vi.spyOn(bridge, "spawnShell").mockRejectedValueOnce(new Error("out of ptys"));
  render(<App bridge={bridge} />);
  await select("feat-36");
  fireEvent.click(screen.getByRole("button", { name: "Open shell" }));
  expect((await screen.findByRole("alert")).textContent).toBe("Error: out of ptys");

  await select("main (main worktree)");

  expect(screen.queryByRole("alert")).toBeNull();

  await select("feat-36");

  expect(screen.getByRole("alert").textContent).toBe("Error: out of ptys");
});

test("closing a shell kills its session and removes its panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  fireEvent.click(screen.getByRole("button", { name: "Open shell" }));

  fireEvent.click(await screen.findByRole("button", { name: "Close shell" }));

  expect(bridge.killed).toEqual(["0"]);
  expect(screen.queryByLabelText("Panel 0")).toBeNull();
  expect(screen.getByRole("button", { name: "Open shell" })).toBeTruthy();
});

test("closing a shell that already exited shows no error", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  fireEvent.click(screen.getByRole("button", { name: "Open shell" }));
  const close = await screen.findByRole("button", { name: "Close shell" });
  await bridge.kill("0");

  fireEvent.click(close);

  await screen.findByRole("button", { name: "Open shell" });
  expect(screen.queryByRole("alert")).toBeNull();
});

// Picks `place` in the sidebar's New session picker. New session stays disabled until the projects
// load, and a click on it before then does nothing, so wait for it to be enabled.
async function pickPlace(place: string) {
  const newSession = await screen.findByRole("button", { name: "New session" });
  await waitFor(() => expect(newSession).toHaveProperty("disabled", false));
  fireEvent.click(newSession);
  await act(async () => fireEvent.click(screen.getByRole("menuitem", { name: place })));
}

test("New session in the local checkout starts a session there, selects it and puts focus in its panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  await pickPlace("Local checkout");

  const panel = await screen.findByRole("region", { name: "Panel 0" });
  expect(panel.hidden).toBe(false);
  expect(bridge.spawnedAgents).toEqual([{ session: "0", worktree: main, agent: "Claude" }]);
  expect(selectedWorktree()).toBe("main");
  expect((await findSessionRow("main")).getAttribute("aria-current")).toBe("true");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Panel 0")));
  expect(bridge.focused).toEqual(["0"]);
});

test("New session in an existing worktree starts a session there, and picking it again starts a second one", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await screen.findByRole("button", { name: "feat-36" });

  await pickPlace("feat-36");
  await screen.findByRole("region", { name: "Panel 0" });
  await findSessionRow("feat-36");
  await pickPlace("feat-36");

  await screen.findByRole("region", { name: "Panel 1" });
  expect(bridge.spawnedAgents).toEqual([
    { session: "0", worktree: feature, agent: "Claude" },
    { session: "1", worktree: feature, agent: "Claude" },
  ]);
  expect(selectedWorktree()).toBe("feat-36");
  expect((await findSessionRow("feat-36", 1)).getAttribute("aria-current")).toBe("true");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Panel 1")));
  // Each session has one panel.
  expect(screen.getAllByRole("region", { name: /^Panel \d+$/ }).map((panel) => panel.ariaLabel)).toEqual([
    "Panel 0",
    "Panel 1",
  ]);
});

test("starting Claude Code in a worktree opens its panel, and the session's row takes the worktree's place", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  startAgent();

  const panel = await screen.findByRole("region", { name: "Panel 0" });
  expect(panel.hidden).toBe(false);
  expect(bridge.spawnedAgents).toEqual([
    { session: "0", worktree: feature, agent: "Claude" },
  ]);
  expect(selectedWorktree()).toBe("feat-36");
  expect((await findSessionRow("feat-36")).getAttribute("aria-current")).toBe("true");
  expect(rowState("feat-36")).toBeNull();
  expect(screen.queryByRole("button", { name: "feat-36" })).toBeNull();
});

test.each<[string, AgentState, StateSource]>([
  ["Working", { state: "Working" }, "Hook"],
  ["Approval", { state: "NeedsInput", kind: "Approval" }, "Hook"],
  ["Question", { state: "NeedsInput", kind: "Question" }, "Hook"],
  ["Done", { state: "Done" }, "Hook"],
  ["Idle", { state: "Idle" }, "Heuristic"],
  ["Failed", { state: "Failed" }, "Heuristic"],
  ["Exited", { state: "Exited" }, "Hook"],
])("a session's row shows its %s, without its source", async (word, state, source) => {
  const shown = word === "Idle" || word === "Exited" ? null : word;
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  const row = await findSessionRow("feat-36");

  act(() => bridge.agentStateChanged({ session: "0", ...state, source }));

  expect(rowState("feat-36")).toBe(shown);
  expect(row.closest(".session-item")!.textContent).not.toMatch(source);
});

test("focusing a Done agent session's panel calls focus, and its row goes to Idle, with no dot", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  const panel = await screen.findByRole("region", { name: "Panel 0" });
  await findSessionRow("feat-36");
  act(() => bridge.agentStateChanged({ session: "0", state: "Done", source: "Hook" }));
  expect(rowState("feat-36")).toBe("Done");

  await act(async () => panel.focus());

  expect(bridge.focused).toEqual(["0"]);
  expect(rowState("feat-36")).toBeNull();
});

test("clicking an agent session's row selects its worktree, whose panels show side by side", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await startAnother(bridge, feature);
  await screen.findByRole("region", { name: "Panel 1" });
  await select("main (main worktree)");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(true);

  const row = await findSessionRow("feat-36");
  await act(async () => fireEvent.click(row));

  expect(selectedWorktree()).toBe("feat-36");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(screen.getByLabelText("Panel 1").hidden).toBe(false);
  expect(sessionRow("feat-36", 0)!.getAttribute("aria-current")).toBe("true");
  expect(sessionRow("feat-36", 1)!.getAttribute("aria-current")).toBe("true");
});

// The sidebar lists projects, then sessions (#70).

test("selecting a session of a worktree not opened since launch shows its worktree's chat area and panel area", async () => {
  const bridge = await trackedBridge();
  const session = await bridge.spawnAgent(feature, "Codex", null);
  render(<App bridge={bridge} />);

  const row = await findSessionRow("feat-36");
  await act(async () => fireEvent.click(row));

  expect(selectedWorktree()).toBe("feat-36");
  const panel = screen.getByRole("region", { name: `Panel ${session}` });
  expect(panel.hidden).toBe(false);
  expect(within(panel).getByText("Codex")).toBeTruthy();
  expect(document.activeElement).toBe(panel);
  expect(screen.getByRole("tabpanel", { name: "Files" }).textContent).toBe(`Files of ${feature}`);
});

test("selecting an ended session resumes it, then shows and focuses its panel", async () => {
  const bridge = await trackedBridge();
  // It ended before the window opened, as after a restart.
  const session = await bridge.spawnAgent(feature, "Claude", null);
  bridge.exit(session);
  render(<App bridge={bridge} />);
  const row = await findSessionRow("feat-36");
  expect(row.closest(".session-item")!.classList.contains("dimmed")).toBe(true);

  await act(async () => fireEvent.click(row));

  expect((await bridge.agentSessions())[0].ended).toBe(false);
  const panel = await screen.findByRole("region", { name: `Panel ${session}` });
  expect(panel.hidden).toBe(false);
  expect(document.activeElement).toBe(panel);
  await waitFor(() => expect(sessionRow("feat-36")!.closest(".session-item")!.classList.contains("dimmed")).toBe(false));
});

test("deleting a shown session drops its panel, and its worktree shows the empty state", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });

  await deleteSession(await findSessionRow("feat-36"));

  await waitFor(() => expect(screen.queryByRole("region", { name: "Panel 0" })).toBeNull());
  expect(sessionRow("feat-36")).toBeUndefined();
  expect(await screen.findByRole("button", { name: "feat-36" })).toBeTruthy();
  expect(selectedWorktree()).toBe("feat-36");
});

// The window keeps the panel of a session that is not running, so the delete must drop it too.
test("deleting a shown session that ended drops its panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  bridge.exit("0");
  await waitFor(() => expect(sessionRow("feat-36")!.closest(".session-item")!.classList.contains("dimmed")).toBe(true));
  expect(screen.getByRole("region", { name: "Panel 0" })).toBeTruthy();

  await deleteSession(sessionRow("feat-36")!);

  await waitFor(() => expect(screen.queryByRole("region", { name: "Panel 0" })).toBeNull());
  expect(sessionRow("feat-36")).toBeUndefined();
  expect(await screen.findByRole("button", { name: "feat-36" })).toBeTruthy();
});

test("selecting a worktree with no session shows the empty state with Start session", async () => {
  const bridge = await trackedBridge();
  await bridge.spawnAgent(main, "Claude", null);
  render(<App bridge={bridge} />);
  await findSessionRow("main");

  await select("feat-36");

  const chat = screen.getByRole("main");
  expect(within(chat).getByText("No agent session yet")).toBeTruthy();
  await act(async () => fireEvent.click(within(chat).getByRole("button", { name: "Start session" })));
  expect(bridge.spawnedAgents.at(-1)).toEqual({ session: "1", worktree: feature, agent: "Claude" });
  await findSessionRow("feat-36");
});

test("a double click starts one agent session", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  startAgent();
  startAgent();

  await screen.findByRole("region", { name: "Panel 0" });
  expect(bridge.spawnedAgents).toHaveLength(1);
});

// What a Dock launch shows when the login shell's PATH has no `claude`.
test("an agent session that fails to start shows the core's error and opens no panel", async () => {
  const bridge = await trackedBridge();
  const error = "could not start the session's command: Unable to spawn claude";
  vi.spyOn(bridge, "spawnAgent").mockRejectedValueOnce(error);
  render(<App bridge={bridge} />);
  await select("feat-36");

  startAgent();

  expect((await screen.findByRole("alert")).textContent).toBe(error);
  expect(screen.queryByLabelText(/^Panel \d/)).toBeNull();
  expect(screen.queryByRole("button", { name: /^Claude Code/ })).toBeNull();
});

test("a turn that ends while its panel has focus goes straight to Idle", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  const panel = await screen.findByRole("region", { name: "Panel 0" });
  await act(async () => panel.focus());
  act(() => bridge.agentStateChanged({ session: "0", state: "Working", source: "Hook" }));

  await act(async () => bridge.agentStateChanged({ session: "0", state: "Done", source: "Hook" }));

  expect(bridge.focused).toEqual(["0", "0"]);
  await findSessionRow("feat-36");
  expect(rowState("feat-36")).toBeNull();
});

test("a turn that ends after focus has left its panel stays Done", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  const panel = await screen.findByRole("region", { name: "Panel 0" });
  await act(async () => panel.focus());
  const row = await findSessionRow("feat-36");
  act(() => row.focus());

  await act(async () => bridge.agentStateChanged({ session: "0", state: "Done", source: "Hook" }));

  expect(bridge.focused).toEqual(["0"]);
  expect(rowState("feat-36")).toBe("Done");
});

function splitter(name: string) {
  return screen.getByRole("separator", { name });
}

function width(name: string) {
  return Number(splitter(name).getAttribute("aria-valuenow"));
}

function pressed(name: string) {
  return screen.getByRole("button", { name }).getAttribute("aria-pressed");
}

function drag(name: string, from: number, to: number) {
  fireEvent.pointerDown(splitter(name), { clientX: from, button: 0 });
  fireEvent.pointerMove(window, { clientX: to });
  fireEvent.pointerUp(window, { clientX: to });
}

test("the sidebar switch hides the sidebar from the sidebar's top row and shows it again from the title bar", async () => {
  render(<App bridge={await trackedBridge()} />);
  const sidebar = screen.getByRole("complementary", { name: "Worktrees" });
  const inSidebar = within(sidebar).getByRole("button", { name: "Sidebar" });
  expect(inSidebar.getAttribute("aria-pressed")).toBe("true");
  // One switch at a time: the title bar has none while the sidebar is open.
  expect(within(screen.getByRole("banner")).queryByRole("button", { name: "Sidebar" })).toBeNull();

  fireEvent.click(inSidebar);

  expect(screen.queryByRole("complementary", { name: "Worktrees" })).toBeNull();
  expect(screen.queryByRole("separator", { name: "Resize sidebar" })).toBeNull();
  const inTitleBar = within(screen.getByRole("banner")).getByRole("button", { name: "Sidebar" });
  expect(inTitleBar.getAttribute("aria-pressed")).toBe("false");
  // The switch clicked went away, so focus moves to the one that took its place.
  expect(document.activeElement).toBe(inTitleBar);

  fireEvent.click(inTitleBar);

  expect(await screen.findByRole("button", { name: "feat-36" })).toBeTruthy();
  expect(within(screen.getByRole("banner")).queryByRole("button", { name: "Sidebar" })).toBeNull();
  const shown = screen.getByRole("complementary", { name: "Worktrees" });
  expect(document.activeElement).toBe(within(shown).getByRole("button", { name: "Sidebar" }));
});

// Tauri drags the window from a `deep` drag region, except from what can be clicked in it.
test("the window's top rows drag the window, and their buttons do not", async () => {
  const { container } = render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  const rows = [...container.querySelectorAll("[data-tauri-drag-region]")];

  expect(rows).toHaveLength(3);
  expect(rows).toContain(container.querySelector(".sidebar-top"));
  expect(rows).toContain(screen.getByRole("banner"));
  expect(rows).toContain(container.querySelector(".ui-panel-tabs"));
  for (const row of rows) {
    expect(row.getAttribute("data-tauri-drag-region")).toBe("deep");
    expect(within(row as HTMLElement).getAllByRole("button").length).toBeGreaterThan(0);
    expect(row.querySelectorAll("button[data-tauri-drag-region]")).toHaveLength(0);
  }
});

test("in full screen the top rows keep no room for the window controls", async () => {
  const bridge = await trackedBridge();
  const { container } = render(<App bridge={bridge} />);
  const app = container.querySelector(".app")!;
  expect(app.classList.contains("fullscreen")).toBe(false);

  act(() => bridge.setFullscreen(true));

  expect(app.classList.contains("fullscreen")).toBe(true);

  act(() => bridge.setFullscreen(false));

  expect(app.classList.contains("fullscreen")).toBe(false);
});

test("Cmd-K with the sidebar collapsed shows the sidebar and puts focus in Search", async () => {
  render(<App bridge={await trackedBridge()} />);
  fireEvent.click(screen.getByRole("button", { name: "Sidebar" }));

  fireEvent.keyDown(document.body, { key: "k", metaKey: true });

  const sidebar = screen.getByRole("complementary", { name: "Worktrees" });
  expect(document.activeElement).toBe(within(sidebar).getByRole("searchbox", { name: "Search" }));
  expect(screen.getByRole("button", { name: "Sidebar" }).getAttribute("aria-pressed")).toBe("true");
});

// Presses a shortcut by the keys the list gives it, or those keys with Ctrl for Cmd.
async function pressShortcut(id: ShortcutId, { target = document.body, ctrlForCmd = false } = {}) {
  const keys = shortcut(id).keys.split("+");
  const cmd = keys.includes("Meta");
  await act(async () =>
    fireEvent.keyDown(target, {
      key: keys.at(-1)!.toLowerCase(),
      metaKey: cmd && !ctrlForCmd,
      ctrlKey: cmd && ctrlForCmd,
      shiftKey: keys.includes("Shift"),
      altKey: keys.includes("Alt"),
    }),
  );
}

function searchBox() {
  return within(screen.getByRole("complementary", { name: "Worktrees" })).getByRole("searchbox", { name: "Search" });
}

test("the list's keys for New session and Search do what the sidebar's controls do", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");

  await pressShortcut("newSession");
  const picker = screen.getByRole("menu", { name: "New session" });
  expect(document.activeElement).toBe(within(picker).getByRole("menuitem", { name: "Local checkout" }));
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();

  await pressShortcut("search");
  expect(document.activeElement).toBe(searchBox());
});

test("Cmd-/ opens Keyboard shortcuts, which shows every shortcut in the list under its group", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");

  await pressShortcut("showShortcuts");

  const sheet = screen.getByRole("dialog", { name: "Keyboard shortcuts" });
  for (const shortcut of SHORTCUTS) {
    const group = within(sheet).getByRole("region", { name: shortcut.group });
    const row = within(group).getByText(shortcut.label).closest("div")!;
    expect(row.querySelector("kbd")?.textContent).toBe(keyHint(shortcut.keys));
  }

  fireEvent.keyDown(sheet, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("no shortcut fires while a dialog is open", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await act(async () =>
    bridge.agentStateChanged({ session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" }),
  );
  await select("main (main worktree)");
  await pressShortcut("showShortcuts");
  const sheet = screen.getByRole("dialog", { name: "Keyboard shortcuts" });

  await pressShortcut("jump", { target: sheet });
  await pressShortcut("newSession", { target: sheet });
  await pressShortcut("search", { target: sheet });

  expect(bridge.focused).toEqual([]);
  expect(screen.getByRole("button", { name: "main (main worktree)" }).getAttribute("aria-current")).toBe("true");
  expect(screen.getAllByRole("dialog").map((dialog) => dialog.getAttribute("aria-label"))).toEqual([
    "Keyboard shortcuts",
  ]);
  expect(screen.queryByRole("menu")).toBeNull();
  expect(document.activeElement).not.toBe(searchBox());
});

test("a terminal keeps its Ctrl keys and the Cmd keys it takes, and Cmd-J still jumps from it", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await act(async () =>
    bridge.agentStateChanged({ session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" }),
  );
  await select("main (main worktree)");
  fireEvent.click(screen.getByRole("button", { name: "Open shell" }));
  const terminal = await screen.findByLabelText(`Panel ${bridge.spawned[0].session}`);
  terminal.focus();

  // Plain Ctrl-K, Ctrl-N, Ctrl-J and Ctrl-/ go to the shell, as they would in any terminal.
  for (const shortcut of SHORTCUTS) await pressShortcut(shortcut.id, { target: terminal, ctrlForCmd: true });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByRole("menu")).toBeNull();
  expect(document.activeElement).toBe(terminal);
  expect(bridge.focused).toEqual([]);

  // A Cmd key the focused terminal or editor takes for itself stays with it.
  terminal.addEventListener("keydown", (event) => event.preventDefault());
  await pressShortcut("newSession", { target: terminal });
  await pressShortcut("search", { target: terminal });
  await pressShortcut("showShortcuts", { target: terminal });
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(screen.queryByRole("menu")).toBeNull();
  expect(document.activeElement).toBe(terminal);

  // Cmd-J comes first, so it jumps even then.
  await pressShortcut("jump", { target: terminal });
  expect(bridge.focused).toEqual(["0"]);
});

test("the title bar sits over the chat lane, and the panel area runs the window's full height beside it", async () => {
  const style = document.createElement("style");
  style.textContent = appStyles;
  document.head.append(style);
  onTestFinished(() => style.remove());
  const { container } = render(<App bridge={await trackedBridge()} />);
  const box = (selector: string) => {
    const found = getComputedStyle(container.querySelector(selector)!);
    return [found.gridRow, found.gridColumn];
  };

  expect(box(".window-top")).toEqual(["1", "3"]);
  expect(box(".chat-area")).toEqual(["2", "3"]);
  expect(box(".sidebar-region")).toEqual(["1 / -1", "1"]);
  expect(box(".split-after")).toEqual(["1 / -1", "4"]);
  expect(box(".panel-area")).toEqual(["1 / -1", "5"]);
});

test("the Panel area switch collapses the panel area and shows it again", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  expect(screen.getByRole("tab", { name: "Files", selected: true })).toBeTruthy();

  // The switch moves between the panel area's tab row and the title bar, so each click finds it again.
  fireEvent.click(screen.getByRole("button", { name: "Panel area" }));

  expect(pressed("Panel area")).toBe("false");
  expect(screen.queryByRole("region", { name: "Panel area" })).toBeNull();
  expect(screen.queryByRole("separator", { name: "Resize panel area" })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Panel area" }));

  expect(screen.getByRole("region", { name: "Panel area" })).toBeTruthy();
});

// A collapsed region measures 0x0, so its terminal is hidden and detaches instead.
test("collapsing the panel area hides the shell's terminal, and showing it again shows it", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  fireEvent.click(screen.getByRole("button", { name: "Open shell" }));
  const panel = await screen.findByRole("region", { name: "Panel 0" });

  fireEvent.click(screen.getByRole("button", { name: "Panel area" }));

  expect(panel.hidden).toBe(true);

  fireEvent.click(screen.getByRole("button", { name: "Panel area" }));

  expect(panel.hidden).toBe(false);
});

test("the chat area shows the selected worktree's agent panel", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  const chat = screen.getByRole("main");
  expect(within(chat).getByText("No agent session yet")).toBeTruthy();

  startAgent();

  expect(await within(chat).findByRole("region", { name: "Panel 0" })).toBeTruthy();
  expect(within(chat).queryByText("No agent session yet")).toBeNull();
});

test("collapsed regions stay collapsed after a reload", async () => {
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);
  fireEvent.click(screen.getByRole("button", { name: "Sidebar" }));
  fireEvent.click(screen.getByRole("button", { name: "Panel area" }));
  unmount();

  render(<App bridge={bridge} />);

  expect(pressed("Sidebar")).toBe("false");
  expect(pressed("Panel area")).toBe("false");
});

/** Makes the window `width` pixels wide until the test ends. */
function windowWidth(width: number) {
  const before = window.innerWidth;
  window.innerWidth = width;
  onTestFinished(() => {
    window.innerWidth = before;
  });
}

test("a resized sidebar and panel area keep their widths after a reload", async () => {
  windowWidth(1800);
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);
  const sidebar = width("Resize sidebar");
  const panels = width("Resize panel area");

  drag("Resize sidebar", 500, 530);
  drag("Resize panel area", 600, 650);

  expect(width("Resize sidebar")).toBe(sidebar + 30);
  expect(width("Resize panel area")).toBe(panels - 50);
  unmount();

  render(<App bridge={bridge} />);

  expect(width("Resize sidebar")).toBe(sidebar + 30);
  expect(width("Resize panel area")).toBe(panels - 50);
});

test("the panel area opens 720 px wide, and a width the user saved stays", async () => {
  windowWidth(1800);
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);

  expect(width("Resize panel area")).toBe(720);
  unmount();

  localStorage.setItem(
    "grove.layout",
    JSON.stringify({ sidebarOpen: true, panelsOpen: true, sidebarWidth: 256, panelsWidth: 400 }),
  );
  render(<App bridge={bridge} />);

  expect(width("Resize panel area")).toBe(400);
});

test("the arrow keys resize a region", async () => {
  render(<App bridge={await trackedBridge()} />);
  const sidebar = width("Resize sidebar");

  fireEvent.keyDown(splitter("Resize sidebar"), { key: "ArrowRight" });

  expect(width("Resize sidebar")).toBe(sidebar + 16);

  fireEvent.keyDown(splitter("Resize sidebar"), { key: "ArrowLeft" });

  expect(width("Resize sidebar")).toBe(sidebar);
});

// jsdom's window is 1024px wide. The chat area keeps at least 300px, and each splitter 5px.
test("no region drags below its minimum width, nor squeezes the chat area below its own", async () => {
  render(<App bridge={await trackedBridge()} />);

  drag("Resize sidebar", 300, -1000);
  expect(width("Resize sidebar")).toBe(200);
  drag("Resize sidebar", 300, 3000);
  expect(width("Resize sidebar")).toBe(380);

  drag("Resize panel area", 600, 3000);
  expect(width("Resize panel area")).toBe(300);
  drag("Resize panel area", 600, -3000);
  expect(width("Resize panel area")).toBe(1024 - 380 - 5 - 300 - 5);
});

test("the palette and theme switches set the window's tokens and survive a reload", async () => {
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);
  const root = document.documentElement;
  expect(root.dataset).toMatchObject({ palette: "default", theme: "dark" });

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  expect(pressed("Default")).toBe("true");
  fireEvent.click(screen.getByRole("button", { name: "Modern" }));
  fireEvent.click(screen.getByRole("button", { name: "Light" }));

  expect(root.dataset).toMatchObject({ palette: "modern", theme: "light" });
  expect(pressed("Modern")).toBe("true");
  expect(pressed("Light")).toBe("true");
  unmount();

  render(<App bridge={bridge} />);

  expect(root.dataset).toMatchObject({ palette: "modern", theme: "light" });
});

test("the System theme follows the OS color scheme as it changes", async () => {
  systemDark = false;
  render(<App bridge={await trackedBridge()} />);
  const root = document.documentElement;
  expect(root.dataset.theme).toBe("light");

  setSystemDark(true);
  expect(root.dataset.theme).toBe("dark");

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  fireEvent.click(screen.getByRole("button", { name: "Light" }));
  setSystemDark(false);
  setSystemDark(true);
  expect(root.dataset.theme).toBe("light");
});

test("a saved setting that is not valid falls back to the defaults", async () => {
  localStorage.setItem("grove.layout", "{not json");
  localStorage.setItem("grove.appearance", JSON.stringify({ palette: "solarized", theme: 3 }));

  render(<App bridge={await trackedBridge()} />);

  expect(pressed("Sidebar")).toBe("true");
  expect(width("Resize sidebar")).toBe(256);
  expect(document.documentElement.dataset).toMatchObject({ palette: "default", theme: "dark" });
});

test("a saved appearance with an unknown palette id keeps its theme and falls back to the default palette", async () => {
  localStorage.setItem("grove.appearance", JSON.stringify({ palette: "solarized", theme: "light" }));

  render(<App bridge={await trackedBridge()} />);
  fireEvent.click(screen.getByRole("button", { name: "Settings" }));

  expect(document.documentElement.dataset).toMatchObject({ palette: "default", theme: "light" });
  expect(pressed("Default")).toBe("true");
  expect(JSON.parse(localStorage.getItem("grove.appearance") ?? "null")).toEqual({
    palette: "default",
    theme: "light",
    appIcon: "auto",
  });
});

test("an agent session that fails to start shows its error beside an agent panel already shown", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  vi.spyOn(bridge, "spawnAgent").mockRejectedValueOnce("out of ptys");

  fireEvent.click(screen.getByRole("button", { name: "Split" }));

  expect((await screen.findByRole("alert")).textContent).toBe("out of ptys");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
});

// Storage can be full or blocked. The setting still applies until the window closes.
test("a setting that cannot be saved still applies", async () => {
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
    throw new DOMException("quota exceeded", "QuotaExceededError");
  });
  vi.spyOn(console, "warn").mockImplementation(() => {});
  render(<App bridge={await trackedBridge()} />);

  fireEvent.click(screen.getByRole("button", { name: "Sidebar" }));

  expect(pressed("Sidebar")).toBe("false");
  expect(console.warn).toHaveBeenCalled();
});

test("Escape closes the appearance menu and puts focus back on Settings", async () => {
  render(<App bridge={await trackedBridge()} />);
  const appearance = screen.getByRole("button", { name: "Settings" });
  fireEvent.click(appearance);
  const light = screen.getByRole("button", { name: "Light" });
  light.focus();

  fireEvent.keyDown(light, { key: "Escape" });

  expect(screen.queryByRole("group", { name: "Theme" })).toBeNull();
  expect(document.activeElement).toBe(appearance);
});

// Tab can leave the open menu. Escape then belongs to wherever focus went, such as a terminal.
test("Escape with focus outside the appearance menu closes it and leaves focus where it is", async () => {
  render(<App bridge={await trackedBridge()} />);
  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  const panelArea = screen.getByRole("button", { name: "Panel area" });
  panelArea.focus();

  fireEvent.keyDown(panelArea, { key: "Escape" });

  expect(screen.queryByRole("group", { name: "Theme" })).toBeNull();
  expect(document.activeElement).toBe(panelArea);
});

const loginShellWarning = {
  kind: "LoginShellEnv",
  reason: "/bin/zsh did not print its environment within 5s",
} as const;

// The core sends it while it starts, before the UI listens.
test("the login shell's warning from start is shown once, until it is dismissed", async () => {
  const bridge = await trackedBridge();
  bridge.warn(loginShellWarning);
  // StrictMode subscribes twice, and the warning comes to each subscription.
  render(
    <StrictMode>
      <App bridge={bridge} />
    </StrictMode>,
  );

  const warning = await screen.findByRole("status");
  expect(warning.textContent).toContain("could not read your login shell's environment");
  expect(warning.textContent).toContain(loginShellWarning.reason);
  expect(screen.getAllByRole("status")).toHaveLength(1);

  fireEvent.click(within(warning).getByRole("button", { name: "Dismiss" }));

  expect(screen.queryByRole("status")).toBeNull();
});

test("a session a restart could not bring back shows its worktree and the reason", async () => {
  const bridge = await trackedBridge();
  bridge.warn({ kind: "SessionNotRestored", worktree: feature, reason: "could not start claude: No such file" });
  render(<App bridge={bridge} />);

  const warning = await screen.findByRole("status");

  expect(warning.querySelector("p")!.textContent).toBe(
    `grove could not bring back a session in ${feature}, so it is gone: could not start claude: No such file.`,
  );
});

test("a warning the core sends while grove runs is shown", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);

  act(() => bridge.warn(loginShellWarning));

  expect(screen.getByRole("status").textContent).toContain(loginShellWarning.reason);
});

test("a file the chat opens in the diff opens the Diff panel on that file", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  const panel = await screen.findByRole("region", { name: "Panel 0" });
  fireEvent.click(screen.getByRole("button", { name: "Panel area" }));
  expect(screen.queryByRole("region", { name: "Panel area" })).toBeNull();

  fireEvent.click(within(panel).getByRole("button", { name: "Open b.rs in the diff" }));

  expect(screen.getByRole("region", { name: "Panel area" })).toBeTruthy();
  expect(screen.getByRole("tab", { name: "Diff", selected: true })).toBeTruthy();
  expect(await screen.findByText("Diff on b.rs")).toBeTruthy();
});

test("the files-changed card's Review diff shows the collapsed panel area", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  fireEvent.click(screen.getByRole("button", { name: "Panel area" }));
  expect(screen.queryByRole("region", { name: "Panel area" })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Review diff" }));

  expect(screen.getByRole("region", { name: "Panel area" })).toBeTruthy();
  expect(screen.getByRole("tab", { name: "Diff", selected: true })).toBeTruthy();
});

test("the panel area has a Files tab and a Diff tab", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  expect(screen.getByRole("tabpanel", { name: "Files" }).textContent).toBe(`Files of ${feature}`);
  expect(screen.queryByRole("tabpanel", { name: "Diff" })).toBeNull();

  fireEvent.click(screen.getByRole("tab", { name: "Diff" }));

  expect(screen.getByRole("tab", { name: "Diff", selected: true })).toBeTruthy();
  expect(screen.getByRole("tabpanel", { name: "Diff" })).toBeTruthy();
  expect(screen.queryByRole("tabpanel", { name: "Files" })).toBeNull();
});

test("arrow keys move between the panel tabs, and only the selected one is in the Tab order", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  const files = screen.getByRole("tab", { name: "Files" });
  const diff = screen.getByRole("tab", { name: "Diff" });
  expect([files.tabIndex, diff.tabIndex]).toEqual([0, -1]);
  expect(files.getAttribute("aria-controls")).toBe(screen.getByRole("tabpanel", { name: "Files" }).id);

  fireEvent.keyDown(files, { key: "ArrowRight" });

  expect(screen.getByRole("tab", { name: "Diff", selected: true })).toBe(document.activeElement);
  expect([files.tabIndex, diff.tabIndex]).toEqual([-1, 0]);
  expect(diff.getAttribute("aria-controls")).toBe(screen.getByRole("tabpanel", { name: "Diff" }).id);

  fireEvent.keyDown(diff, { key: "ArrowRight" });
  expect(screen.getByRole("tab", { name: "Files", selected: true })).toBe(document.activeElement);
  fireEvent.keyDown(files, { key: "ArrowLeft" });
  expect(screen.getByRole("tab", { name: "Diff", selected: true })).toBe(document.activeElement);
});

// Its open files and unsaved edits must survive a look at another worktree.
test("a worktree's Files panel stays open, hidden, while another worktree is selected", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  const files = screen.getByText(`Files of ${feature}`);

  await select("main (main worktree)");

  expect(screen.getByText(`Files of ${feature}`)).toBe(files);
  expect(files.closest("[hidden]")).toBeTruthy();
  expect(screen.getByRole("tabpanel", { name: "Files" }).textContent).toContain(`Files of ${main}`);
});

function footer() {
  return screen.getByRole("group", { name: "Account and settings" });
}

test("the sidebar footer shows the selected agent session's agent's account, also one reported before the window opened", async () => {
  const bridge = await trackedBridge();
  const account = { email: "ann@example.com", organization: null, plan: "max", apiKeySource: null };
  bridge.accountChanged({ agent: "Claude", account });
  bridge.accountChanged({ agent: "Codex", account: null });
  render(<App bridge={bridge} />);
  await screen.findByRole("button", { name: "feat-36" });
  // No agent session is selected, so no agent's.
  expect(within(footer()).queryByRole("listitem")).toBeNull();

  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });

  expect(within(footer()).getByRole("listitem").textContent).toBe("ann@example.com · max plan");
  act(() => bridge.accountChanged({ agent: "Claude", account: null }));
  expect(within(footer()).getByRole("listitem").textContent).toBe("Not logged in");

  let codex = "";
  await act(async () => void (codex = await bridge.spawnAgent(main, "Codex", null)));
  await act(async () => fireEvent.click(await findSessionRow("main")));
  await screen.findByRole("region", { name: `Panel ${codex}` });
  expect(within(footer()).getByRole("listitem").title).toBe("Codex");
});

test("the footer never says Logged in after a turn of that agent failed to log in, until one works", async () => {
  const bridge = await trackedBridge();
  // What the agent reports for a login it has a token for, even an expired one.
  bridge.accountChanged({ agent: "Claude", account: { email: null, organization: null, plan: null, apiKeySource: null } });
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  const panel = await screen.findByRole("region", { name: "Panel 0" });
  expect(within(footer()).getByRole("listitem").textContent).toBe("Logged in");

  fireEvent.click(within(panel).getByRole("button", { name: "Fail to log in" }));

  expect(footer().textContent).not.toContain("Logged in");
  // The agent reports the same login again, as a new process does after a mode switch.
  act(() =>
    bridge.accountChanged({ agent: "Claude", account: { email: null, organization: null, plan: null, apiKeySource: null } }),
  );
  expect(footer().textContent).not.toContain("Logged in");
  // Another session of the same agent says nothing of its login.
  await startAnother(bridge, feature);
  await screen.findByRole("region", { name: "Panel 1" });
  await act(async () => fireEvent.click(await findSessionRow("feat-36", 1)));
  expect(footer().textContent).not.toContain("Logged in");

  fireEvent.click(within(panel).getByRole("button", { name: "Finish a turn" }));

  expect(within(footer()).getByRole("listitem").textContent).toBe("Logged in");
});

test("picking Codex before the first message starts a Codex session in the same panel and ends the Claude one", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  fireEvent.change(screen.getByRole("textbox", { name: "Draft" }), { target: { value: "add a test" } });

  fireEvent.click(screen.getByRole("button", { name: "Switch to Codex" }));

  const panel = await screen.findByRole("region", { name: "Panel 1" });
  expect(panel.hidden).toBe(false);
  expect(within(panel).getByText("Codex")).toBeTruthy();
  expect((within(panel).getByRole("textbox", { name: "Draft" }) as HTMLInputElement).value).toBe("add a test");
  expect(bridge.spawnedAgents.map((s) => s.agent)).toEqual(["Claude", "Codex"]);
  expect(bridge.killed).toEqual(["0"]);
  // The Codex session took the Claude one's place, which is deleted, so the worktree has one session.
  await waitFor(async () => expect((await bridge.agentSessions()).map((s) => s.agent)).toEqual(["Codex"]));
  await waitFor(() => expect(document.querySelectorAll(".session-row")).toHaveLength(1));
});

// The core starts the Codex session in a panel of its own. The switch drops that panel, even when
// the core's change is drawn before the switch ends.
test("a switch leaves one panel, with the Codex session, in the saved workspace", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  const spawnAgent = bridge.spawnAgent.bind(bridge);
  vi.spyOn(bridge, "spawnAgent").mockImplementation(async (worktree, agent, panel) => {
    const session = await spawnAgent(worktree, agent, panel);
    await new Promise((drawn) => setTimeout(drawn, 10));
    return session;
  });

  fireEvent.click(screen.getByRole("button", { name: "Switch to Codex" }));

  await waitFor(async () =>
    expect((await bridge.workspace(feature)).chat).toEqual([{ agent: "Codex", session: "1", shown: true }]),
  );
  expect(screen.getAllByRole("region", { name: /^Panel \d+$/ }).map((region) => region.getAttribute("aria-label"))).toEqual([
    "Panel 1",
  ]);
});

test("a focused panel that switches agent stays focused on the new session", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  const panel = await screen.findByRole("region", { name: "Panel 0" });
  act(() => panel.focus());

  fireEvent.click(screen.getByRole("button", { name: "Switch to Codex" }));
  await screen.findByRole("region", { name: "Panel 1" });
  await act(async () => bridge.agentStateChanged({ session: "1", state: "Done", source: "Protocol" }));

  expect(bridge.focused).toEqual(["0", "1", "1"]);
});

test("a failed switch keeps the Claude session and says why", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  vi.spyOn(bridge, "spawnAgent").mockRejectedValueOnce("codex is not on PATH");

  fireEvent.click(screen.getByRole("button", { name: "Switch to Codex" }));

  expect((await screen.findByRole("alert")).textContent).toBe("codex is not on PATH");
  expect(screen.getByRole("region", { name: "Panel 0" }).hidden).toBe(false);
  expect(bridge.killed).toEqual([]);
});

// Starts Claude Code from the selected worktree's menu.
function startAgent() {
  const row = screen.getAllByRole("button", { current: true }).find((button) => button.classList.contains("wt-name"))!;
  fireEvent.click(within(row.closest(".wt-row") as HTMLElement).getByRole("button", { name: /^Actions for / }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Start Claude Code" }));
}

function needsYou() {
  return screen.getByRole("button", { name: /^Needs you/ });
}

// The Needs you row's count: the attention queue's length.
function waiting() {
  return needsYou().querySelector(".count")?.textContent;
}

async function pressJump() {
  await pressShortcut("jump");
}

test("with nothing waiting Needs you counts 0, and the key does nothing", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await act(async () => bridge.agentStateChanged({ session: "0", state: "Working", source: "Protocol" }));
  await select("main (main worktree)");

  await pressJump();

  expect(waiting()).toBe("0");
  expect(selectedWorktree()).toBe("main");
  expect(bridge.focused).toEqual([]);
});

test("the key focuses the head of the attention queue's agent panel in another worktree, then the next once it is answered", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await select("main (main worktree)");
  startAgent();
  await screen.findByRole("region", { name: "Panel 1" });
  // Selecting the session focuses its panel.
  const row = await findSessionRow("feat-36");
  await act(async () => fireEvent.click(row));
  expect(bridge.focused).toEqual(["0"]);
  // The approval ranks first, though the question came first.
  await act(async () =>
    bridge.agentStateChanged({ session: "0", state: "NeedsInput", kind: "Question", source: "Protocol" }),
  );
  await act(async () =>
    bridge.agentStateChanged({ session: "1", state: "NeedsInput", kind: "Approval", source: "Protocol" }),
  );
  expect(waiting()).toBe("2");

  await pressJump();

  expect(selectedWorktree()).toBe("main");
  expect(screen.getByLabelText("Panel 1").hidden).toBe(false);
  expect(document.activeElement).toBe(screen.getByLabelText("Panel 1"));
  expect(bridge.focused).toEqual(["0", "1"]);

  // The user answers, and the agent goes back to work.
  await act(async () => bridge.agentStateChanged({ session: "1", state: "Working", source: "Protocol" }));
  expect(waiting()).toBe("1");
  await pressJump();

  expect(selectedWorktree()).toBe("feat-36");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(screen.getByLabelText("Panel 1").hidden).toBe(true);
  expect(document.activeElement).toBe(screen.getByLabelText("Panel 0"));
  expect(bridge.focused).toEqual(["0", "1", "0"]);
});

test("clicking Needs you jumps as the key does", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await act(async () => bridge.agentStateChanged({ session: "0", state: "Failed", source: "Protocol" }));
  await select("main (main worktree)");

  await act(async () => fireEvent.click(needsYou()));

  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(document.activeElement).toBe(screen.getByLabelText("Panel 0"));
  expect(bridge.focused).toEqual(["0"]);
});

test("jumping to a Done session's panel makes it Idle, which leaves the queue", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await act(async () => bridge.agentStateChanged({ session: "0", state: "Done", source: "Protocol" }));
  await select("main (main worktree)");
  expect(waiting()).toBe("1");

  await findSessionRow("feat-36");
  expect(rowState("feat-36")).toBe("Done");

  await pressJump();

  expect(rowState("feat-36")).toBeNull();
  expect(waiting()).toBe("0");
});

// The window has no panel for a session the core runs that no workspace names, such as a subagent.
test("the key opens and focuses a waiting session that has no panel", async () => {
  const bridge = await trackedBridge();
  const session = bridge.spawnAgentWithoutPanel(feature, "Claude");
  bridge.agentStateChanged({ session, state: "NeedsInput", kind: "Approval", source: "Protocol" });
  render(<App bridge={bridge} />);
  await expect.poll(waiting).toBe("1");

  await pressJump();

  expect(selectedWorktree()).toBe("feat-36");
  expect(screen.getByLabelText(`Panel ${session}`).hidden).toBe(false);
  expect(document.activeElement).toBe(screen.getByLabelText(`Panel ${session}`));
  expect(bridge.focused).toEqual([session]);
});

test("clicking a notification shows its session's worktree and focuses its agent panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await select("main (main worktree)");

  await act(async () => bridge.clickNotification({ session: "0", worktree: feature }));

  expect(selectedWorktree()).toBe("feat-36");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(document.activeElement).toBe(screen.getByLabelText("Panel 0"));
  expect(bridge.focused).toEqual(["0"]);
});

test("clicking a notification for a waiting session with no panel opens and focuses one", async () => {
  const bridge = await trackedBridge();
  const session = bridge.spawnAgentWithoutPanel(feature, "Claude");
  bridge.agentStateChanged({ session, state: "NeedsInput", kind: "Approval", source: "Protocol" });
  render(<App bridge={bridge} />);
  await expect.poll(waiting).toBe("1");

  await act(async () => bridge.clickNotification({ session, worktree: feature }));

  expect(screen.getByLabelText(`Panel ${session}`).hidden).toBe(false);
  expect(document.activeElement).toBe(screen.getByLabelText(`Panel ${session}`));
  expect(bridge.focused).toEqual([session]);
});

// Its worktree was removed, say, after the notification went out.
test("clicking a notification for a session grove no longer has changes nothing", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("main (main worktree)");

  await act(async () => bridge.clickNotification({ session: "7", worktree: feature }));

  expect(selectedWorktree()).toBe("main");
  expect(screen.queryByLabelText("Panel 7")).toBeNull();
  expect(bridge.focused).toEqual([]);
});

test("Needs you counts sessions that were waiting before the window opened", async () => {
  const bridge = await trackedBridge();
  const session = await bridge.spawnAgent(feature, "Claude", null);
  bridge.agentStateChanged({ session, state: "NeedsInput", kind: "Question", source: "Protocol" });

  render(<App bridge={bridge} />);

  await expect.poll(waiting).toBe("1");
});

test("Cmd-Q asks first while an agent session works, and quits at once after it is done", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });

  act(() => bridge.agentStateChanged({ session: "0", state: "Working", source: "Protocol" }));
  act(() => bridge.requestQuit());

  const dialog = await screen.findByRole("alertdialog", { name: "Quit grove?" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(bridge.quitCount()).toBe(0);

  act(() => bridge.agentStateChanged({ session: "0", state: "Done", source: "Protocol" }));
  act(() => bridge.requestQuit());

  await expect.poll(() => bridge.quitCount()).toBe(1);
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

test("New worktree… in New session makes a worktree, which opens selected with Claude Code started in it", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await pickPlace("New worktree…");
  const dialog = await screen.findByRole("dialog", { name: "New worktree" });
  fireEvent.change(within(dialog).getByLabelText("Branch name"), { target: { value: "feat-52" } });
  const profile = within(dialog).getByLabelText<HTMLSelectElement>("Profile");
  fireEvent.change(profile, { target: { value: "none" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  const panel = await screen.findByRole("region", { name: "Panel 0" });
  expect(panel.hidden).toBe(false);
  expect(bridge.spawnedAgents).toEqual([
    { session: "0", worktree: "/grovecode/worktrees/grove/feat-52", agent: "Claude" },
  ]);
  // The title bar names the worktree once the new worktree's list comes.
  await waitFor(() => expect(selectedWorktree()).toBe("feat-52"));
  expect((await findSessionRow("feat-52")).getAttribute("aria-current")).toBe("true");
});

test("New worktree… without Claude Code selects the worktree as soon as it exists, with Start session", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await pickPlace("New worktree…");
  const dialog = await screen.findByRole("dialog", { name: "New worktree" });
  fireEvent.change(within(dialog).getByLabelText("Branch name"), { target: { value: "feat-52" } });
  fireEvent.change(within(dialog).getByLabelText<HTMLSelectElement>("Profile"), { target: { value: "none" } });
  fireEvent.click(within(dialog).getByLabelText("Start Claude Code"));
  // Each frame from the create on, as the user sees it.
  const frames: string[] = [];
  const watch = new MutationObserver(() => frames.push(document.querySelector(".chat-area")?.textContent ?? ""));
  watch.observe(document.body, { subtree: true, childList: true, characterData: true });
  onTestFinished(() => watch.disconnect());

  await act(async () => fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" })));

  await waitFor(() => expect(selectedWorktree()).toBe("feat-52"));
  expect(screen.getByRole("button", { name: "feat-52" }).getAttribute("aria-current")).toBe("true");
  await screen.findByRole("heading", { name: "No agent session yet" });
  expect(screen.getByRole("button", { name: "Start session" })).toBeTruthy();
  expect(frames.filter((frame) => frame.includes("No worktree selected"))).toEqual([]);
  expect(bridge.spawnedAgents).toEqual([]);
});

// A repo whose Cargo.toml gets it the Rust crate profile, with its setup script.
async function rustRepo() {
  const bridge = fakeBridge({ [main]: [{ path: main, branch: "main", isMain: true }] }, {}, { [main]: { "Cargo.toml": "" } });
  await bridge.addRepo(main);
  return bridge;
}

async function createWorktree(branch: string) {
  await pickPlace("New worktree…");
  const dialog = await screen.findByRole("dialog", { name: "New worktree" });
  await within(dialog).findByText(/^Suggested/);
  fireEvent.change(within(dialog).getByLabelText("Branch name"), { target: { value: branch } });
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));
  await expect.poll(() => screen.queryByRole("dialog")).toBeNull();
}

test("Claude Code starts in a new worktree once its setup script succeeds, and the row shows setup until then", async () => {
  const bridge = await rustRepo();
  render(<App bridge={bridge} />);

  await createWorktree("feat-52");

  expect(await screen.findByRole("img", { name: "Setup running" })).toBeTruthy();
  expect(menuItems("feat-52")).not.toContain("Start Claude Code");
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(bridge.spawnedAgents).toEqual([]);

  act(() => bridge.finishSetup("/grovecode/worktrees/grove/feat-52", 0));

  await screen.findByRole("region", { name: "Panel 0" });
  expect(bridge.spawnedAgents).toEqual([
    { session: "0", worktree: "/grovecode/worktrees/grove/feat-52", agent: "Claude" },
  ]);
  expect(screen.queryByRole("img", { name: "Setup running" })).toBeNull();
  // The session's row takes the worktree's place.
  await findSessionRow("feat-52");
  expect(screen.queryByRole("button", { name: "feat-52" })).toBeNull();
});

// Issue #158: a start that waits for setup holds back none of the worktree's saves.
test("a change made while setup runs saves at once, and the session still gets its panel", async () => {
  const bridge = await rustRepo();
  const worktree = "/grovecode/worktrees/grove/feat-52";
  render(<App bridge={bridge} />);
  await createWorktree("feat-52");
  await screen.findByRole("img", { name: "Setup running" });

  fireEvent.click(screen.getByRole("button", { name: "Open a panel" }));
  fireEvent.click(within(screen.getByRole("menu", { name: "Open a panel" })).getByRole("menuitemcheckbox", { name: "Logs" }));

  await expect
    .poll(() => bridge.workspacesSet.some((set) => set.worktree === worktree && set.workspace.panels.includes("Logs")))
    .toBe(true);
  act(() => bridge.finishSetup(worktree, 0));
  await screen.findByRole("region", { name: "Panel 0" });
  await waitFor(async () => {
    const saved = await bridge.workspace(worktree);
    expect(saved.chat).toEqual([{ agent: "Claude", session: "0", shown: true }]);
    expect(saved.panels).toContain("Logs");
  });
});

test("a change saved over a session started after setup puts its panel back", async () => {
  const bridge = await rustRepo();
  const worktree = "/grovecode/worktrees/grove/feat-52";
  const spawnAfterSetup = bridge.spawnAgentAfterSetup.bind(bridge);
  let reply = () => {};
  vi.spyOn(bridge, "spawnAgentAfterSetup").mockImplementation(async (at, agent, panel) => {
    // The core has placed it, and its reply and event are still on the way.
    const session = await spawnAfterSetup(at, agent, panel);
    return new Promise((resolve) => (reply = () => resolve(session)));
  });
  render(<App bridge={bridge} />);
  await createWorktree("feat-52");
  await screen.findByRole("img", { name: "Setup running" });
  const release = bridge.holdWorkspaceChanges();
  act(() => bridge.finishSetup(worktree, 0));
  await expect.poll(() => bridge.spawnedAgents.length).toBe(1);

  fireEvent.click(screen.getByRole("button", { name: "Open a panel" }));
  fireEvent.click(within(screen.getByRole("menu", { name: "Open a panel" })).getByRole("menuitemcheckbox", { name: "Logs" }));
  await expect.poll(async () => (await bridge.workspace(worktree)).chat).toEqual([{ agent: "Claude", session: null, shown: true }]);
  await act(async () => reply());
  await act(async () => release());

  await waitFor(async () => {
    const saved = await bridge.workspace(worktree);
    expect(saved.chat).toEqual([{ agent: "Claude", session: "0", shown: true }]);
    expect(saved.panels).toContain("Logs");
  });
  expect(screen.getByRole("region", { name: "Panel 0" }).hidden).toBe(false);
});

// Issue #204: the session runs, so only the layout failed.
test("a session started after setup whose core copy then does not load shows as started, with a layout error", async () => {
  const bridge = await rustRepo();
  const worktree = "/grovecode/worktrees/grove/feat-52";
  const spawnAfterSetup = bridge.spawnAgentAfterSetup.bind(bridge);
  vi.spyOn(bridge, "spawnAgentAfterSetup").mockImplementation(async (at, agent, panel) => {
    const session = await spawnAfterSetup(at, agent, panel);
    vi.spyOn(bridge, "workspace").mockRejectedValueOnce("the state file is unreadable");
    return session;
  });
  render(<App bridge={bridge} />);
  await createWorktree("feat-52");
  await screen.findByRole("img", { name: "Setup running" });
  // The core's change comes after the start's own load.
  const release = bridge.holdWorkspaceChanges();

  act(() => bridge.finishSetup(worktree, 0));
  await expect.poll(() => screen.queryByRole("alert")?.textContent).toBe(
    "Could not load this worktree's layout: the state file is unreadable",
  );
  await act(async () => release());

  expect((await screen.findByRole("region", { name: "Panel 0" })).hidden).toBe(false);
  expect(bridge.spawnedAgents).toHaveLength(1);
  expect(screen.getByRole("alert").textContent).toBe(
    "Could not load this worktree's layout: the state file is unreadable",
  );
});

test("a session started after setup whose panel then does not save shows as started, with a layout error", async () => {
  const bridge = await rustRepo();
  const worktree = "/grovecode/worktrees/grove/feat-52";
  const spawnAfterSetup = bridge.spawnAgentAfterSetup.bind(bridge);
  let reply = () => {};
  vi.spyOn(bridge, "spawnAgentAfterSetup").mockImplementation(async (at, agent, panel) => {
    const session = await spawnAfterSetup(at, agent, panel);
    return new Promise((resolve) => (reply = () => resolve(session)));
  });
  render(<App bridge={bridge} />);
  await createWorktree("feat-52");
  await screen.findByRole("img", { name: "Setup running" });
  bridge.holdWorkspaceChanges();
  act(() => bridge.finishSetup(worktree, 0));
  await expect.poll(() => bridge.spawnedAgents.length).toBe(1);
  // A change saved meanwhile leaves out the session's panel, so the start saves it back.
  fireEvent.click(screen.getByRole("button", { name: "Open a panel" }));
  fireEvent.click(within(screen.getByRole("menu", { name: "Open a panel" })).getByRole("menuitemcheckbox", { name: "Logs" }));
  await expect.poll(async () => (await bridge.workspace(worktree)).chat).toEqual([{ agent: "Claude", session: null, shown: true }]);
  vi.spyOn(bridge, "setWorkspace").mockRejectedValueOnce("the state file is read-only");

  await act(async () => reply());

  await expect.poll(() => screen.queryByRole("alert")?.textContent).toBe(
    "Could not save this worktree's layout: the state file is read-only",
  );
  expect(screen.getByRole("region", { name: "Panel 0" }).hidden).toBe(false);
});

test("a setup script that fails shows on the row and in the chat area, and starts no agent session", async () => {
  const bridge = await rustRepo();
  render(<App bridge={bridge} />);
  await createWorktree("feat-52");
  await screen.findByRole("img", { name: "Setup running" });

  act(() => bridge.finishSetup("/grovecode/worktrees/grove/feat-52", 3));

  expect(await screen.findByText("Setup failed: exited with code 3")).toBeTruthy();
  expect(
    await screen.findByText(
      "the setup script in /grovecode/worktrees/grove/feat-52 exited with code 3, so no agent session started",
    ),
  ).toBeTruthy();
  expect(bridge.spawnedAgents).toEqual([]);
  expect(menuItems("feat-52")).toContain("Start Claude Code");
});

// Makes worktree feat-52 with no profile, so no setup script runs.
async function createFeat52({ startAgent }: { startAgent: boolean }) {
  await pickPlace("New worktree…");
  const dialog = await screen.findByRole("dialog", { name: "New worktree" });
  fireEvent.change(within(dialog).getByLabelText("Branch name"), { target: { value: "feat-52" } });
  fireEvent.change(within(dialog).getByLabelText("Profile"), { target: { value: "none" } });
  if (!startAgent) fireEvent.click(within(dialog).getByLabelText("Start Claude Code"));
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));
  await expect.poll(() => screen.queryByRole("dialog")).toBeNull();
}

// Clicks Delete in a worktree's dialog. Its row goes once the delete is done.
async function startDelete(name: string) {
  fireEvent.click(screen.getByRole("button", { name: `Actions for ${name}` }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Delete" }));
  const dialog = await screen.findByRole("dialog", { name: "Delete worktree" });
  fireEvent.click(await within(dialog).findByRole("button", { name: "Delete" }));
  return dialog;
}

async function deleteFeat52() {
  await startDelete("feat-52");
  await expect.poll(() => screen.queryByRole("button", { name: "feat-52" })).toBeNull();
}

// Deletes the session in the row's menu, once the dialog is confirmed.
async function deleteSession(row: HTMLElement) {
  fireEvent.click(within(row.closest<HTMLElement>(".session-item")!).getByRole("button", { name: /^Actions for / }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Delete session" }));
  const dialog = await screen.findByRole("dialog", { name: "Delete session" });
  await act(async () => fireEvent.click(within(dialog).getByRole("button", { name: "Delete session" })));
}

test("a worktree deleted and made again on the same branch starts with no agent session or shell", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await createFeat52({ startAgent: true });
  await screen.findByRole("region", { name: "Panel 0" });
  fireEvent.click(await screen.findByRole("button", { name: "Open shell" }));
  await screen.findByRole("button", { name: "Close shell" });
  // A worktree with a session has no row, so its session goes first.
  await deleteSession(await findSessionRow("feat-52"));

  await deleteFeat52();

  await createFeat52({ startAgent: false });

  expect(screen.getByRole("button", { name: "feat-52" }).getAttribute("aria-current")).toBe("true");
  expect(await screen.findByRole("button", { name: "Open shell" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Close shell" })).toBeNull();
  expect(sessionRow("feat-52")).toBeUndefined();
  expect(screen.queryByRole("region", { name: "Panel 0" })).toBeNull();
});

test("a layout that loads after its worktree was deleted does not reach the one made again at its path", async () => {
  const bridge = await trackedBridge();
  const workspace = bridge.workspace.bind(bridge);
  const path = "/grovecode/worktrees/grove/feat-52";
  const loads: ((layout: Workspace) => void)[] = [];
  vi.spyOn(bridge, "workspace").mockImplementation((worktree) =>
    worktree === path ? new Promise((resolve) => loads.push(resolve)) : workspace(worktree),
  );
  render(<App bridge={bridge} />);
  await createFeat52({ startAgent: false });
  await deleteFeat52();
  await createFeat52({ startAgent: false });
  await expect.poll(() => loads.length).toBe(2);
  const fresh = await workspace(path);

  await act(async () => loads[0]({ ...fresh, terminal: false }));
  await act(async () => loads[1](fresh));

  expect(screen.getByRole("button", { name: "Open shell" })).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a shell listed after its worktree was deleted does not reach the one made again at its path", async () => {
  const bridge = await trackedBridge();
  const sessions = bridge.sessions.bind(bridge);
  const path = "/grovecode/worktrees/grove/feat-52";
  const lists: (() => Promise<void>)[] = [];
  vi.spyOn(bridge, "sessions").mockImplementation((worktree) =>
    worktree === path
      ? new Promise((resolve) => lists.push(async () => resolve([{ session: "7", running: true, kind: "Shell" }])))
      : sessions(worktree),
  );
  render(<App bridge={bridge} />);
  await createFeat52({ startAgent: false });
  await deleteFeat52();
  await createFeat52({ startAgent: false });
  await expect.poll(() => lists.length).toBe(2);

  await act(async () => lists[0]());

  expect(screen.getByRole("button", { name: "Open shell" })).toBeTruthy();
  expect(screen.queryByLabelText("Panel 7")).toBeNull();
});

test("Cmd-Q asks first while a worktree is being deleted, even after its dialog was cancelled", async () => {
  const bridge = await trackedBridge();
  const deleteWorktree = bridge.deleteWorktree.bind(bridge);
  let finishDelete = async () => {};
  vi.spyOn(bridge, "deleteWorktree").mockImplementation(
    (...args) => new Promise((resolve) => (finishDelete = async () => resolve(await deleteWorktree(...args)))),
  );
  render(<App bridge={bridge} />);
  await screen.findByRole("button", { name: "feat-36" });
  const dialog = await startDelete("feat-36");
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

  act(() => bridge.requestQuit());

  const asking = await screen.findByRole("alertdialog", { name: "Quit grove?" });
  expect(within(asking).getByText("grove is still deleting feat-36. Quitting waits until it is done.")).toBeTruthy();
  fireEvent.click(within(asking).getByRole("button", { name: "Cancel" }));

  await act(async () => finishDelete());
  act(() => bridge.requestQuit());

  await expect.poll(() => bridge.quitCount()).toBe(1);
});

// Run sessions (#168).

const RUN_SESSIONS = `[[run_session]]\nname = "dev"\ncommand = "pnpm dev"\n\n[[run_session]]\nname = "tests"\ncommand = "cargo test"\n`;

// A tracked repo with the dev and tests run sessions, whose feat-36 worktree shows the Profile tab
// with its terminal panel closed.
async function runBridge() {
  const bridge = await trackedBridge();
  await bridge.setRepoOverridesText(main, RUN_SESSIONS);
  await bridge.setWorkspace(feature, { ...DEFAULT_WORKSPACE, panels: ["Profile"], selected: "Profile", terminal: false });
  return bridge;
}

const terminalTab = (name: string | RegExp) =>
  within(screen.getByRole("tablist", { name: "Terminal sessions" })).getByRole("tab", { name });

async function clickButton(name: string) {
  await act(async () => fireEvent.click(screen.getByRole("button", { name })));
}

test("Start in the Profile tab starts the run session in the worktree and shows its terminal in a tab", async () => {
  const bridge = await runBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  await clickButton("Start dev");

  expect(bridge.spawnedRuns).toEqual([{ session: "0", worktree: feature, name: "dev", cols: 80, rows: 24 }]);
  expect(terminalTab("dev running").getAttribute("aria-selected")).toBe("true");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(screen.getByLabelText("Panel 0").dataset.ended).toBeUndefined();
  expect(pressed("Terminal")).toBe("true");
});

test("the shell and each run session have their own tab in the terminal panel", async () => {
  const bridge = await runBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Start dev");

  await act(async () => fireEvent.click(terminalTab("Shell")));

  expect(screen.getByLabelText("Panel 0").hidden).toBe(true);
  await clickButton("Open shell");
  expect(screen.getByLabelText("Panel 1").hidden).toBe(false);

  await act(async () => fireEvent.click(terminalTab(/^dev/)));

  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(screen.getByLabelText("Panel 1").hidden).toBe(true);
});

test("Start on a run session that is running shows its tab and starts no other", async () => {
  const bridge = await runBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Start dev");
  await act(async () => fireEvent.click(terminalTab("Shell")));

  await clickButton("Start dev");

  expect(bridge.spawnedRuns).toHaveLength(1);
  expect(terminalTab("dev running").getAttribute("aria-selected")).toBe("true");
});

test("a run session that exits says stopped, keeps its terminal, and Start again starts it anew", async () => {
  const bridge = await runBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Start tests");

  act(() => bridge.exit("0"));

  expect(terminalTab("tests stopped")).toBeTruthy();
  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(screen.getByLabelText("Panel 0").dataset.ended).toBe("true");

  await clickButton("Start again");

  expect(bridge.spawnedRuns.map(({ session, name }) => ({ session, name }))).toEqual([
    { session: "0", name: "tests" },
    { session: "1", name: "tests" },
  ]);
  expect(terminalTab("tests running").getAttribute("aria-selected")).toBe("true");
  expect(screen.getByLabelText("Panel 1").hidden).toBe(false);
  expect(screen.queryByLabelText("Panel 0")).toBeNull();
});

test("a run session that fails to start says why on its own tab only", async () => {
  const bridge = await runBridge();
  vi.spyOn(bridge, "spawnRun").mockRejectedValueOnce(new Error("out of ptys"));
  render(<App bridge={bridge} />);
  await select("feat-36");

  await clickButton("Start dev");

  expect(terminalTab("dev stopped").getAttribute("aria-selected")).toBe("true");
  expect(screen.getByRole("alert").textContent).toBe("Error: out of ptys");
  // A run session that never started has no tab once another is shown.
  await act(async () => fireEvent.click(terminalTab("Shell")));
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByRole("tablist", { name: "Terminal sessions" })).toBeNull();

  await clickButton("Start dev");

  expect(terminalTab("dev running").getAttribute("aria-selected")).toBe("true");
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a run session starts while another is still starting", async () => {
  const bridge = await runBridge();
  const spawnRun = bridge.spawnRun.bind(bridge);
  let startDev = async () => {};
  vi.spyOn(bridge, "spawnRun").mockImplementation((worktree, name, cols, rows) =>
    name === "dev"
      ? new Promise((resolve) => (startDev = async () => resolve(await spawnRun(worktree, name, cols, rows))))
      : spawnRun(worktree, name, cols, rows),
  );
  render(<App bridge={bridge} />);
  await select("feat-36");

  await clickButton("Start dev");
  await clickButton("Start tests");
  await act(async () => startDev());

  expect(bridge.spawnedRuns.map(({ name }) => name).sort()).toEqual(["dev", "tests"]);
  expect(terminalTab("dev running")).toBeTruthy();
  expect(terminalTab("tests running")).toBeTruthy();
});


test("a listed run session that exits before the listing returns says stopped", async () => {
  const bridge = await runBridge();
  const { unmount } = render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Start tests");
  unmount();
  const sessions = bridge.sessions.bind(bridge);
  let reply = async () => {};
  vi.spyOn(bridge, "sessions").mockImplementation(async (worktree) => {
    // Listed while it still runs, but the reply arrives after it exits.
    const listed = await sessions(worktree);
    return new Promise((resolve) => (reply = async () => resolve(listed)));
  });
  render(<App bridge={bridge} />);
  await select("feat-36");

  act(() => bridge.exit("0"));
  await act(async () => reply());

  expect(terminalTab("tests stopped")).toBeTruthy();
});
test("a run session that exited comes back stopped, with its terminal, after the window reloads", async () => {
  const bridge = await runBridge();
  const { unmount } = render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Start tests");
  act(() => bridge.exit("0"));
  unmount();

  render(<App bridge={bridge} />);
  await select("feat-36");
  await act(async () => fireEvent.click(terminalTab("tests stopped")));

  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(screen.getByLabelText("Panel 0").dataset.ended).toBe("true");
});

test("a run session a restart brought back shows stopped in its own worktree's terminal panel", async () => {
  const bridge = await runBridge();
  const restored = bridge.restoreStoppedRun(feature, "dev");
  render(<App bridge={bridge} />);

  await select("main (main worktree)");
  expect(screen.queryByRole("tab", { name: /^dev/ })).toBeNull();
  await select("feat-36");
  await clickButton("Terminal");
  await act(async () => fireEvent.click(terminalTab("dev stopped")));

  expect(screen.getByLabelText(`Panel ${restored}`).dataset.ended).toBe("true");
  await clickButton("Start again");

  expect(bridge.spawnedRuns.map(({ worktree, name }) => ({ worktree, name }))).toEqual([{ worktree: feature, name: "dev" }]);
  expect(terminalTab("dev running")).toBeTruthy();
  expect((await bridge.sessions(feature)).filter((info) => info.kind === "Run")).toEqual([
    { session: bridge.spawnedRuns[0].session, running: true, kind: "Run", name: "dev" },
  ]);
});

test("a run session that exits before its start returns says stopped", async () => {
  const bridge = await runBridge();
  const spawnRun = bridge.spawnRun.bind(bridge);
  vi.spyOn(bridge, "spawnRun").mockImplementation(async (...args) => {
    const session = await spawnRun(...args);
    bridge.exit(session);
    return session;
  });
  render(<App bridge={bridge} />);
  await select("feat-36");

  await clickButton("Start tests");

  expect(terminalTab("tests stopped").getAttribute("aria-selected")).toBe("true");
});

test("a run session whose start returns after its worktree was deleted does not reach the one made again at its path", async () => {
  const bridge = await trackedBridge();
  await bridge.setRepoOverridesText(main, RUN_SESSIONS);
  const spawnRun = bridge.spawnRun.bind(bridge);
  let returnDev = () => {};
  vi.spyOn(bridge, "spawnRun").mockImplementation(async (...args) => {
    const session = await spawnRun(...args);
    await new Promise<void>((resolve) => (returnDev = resolve));
    return session;
  });
  render(<App bridge={bridge} />);
  await createFeat52({ startAgent: false });
  await clickButton("Open a panel");
  const menu = screen.getByRole("menu", { name: "Open a panel" });
  await act(async () => fireEvent.click(within(menu).getByRole("menuitemcheckbox", { name: "Profile" })));
  await clickButton("Start dev");
  await deleteFeat52();
  await createFeat52({ startAgent: false });

  await act(async () => returnDev());

  expect(screen.queryByRole("tablist", { name: "Terminal sessions" })).toBeNull();
});

test("removing a worktree keeps the run session start errors of another whose path ends with its", async () => {
  const bridge = fakeBridge({
    "/a": [
      { path: "/a", branch: "main", isMain: true },
      { path: "/b", branch: "b", isMain: false },
      { path: "/x /b", branch: "spaced", isMain: false },
    ],
  });
  await bridge.addRepo("/a");
  await bridge.setRepoOverridesText("/a", RUN_SESSIONS);
  await bridge.setWorkspace("/x /b", { ...DEFAULT_WORKSPACE, panels: ["Profile"], selected: "Profile" });
  vi.spyOn(bridge, "spawnRun").mockRejectedValueOnce("out of ptys");
  render(<App bridge={bridge} />);
  await select("spaced");
  await clickButton("Start dev");
  expect(screen.getByRole("alert").textContent).toBe("out of ptys");

  await startDelete("b");
  await expect.poll(() => screen.queryByRole("button", { name: "b" })).toBeNull();

  expect(screen.getByRole("alert").textContent).toBe("out of ptys");
});

// Details card (#243). DetailsCard.test.tsx covers each row.

const detailsCard = () => screen.getByRole("region", { name: "Details" });

test("the Details button opens the card, shows whether it is open, and closes it", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  expect(screen.getByRole("button", { name: "Details" })).toHaveProperty("disabled", true);
  await select("feat-36");
  const button = screen.getByRole("button", { name: "Details" });
  expect(button.getAttribute("aria-pressed")).toBe("false");

  await act(async () => fireEvent.click(button));

  expect(within(detailsCard()).getByTitle(feature)).toBeTruthy();
  expect(button.getAttribute("aria-pressed")).toBe("true");

  fireEvent.pointerDown(button);
  await act(async () => fireEvent.click(button));
  expect(screen.queryByRole("region", { name: "Details" })).toBeNull();
  expect(button.getAttribute("aria-pressed")).toBe("false");
});

test("Start and Stop in the Details card's scripts row follow the run session's state", async () => {
  const bridge = await runBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Details");
  const dev = () => within(detailsCard()).getByRole("listitem", { name: "Run dev" });

  await act(async () => fireEvent.click(within(dev()).getByRole("button", { name: "Start dev" })));

  expect(bridge.spawnedRuns.map(({ worktree, name }) => ({ worktree, name }))).toEqual([{ worktree: feature, name: "dev" }]);
  expect(within(dev()).getByText("Running")).toBeTruthy();

  await act(async () => fireEvent.click(within(dev()).getByRole("button", { name: "Stop dev" })));

  expect(bridge.killed).toEqual([bridge.spawnedRuns[0].session]);
  expect(within(dev()).getByText("Stopped")).toBeTruthy();
  expect(terminalTab("dev stopped")).toBeTruthy();
});

test("the Details card keeps the port of a running run session after the settings drop it", async () => {
  const bridge = await runBridge();
  await bridge.setRepoOverridesText(main, `[[run_session]]\nname = "dev"\ncommand = "pnpm dev"\nport = 5173\n`);
  render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Details");
  await act(async () => fireEvent.click(within(detailsCard()).getByRole("button", { name: "Start dev" })));
  await clickButton("Details");

  await act(async () => bridge.setRepoOverridesText(main, `[[run_session]]\nname = "web"\ncommand = "pnpm dev"\n`));
  await clickButton("Details");

  const dev = within(detailsCard()).getByRole("listitem", { name: "Run dev" });
  expect(within(dev).getByRole("button", { name: "Stop dev" })).toBeTruthy();
  const servers = within(detailsCard()).getByRole("listitem", { name: "Servers" });
  expect(within(servers).getByText(":5173 dev")).toBeTruthy();
});

test("the Details card's Changes row opens the Diff panel, and the panel area if it was closed", async () => {
  const bridge = fakeBridge(
    {
      [main]: [
        { path: main, branch: "main", isMain: true },
        { path: feature, branch: "feat-36", isMain: false },
      ],
    },
    { [feature]: { Head: [], MergeBase: [] } },
  );
  await bridge.addRepo(main);
  render(<App bridge={bridge} />);
  await select("feat-36");
  fireEvent.click(screen.getByRole("button", { name: "Panel area" }));
  await clickButton("Details");

  await act(async () => fireEvent.click(within(detailsCard()).getByRole("button", { name: /Changes/ })));

  expect(screen.getByRole("region", { name: "Panel area" })).toBeTruthy();
  expect(screen.getByRole("tab", { name: "Diff", selected: true })).toBeTruthy();
  expect(screen.getByText("Diff opened on MergeBase 1")).toBeTruthy();

  // Each click asks again, so a Diff the user switched to HEAD goes back to the merge base.
  await act(async () => fireEvent.click(within(detailsCard()).getByRole("button", { name: /Changes/ })));
  expect(screen.getByText("Diff opened on MergeBase 2")).toBeTruthy();
});

test("the Details card's Changes row opens the Diff on HEAD for a worktree with no base branch", async () => {
  const bridge = fakeBridge(
    { [main]: [{ path: main, branch: "main", isMain: true, baseBranch: null }] },
    { [main]: { Head: [], MergeBase: [] } },
  );
  await bridge.addRepo(main);
  render(<App bridge={bridge} />);
  await select("main (main worktree)");
  await clickButton("Details");

  await act(async () => fireEvent.click(within(detailsCard()).getByRole("button", { name: /Changes/ })));

  expect(screen.getByText("Diff opened on Head 1")).toBeTruthy();
});

test("the Details card's Add run session row opens the Profile panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Details");

  await act(async () => fireEvent.click(within(detailsCard()).getByRole("button", { name: "Add run session" })));

  expect(screen.getByRole("tab", { name: "Profile", selected: true })).toBeTruthy();
});

test("a run session's tab shows at once while its port is still being read", async () => {
  const bridge = await runBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  vi.spyOn(bridge, "runSessions").mockReturnValue(new Promise(() => {}));

  await clickButton("Start dev");

  expect(terminalTab("dev running").getAttribute("aria-selected")).toBe("true");
});

test("the Details card shows the focused agent session's name, state and where its state comes from", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await startAnother(bridge, feature);
  const second = await screen.findByRole("region", { name: "Panel 1" });
  await act(() => bridge.sendMessage("1", "Second task"));
  act(() => bridge.agentStateChanged({ session: "1", state: "Working", source: "Protocol" }));
  fireEvent.focus(second);

  await clickButton("Details");

  const block = within(detailsCard()).getByRole("group", { name: "Agent session" });
  expect(within(block).getByRole("heading", { name: "Second task" })).toBeTruthy();
  expect(within(block).getByText("Working")).toBeTruthy();
  expect(within(block).getByText("State from Protocol")).toBeTruthy();
});

test("the Details card shows the new session of a panel that changed agent while it had focus", async () => {
  const bridge = await trackedBridge();
  const two: Workspace = {
    ...DEFAULT_WORKSPACE,
    chat: [
      { agent: "Claude", session: null, shown: true },
      { agent: "Claude", session: null, shown: true },
    ],
  };
  await bridge.setWorkspace(feature, two);
  render(<App bridge={bridge} />);
  await select("feat-36");
  await act(async () => fireEvent.click(screen.getAllByRole("button", { name: "Start session" })[0]));
  await act(async () => fireEvent.click(screen.getAllByRole("button", { name: "Start session" })[0]));
  const second = screen.getByRole("region", { name: "Panel 1" });

  fireEvent.focus(second);
  await act(async () => fireEvent.click(within(second).getByRole("button", { name: "Switch to Codex" })));
  await clickButton("Details");

  const session = within(detailsCard()).getByRole("listitem", { name: "Session" });
  expect(within(session).getByText("Codex")).toBeTruthy();
});

test("selecting an agent session in the sidebar shows it in the Details card", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await startAnother(bridge, feature);
  await screen.findByRole("region", { name: "Panel 1" });
  await act(() => bridge.sendMessage("0", "First task"));
  await act(() => bridge.sendMessage("1", "Second task"));
  await clickButton("Details");
  const sessionTitle = () =>
    within(within(detailsCard()).getByRole("group", { name: "Agent session" })).getByRole("heading").textContent;

  const row0 = await findSessionRow("feat-36", 0);
  await act(async () => fireEvent.click(row0));
  await waitFor(() => expect(sessionTitle()).toBe("First task"));

  const row1 = await findSessionRow("feat-36", 1);
  await act(async () => fireEvent.click(row1));
  await waitFor(() => expect(sessionTitle()).toBe("Second task"));
});

test("the Details card stays open on a click outside, Escape, and after selecting another agent session and another worktree", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await startAnother(bridge, feature);
  await screen.findByRole("region", { name: "Panel 1" });
  await clickButton("Details");

  fireEvent.pointerDown(screen.getByRole("main"));
  fireEvent.keyDown(document.body, { key: "Escape" });
  const second = await findSessionRow("feat-36", 1);
  fireEvent.pointerDown(second);
  await act(async () => fireEvent.click(second));
  const other = screen.getByRole("button", { name: "main (main worktree)" });
  fireEvent.pointerDown(other);
  await act(async () => fireEvent.click(other));

  expect(detailsCard()).toBeTruthy();
  expect(pressed("Details")).toBe("true");
});

// Window shell (#29).

test("each title bar switch opens and closes its region, and shows as pressed while it is open", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  const regions: [string, () => HTMLElement | null][] = [
    ["Details", () => screen.queryByRole("region", { name: "Details" })],
    ["Terminal", () => screen.queryByRole("button", { name: "Open shell" })],
    ["Panel area", () => screen.queryByRole("region", { name: "Panel area" })],
  ];

  for (const [name, region] of regions) {
    const open = region() !== null;
    expect(pressed(name)).toBe(String(open));

    await clickButton(name);
    expect(region() !== null).toBe(!open);
    expect(pressed(name)).toBe(String(!open));

    await clickButton(name);
    expect(region() !== null).toBe(open);
    expect(pressed(name)).toBe(String(open));
  }
});

/** The accessible names of the buttons in `element`, in order. */
function buttonNames(element: HTMLElement) {
  return within(element)
    .getAllByRole("button")
    .map((button) => button.getAttribute("aria-label") ?? button.textContent);
}

test("with the panel area closed, the title bar ends with all three switches: details card, terminal, panel area", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  await clickButton("Panel area");
  const titleBar = screen.getByRole("banner");

  expect(buttonNames(titleBar).slice(-3)).toEqual(["Details", "Terminal", "Panel area"]);
  expect(within(titleBar).queryByRole("button", { name: /waiting|Settings|Appearance/ })).toBeNull();
});

test("with the panel area open, the title bar ends with Details, and the panel area's switches end its tab row", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  const titleBar = screen.getByRole("banner");
  const panelArea = screen.getByRole("region", { name: "Panel area" });

  expect(buttonNames(titleBar).at(-1)).toBe("Details");
  expect(within(titleBar).queryByRole("button", { name: "Terminal" })).toBeNull();
  expect(within(titleBar).queryByRole("button", { name: "Panel area" })).toBeNull();
  const tabRow = buttonNames(panelArea);
  const menu = tabRow.indexOf("Open a panel");
  expect(tabRow.slice(menu, menu + 3)).toEqual(["Open a panel", "Terminal", "Panel area"]);

  // The panel area's own switch closes it, and the switches go back to the title bar.
  await act(async () => fireEvent.click(within(panelArea).getByRole("button", { name: "Panel area" })));
  expect(buttonNames(titleBar).slice(-3)).toEqual(["Details", "Terminal", "Panel area"]);
});

test("with the panel area open and no worktree selected, its own switch still closes it", async () => {
  render(<App bridge={await trackedBridge()} />);
  const panelArea = screen.getByRole("region", { name: "Panel area" });

  expect(within(panelArea).getByRole("button", { name: "Terminal" }).hasAttribute("disabled")).toBe(true);
  await act(async () => fireEvent.click(within(panelArea).getByRole("button", { name: "Panel area" })));

  expect(screen.queryByRole("region", { name: "Panel area" })).toBeNull();
  expect(pressed("Panel area")).toBe("false");
});

test("the Details card opens in the chat lane, under the title bar, with the panel area open or closed", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");

  for (const panelsOpen of [true, false]) {
    if (!panelsOpen) await clickButton("Panel area");
    await clickButton("Details");
    const card = screen.getByRole("region", { name: "Details" });
    expect(screen.getByRole("main").contains(card)).toBe(true);
    expect(screen.queryByRole("region", { name: "Panel area" })?.contains(card) ?? false).toBe(false);
    await clickButton("Details");
  }
});

test("the Details column sits at the chat area's right edge, and the chat's text keeps out of it", async () => {
  const style = document.createElement("style");
  style.textContent = appStyles + detailsStyles;
  document.head.append(style);
  onTestFinished(() => style.remove());
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  const lane = screen.getByRole("main");

  await clickButton("Details");

  expect(getComputedStyle(lane).paddingRight).toBe("var(--details-card-width)");
  expect(getComputedStyle(document.querySelector(".details-holder")!).width).toBe("var(--details-card-width)");
});

const detailsShown = () => screen.queryByRole("region", { name: "Details" }) !== null;

test("Details hides, and its button shows it closed, while the chat area is too narrow for it, and comes back with room", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await clickButton("Details");

  relayout([[".chat-area", { ...WIDE_LANE, right: WIDE_LANE.left + 619 }]]);
  expect(detailsShown()).toBe(false);
  expect(pressed("Details")).toBe("false");

  relayout([[".chat-area", { ...WIDE_LANE, right: WIDE_LANE.left + 620 }]]);
  expect(detailsShown()).toBe(true);
  expect(pressed("Details")).toBe("true");
});

test("with two agent panels, Details shows only while each panel keeps its minimum width beside it", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await startAnother(bridge, feature);
  await screen.findByRole("region", { name: "Panel 1" });
  await clickButton("Details");
  expect(detailsShown()).toBe(true);

  relayout([[".chat-area", { ...WIDE_LANE, right: WIDE_LANE.left + 919 }]]);
  expect(detailsShown()).toBe(false);
  expect(pressed("Details")).toBe("false");

  relayout([[".chat-area", { ...WIDE_LANE, right: WIDE_LANE.left + 920 }]]);
  expect(detailsShown()).toBe(true);
});

test("Details stays open after a reload once opened, and opens closed when its saved choice is broken", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  await clickButton("Details");
  cleanup();

  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  expect(detailsShown()).toBe(true);
  cleanup();

  localStorage.setItem("grove.detailsOpen", "{not json");
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  expect(detailsShown()).toBe(false);
});

test("the title bar names the selected worktree's repo, with its badge, then the worktree", async () => {
  const bridge = fakeBridge({
    [main]: [
      { path: main, branch: "main", isMain: true },
      { path: feature, branch: "feat-36", isMain: false, displayName: "Title bar" },
    ],
  });
  await bridge.addRepo(main);
  render(<App bridge={bridge} />);
  const titleBar = screen.getByRole("banner");
  expect(titleBar.textContent).toContain("No worktree selected");

  await select("Title bar");

  expect(within(titleBar).getByText("GR").className).toContain("repo-badge");
  expect(within(titleBar).getByText("grove")).toBeTruthy();
  expect(within(titleBar).getByText("Title bar")).toBeTruthy();
});

test("Profiles in the sidebar opens the Profile panel, and the panel area with it", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  await clickButton("Panel area");

  await clickButton("Profiles");

  expect(screen.getByRole("tab", { name: "Profile", selected: true })).toBeTruthy();
  expect(pressed("Panel area")).toBe("true");
});

// Sidebar tree (#32).

test("clicking the second of two nested agent sessions focuses its panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  await startAnother(bridge, feature);
  await screen.findByRole("region", { name: "Panel 1" });
  await select("main (main worktree)");

  const row = await findSessionRow("feat-36", 1);
  await act(async () => fireEvent.click(row));

  expect(screen.getByLabelText("Panel 1").hidden).toBe(false);
  expect(document.activeElement).toBe(screen.getByLabelText("Panel 1"));
  expect(bridge.focused).toEqual(["1"]);
});

// Add repo with a folder picker (#17).

test("with no repos, the empty chat area's Add repo opens the picker and adds the folder", async () => {
  const bridge = fakeBridge({ [main]: [{ path: main, branch: "main", isMain: true }] });
  render(<App bridge={bridge} />);
  const chatArea = screen.getByRole("main");
  expect(await within(chatArea).findByText("No repo added yet")).toBeTruthy();
  bridge.willPickFolder(main);

  await act(async () => fireEvent.click(within(chatArea).getByRole("button", { name: "Add repo" })));

  expect(await screen.findByRole("button", { name: "main (main worktree)" })).toBeTruthy();
  expect(within(chatArea).queryByRole("button", { name: "Add repo" })).toBeNull();
});

test("the chat area's Add repo with the sidebar collapsed shows the sidebar when the folder is no repo", async () => {
  const bridge = fakeBridge({});
  render(<App bridge={bridge} />);
  fireEvent.click(screen.getByRole("button", { name: "Sidebar" }));
  const chatArea = screen.getByRole("main");
  bridge.willPickFolder("/code/not-a-repo");

  const addRepo = await within(chatArea).findByRole("button", { name: "Add repo" });

  await act(async () => fireEvent.click(addRepo));

  const sidebar = await screen.findByRole("complementary", { name: "Worktrees" });
  expect(within(sidebar).getByRole("alert").textContent).toBe("/code/not-a-repo is not a git repo");
});

test("the chat area says no repo is added only once the core's worktree list says so", async () => {
  const bridge = fakeBridge({});
  let answer: (worktrees: Worktree[]) => void = () => {};
  const first = new Promise<Worktree[]>((resolve) => (answer = resolve));
  bridge.listWorktrees = () => first;
  render(<App bridge={bridge} />);
  const chatArea = screen.getByRole("main");
  expect(within(chatArea).queryByText("No repo added yet")).toBeNull();

  await act(async () => answer([]));

  expect(await within(chatArea).findByText("No repo added yet")).toBeTruthy();
});

// The app icon (#55): the Dock shows the chosen style in the version that matches the theme.

test("by default the Dock shows the Mono icon that matches the theme, and the menu says Auto", async () => {
  systemDark = false;
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);

  fireEvent.click(screen.getByRole("button", { name: "Settings" }));

  expect(bridge.dockIcons).toEqual(["mono-light"]);
  expect(pressed("Auto")).toBe("true");
});

test.each([
  ["auto", "light", "mono-light"],
  ["auto", "dark", "mono-dark"],
  ["mono", "light", "mono-light"],
  ["mono", "dark", "mono-dark"],
  ["blue", "light", "blue-light"],
  ["blue", "dark", "blue-dark"],
])("the %s style with the %s theme asks for the %s icon", async (appIcon, theme, icon) => {
  localStorage.setItem("grove.appearance", JSON.stringify({ theme, palette: "default", appIcon }));
  const bridge = await trackedBridge();

  render(<App bridge={bridge} />);

  expect(bridge.dockIcons).toEqual([icon]);
});

test("picking a style switches the Dock icon at once, and the choice survives a reload", async () => {
  systemDark = true;
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);
  fireEvent.click(screen.getByRole("button", { name: "Settings" }));

  fireEvent.click(screen.getByRole("button", { name: "Blue" }));

  await waitFor(() => expect(bridge.dockIcons).toEqual(["mono-dark", "blue-dark"]));
  unmount();
  render(<App bridge={bridge} />);
  fireEvent.click(screen.getByRole("button", { name: "Settings" }));
  await waitFor(() => expect(bridge.dockIcons).toEqual(["mono-dark", "blue-dark", "blue-dark"]));
  expect(pressed("Blue")).toBe("true");
});

test("theme changes made while a request is out ask only for the last icon", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  fireEvent.click(screen.getByRole("button", { name: "Settings" }));

  fireEvent.click(screen.getByRole("button", { name: "Light" }));
  fireEvent.click(screen.getByRole("button", { name: "Light" }));
  fireEvent.click(screen.getByRole("button", { name: "Dark" }));

  // The Light click waits behind the first request and is replaced by the Dark click.
  await waitFor(() => expect(bridge.dockIcons).toEqual(["mono-dark", "mono-dark"]));
});

test("with the System theme, a macOS appearance change asks for the new icon once", async () => {
  systemDark = false;
  localStorage.setItem("grove.appearance", JSON.stringify({ theme: "system", palette: "default", appIcon: "blue" }));
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);

  setSystemDark(true);

  await waitFor(() => expect(bridge.dockIcons).toEqual(["blue-light", "blue-dark"]));
});

test("an appearance saved before the app icon setting keeps its theme and palette and gets Auto", async () => {
  localStorage.setItem("grove.appearance", JSON.stringify({ theme: "light", palette: "modern" }));
  const bridge = await trackedBridge();

  render(<App bridge={bridge} />);

  expect(document.documentElement.dataset).toMatchObject({ palette: "modern", theme: "light" });
  expect(bridge.dockIcons).toEqual(["mono-light"]);
  expect(JSON.parse(localStorage.getItem("grove.appearance") ?? "null")).toEqual({
    theme: "light",
    palette: "modern",
    appIcon: "auto",
  });
});

test("a Dock icon that cannot be set is logged and the app keeps working", async () => {
  const bridge = await trackedBridge();
  vi.spyOn(bridge, "setDockIcon").mockRejectedValue("no main thread");
  vi.spyOn(console, "warn").mockImplementation(() => {});

  render(<App bridge={bridge} />);

  await waitFor(() => expect(console.warn).toHaveBeenCalledWith("could not set the Dock icon:", "no main thread"));
  expect(screen.getByRole("button", { name: "Settings" })).toBeTruthy();
});

test("quick icon changes leave the last requested Dock icon applied, even when an older request is slow", async () => {
  systemDark = true;
  const bridge = await trackedBridge();
  const finishers: (() => void)[] = [];
  const applied: string[] = [];
  vi.spyOn(bridge, "setDockIcon").mockImplementation((icon) => {
    return new Promise<void>((resolve) => {
      finishers.push(() => {
        applied.push(icon);
        resolve();
      });
    });
  });
  render(<App bridge={bridge} />);
  fireEvent.click(screen.getByRole("button", { name: "Settings" }));

  fireEvent.click(screen.getByRole("button", { name: "Blue" }));
  fireEvent.click(screen.getByRole("button", { name: "Mono" }));
  fireEvent.click(screen.getByRole("button", { name: "Blue" }));

  // Only the first request is out while it is slow.
  expect(finishers).toHaveLength(1);
  while (finishers.length > 0) {
    finishers.shift()?.();
    await Promise.resolve();
    await Promise.resolve();
  }
  await waitFor(() => expect(applied.at(-1)).toBe("blue-dark"));
});
