import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { App } from "./App";
import { DEFAULT_WORKSPACE, fakeBridge, type FakeBridge } from "./fakeBridge";

// Each worktree's workspace: its agent panels side by side, its panel-area tabs and its bottom
// terminal. The agent panels are the real ones, so two of them streaming can be told apart.
vi.mock("./DiffView", () => ({ DiffView: () => <p>Diff</p> }));
vi.mock("./FilesPanel", () => ({
  FilesPanel: ({ worktree }: { worktree: string }) => <p>Files of {worktree}</p>,
}));
vi.mock("./ProfilePanel", () => ({
  ProfilePanel: ({ worktree }: { worktree: string }) => <p>Profile of {worktree}</p>,
}));
vi.mock("./TerminalPanel", () => ({
  TerminalPanel: ({ session, shown }: { session: string; shown: boolean }) => (
    <section aria-label={`Panel ${session}`} hidden={!shown} />
  ),
}));

// Animation frames run only when a test calls `nextFrame`.
let frames: FrameRequestCallback[] = [];

function nextFrame() {
  const due = frames;
  frames = [];
  act(() => due.forEach((callback) => callback(performance.now())));
}

beforeEach(() => {
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => frames.push(callback));
  vi.stubGlobal("cancelAnimationFrame", () => {});
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.stubGlobal("matchMedia", (media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  }));
});

afterEach(() => {
  cleanup();
  frames = [];
  localStorage.clear();
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

async function select(branch: string) {
  const row = await screen.findByRole("button", { name: branch });
  await act(async () => fireEvent.click(row));
}

function panel(session: string) {
  return screen.getByLabelText(`Panel ${session}`);
}

function shownPanels() {
  return within(screen.getByRole("main"))
    .queryAllByRole("region")
    .map((region) => region.getAttribute("aria-label"));
}

async function click(element: HTMLElement) {
  await act(async () => fireEvent.click(element));
}

// Starts Claude Code from the selected worktree's menu.
async function startClaudeCode() {
  const row = screen.getAllByRole("button", { current: true }).find((button) => button.classList.contains("wt-name"))!;
  fireEvent.click(within(row.closest(".wt-row") as HTMLElement).getByRole("button", { name: /^Actions for / }));
  await click(screen.getByRole("menuitem", { name: "Start Claude Code" }));
}

// Starts Claude Code in feat-36, then splits its panel from the title bar, which starts Codex
// beside it.
async function twoPanels(bridge: FakeBridge) {
  await select("feat-36");
  await startClaudeCode();
  await screen.findByRole("region", { name: "Panel 0" });
  await click(within(screen.getByRole("banner")).getByRole("button", { name: "Split" }));
  await screen.findByRole("region", { name: "Panel 1" });
  expect(bridge.spawnedAgents.map((s) => s.agent)).toEqual(["Claude", "Codex"]);
}

// The agent the panel's header names.
function agentName(region: HTMLElement) {
  return region.querySelector(".pane-head b")!.textContent;
}

function terminalToggle() {
  // It ends the panel area's tab row while the panel area is open, and the title bar otherwise.
  const bar = screen.queryByRole("region", { name: "Panel area" }) ?? screen.getByRole("banner");
  return within(bar).getByRole("button", { name: "Terminal" });
}

function tab(name: string) {
  return screen.getByRole("tab", { name });
}

function openMenu() {
  return click(screen.getByRole("button", { name: "Open a panel" }));
}

test("with one agent panel the title bar names its agent and state, and the panel draws no header row", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await startClaudeCode();
  await screen.findByRole("region", { name: "Panel 0" });

  act(() => bridge.agentStateChanged({ session: "0", state: "Working", source: "Protocol" }));

  const titleBar = screen.getByRole("banner");
  expect(within(titleBar).getByText("Claude Code")).toBeTruthy();
  const state = titleBar.querySelector<HTMLElement>(".ui-agent-state")!;
  expect(state.textContent).toBe("Working");
  expect(state.title).toBe("State from Protocol");
  expect(panel("0").querySelector(".pane-head")).toBeNull();
});

test("with one agent panel, Split sits in the title bar before Details, and the panel has none", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await startClaudeCode();
  await screen.findByRole("region", { name: "Panel 0" });

  const switches = within(screen.getByRole("banner"))
    .getAllByRole("button")
    .map((button) => button.getAttribute("aria-label"));
  expect(switches.indexOf("Split")).toBe(switches.indexOf("Details") - 1);
  expect(within(panel("0")).queryByRole("button", { name: "Split" })).toBeNull();
});

test("splitting a panel starts the other agent beside it and saves both in the workspace", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);

  await twoPanels(bridge);

  expect(shownPanels()).toEqual(["Panel 0", "Panel 1"]);
  expect(agentName(panel("1"))).toBe("Codex");
  // With two panels, each splits from its own header.
  expect(within(screen.getByRole("banner")).queryByRole("button", { name: "Split" })).toBeNull();
  expect(within(panel("0")).getByRole("button", { name: "Split" })).toBeTruthy();
  expect((await bridge.workspace(feature)).chat).toEqual([
    { agent: "Claude", session: "0", shown: true },
    { agent: "Codex", session: "1", shown: true },
  ]);
});

test("with two agent panels, each draws a header with its agent and state, and the title bar names neither", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await twoPanels(bridge);

  act(() => {
    bridge.agentStateChanged({ session: "0", state: "Working", source: "Protocol" });
    bridge.agentStateChanged({ session: "1", state: "NeedsInput", kind: "Approval", source: "Protocol" });
  });

  for (const [session, agent, state] of [
    ["0", "Claude Code", "Working"],
    ["1", "Codex", "Approval"],
  ]) {
    const head = panel(session).querySelector<HTMLElement>(".pane-head")!;
    expect(agentName(panel(session))).toBe(agent);
    expect(head.querySelector(".ui-agent-state")!.textContent).toBe(state);
    expect(head.textContent).not.toContain("Protocol");
  }
  expect(screen.getByRole("banner").querySelector(".titlebar-agent")).toBeNull();
});

test("a third panel can be split off, but no fourth", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await twoPanels(bridge);

  await click(within(panel("1")).getByRole("button", { name: "Split" }));

  expect(await screen.findByRole("region", { name: "Panel 2" })).toBeTruthy();
  expect(bridge.spawnedAgents[2].agent).toBe("Claude");
  expect(within(screen.getByRole("main")).queryByRole("button", { name: "Split" })).toBeNull();
});

test("hiding a panel keeps its session running, and its row in the sidebar shows it again", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await twoPanels(bridge);

  await click(within(panel("1")).getByRole("button", { name: "Hide panel" }));

  expect(panel("1").hidden).toBe(true);
  expect(bridge.killed).toEqual([]);
  expect((await bridge.workspace(feature)).chat[1]).toEqual({ agent: "Codex", session: "1", shown: false });
  // The last shown panel has no Hide button.
  expect(within(panel("0")).queryByRole("button", { name: "Hide panel" })).toBeNull();

  await click(screen.getByText("Codex", { selector: ".session-name" }));

  expect(shownPanels()).toEqual(["Panel 0", "Panel 1"]);
  expect((await bridge.workspace(feature)).chat[1].shown).toBe(true);
});

test("focusing a panel calls focus for its own session", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await twoPanels(bridge);

  await act(async () => panel("1").focus());
  await act(async () => panel("0").focus());

  expect(bridge.focused).toEqual(["1", "0"]);
});

test("two panels streaming at once each show only their own messages", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await twoPanels(bridge);

  act(() => {
    bridge.messages("0", { type: "TurnStarted" }, { type: "TextDelta", text: "Claude writes the test." });
    bridge.messages("1", { type: "TurnStarted" }, { type: "TextDelta", text: "Codex reviews the diff." });
    bridge.messages("0", { type: "TextDelta", text: " It fails first." });
    bridge.messages("1", { type: "TextDelta", text: " Two nits." });
  });
  nextFrame();

  expect(within(panel("0")).getByText("Claude writes the test. It fails first.")).toBeTruthy();
  expect(within(panel("0")).queryByText(/Codex reviews/)).toBeNull();
  expect(within(panel("1")).getByText("Codex reviews the diff. Two nits.")).toBeTruthy();
  expect(within(panel("1")).queryByText(/Claude writes/)).toBeNull();
});

test("each worktree keeps its own layout", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await twoPanels(bridge);
  await click(tab("Diff"));
  await click(terminalToggle());

  await select("main (main worktree)");

  expect(shownPanels()).toEqual([]);
  expect(screen.getByRole("button", { name: "Start session" })).toBeTruthy();
  expect(tab("Files").getAttribute("aria-selected")).toBe("true");
  expect(terminalToggle().getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByRole("button", { name: "Open shell" })).toBeTruthy();

  await select("feat-36");

  expect(shownPanels()).toEqual(["Panel 0", "Panel 1"]);
  expect(tab("Diff").getAttribute("aria-selected")).toBe("true");
  expect(terminalToggle().getAttribute("aria-pressed")).toBe("false");
  expect(screen.queryByRole("button", { name: "Open shell" })).toBeNull();
  expect(bridge.workspacesSet.every((set) => set.worktree === feature)).toBe(true);
});

test("a reloaded window gets each worktree's layout back from the core", async () => {
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);
  await twoPanels(bridge);
  await click(tab("Diff"));
  unmount();

  render(<App bridge={bridge} />);
  await select("feat-36");

  expect(shownPanels()).toEqual(["Panel 0", "Panel 1"]);
  expect(agentName(panel("1"))).toBe("Codex");
  expect(tab("Diff").getAttribute("aria-selected")).toBe("true");
});

// The mode the agent panel's switch shows.
function pressedMode() {
  const modes = within(screen.getByRole("group", { name: "Mode" })).getAllByRole("button");
  return modes.find((button) => button.getAttribute("aria-pressed") === "true")?.textContent;
}

// A restart brings each agent session back in its mode, under the same id, before the window opens.
test("after a restart a panel whose session runs in terminal mode opens in terminal mode", async () => {
  const bridge = await trackedBridge();
  const session = await bridge.spawnAgent(feature, "Claude", 0);
  await bridge.setMode(session, "Terminal");
  render(<App bridge={bridge} />);

  await select("feat-36");

  await waitFor(() => expect(pressedMode()).toBe("Terminal"));
  expect(screen.getByText("The same conversation, resumed in Claude Code's own TUI. State now comes from hooks.")).toBeTruthy();
  expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
  expect(bridge.subscribed).toEqual([]);
});

test("after a window reload a panel switched to terminal mode opens in terminal mode", async () => {
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);
  await select("feat-36");
  await startClaudeCode();
  await click(within(panel("0")).getByRole("button", { name: "Terminal" }));
  unmount();
  const subscribes = bridge.subscribed.length;

  render(<App bridge={bridge} />);
  await select("feat-36");

  await waitFor(() => expect(pressedMode()).toBe("Terminal"));
  expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
  expect(bridge.subscribed).toHaveLength(subscribes);
});

test("an unstarted panel starts its own agent", async () => {
  const bridge = await trackedBridge();
  await bridge.setWorkspace(feature, {
    ...DEFAULT_WORKSPACE,
    chat: [{ agent: "Codex", session: null, shown: true }],
  });
  render(<App bridge={bridge} />);
  await select("feat-36");

  // Alone, it draws no header: the title bar names its agent.
  expect(within(screen.getByRole("banner")).getByText("Codex")).toBeTruthy();
  expect(screen.getByRole("main").querySelector(".pane-head")).toBeNull();

  await click(screen.getByRole("button", { name: "Start session" }));

  expect(await screen.findByRole("region", { name: "Panel 0" })).toBeTruthy();
  expect(bridge.spawnedAgents).toEqual([{ session: "0", worktree: feature, agent: "Codex" }]);
  expect((await bridge.workspace(feature)).chat).toEqual([{ agent: "Codex", session: "0", shown: true }]);
});

test("the + menu opens a tab and selects it, and closing a tab selects the last one left", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  await openMenu();
  const menu = screen.getByRole("menu", { name: "Open a panel" });
  expect(within(menu).getByRole("menuitemcheckbox", { name: "Files" }).getAttribute("aria-checked")).toBe("true");
  expect(within(menu).getByRole("menuitemcheckbox", { name: "Logs" }).getAttribute("aria-checked")).toBe("false");
  await click(within(menu).getByRole("menuitemcheckbox", { name: "Logs" }));

  expect(screen.queryByRole("menu")).toBeNull();
  expect(tab("Logs").getAttribute("aria-selected")).toBe("true");
  expect(screen.getByText("The Logs panel is not available yet.")).toBeTruthy();
  expect(await bridge.workspace(feature)).toMatchObject({ panels: ["Files", "Diff", "Logs"], selected: "Logs" });

  await click(screen.getByRole("button", { name: "Close Logs" }));

  expect(screen.queryByRole("tab", { name: "Logs" })).toBeNull();
  expect(tab("Diff").getAttribute("aria-selected")).toBe("true");

  await click(screen.getByRole("button", { name: "Close Diff" }));
  await click(screen.getByRole("button", { name: "Close Files" }));

  expect(screen.queryByRole("tab")).toBeNull();
  expect(screen.getByText("No panel open. Use + to open one.")).toBeTruthy();
  expect(await bridge.workspace(feature)).toMatchObject({ panels: [], selected: null });
});

test("the Profile tab shows the selected worktree's Profile panel, which stays while another tab shows", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  await openMenu();
  await click(within(screen.getByRole("menu", { name: "Open a panel" })).getByRole("menuitemcheckbox", { name: "Profile" }));

  const body = screen.getByRole("tabpanel", { name: "Profile" });
  expect(within(body).getByText(`Profile of ${feature}`)).toBeTruthy();
  expect(screen.queryByText("The Profile panel is not available yet.")).toBeNull();

  await click(tab("Files"));

  expect(screen.queryByRole("tabpanel", { name: "Profile" })).toBeNull();
  expect(screen.getByText(`Profile of ${feature}`)).toBeTruthy();
});

test("while the + menu is open, each panel's letter opens it", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  await openMenu();
  await act(async () => fireEvent.keyDown(document.activeElement!, { key: "b" }));

  expect(screen.queryByRole("menu")).toBeNull();
  expect(tab("Browser").getAttribute("aria-selected")).toBe("true");

  // With the menu closed, a letter opens nothing.
  await act(async () => fireEvent.keyDown(document.body, { key: "l" }));
  expect(screen.queryByRole("tab", { name: "Logs" })).toBeNull();

  await openMenu();
  await act(async () => fireEvent.keyDown(document.activeElement!, { key: "F" }));

  expect(tab("Files").getAttribute("aria-selected")).toBe("true");
  expect(screen.getAllByRole("tab").map((t) => t.textContent)).toEqual(["Files", "Diff", "Browser"]);
});

test("Escape closes the + menu and opens nothing", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");

  await openMenu();
  await act(async () => fireEvent.keyDown(document.activeElement!, { key: "Escape" }));

  expect(screen.queryByRole("menu")).toBeNull();
  expect(screen.getAllByRole("tab")).toHaveLength(2);
});

function focusedMenuItem() {
  return (document.activeElement as HTMLElement).getAttribute("aria-label");
}

/** Presses each key on the focused element in turn and returns which item has focus after each press. */
async function pressInMenu(...keys: string[]) {
  const focused: (string | null)[] = [];
  for (const key of keys) {
    await act(async () => fireEvent.keyDown(document.activeElement!, { key }));
    focused.push(focusedMenuItem());
  }
  return focused;
}

test("in the + menu, Down and Up move focus to the next and previous item, wrapping at both ends", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  await openMenu();
  expect(focusedMenuItem()).toBe("Files");

  // Down through all six items, which ends back on the first.
  expect(await pressInMenu(...Array(6).fill("ArrowDown"))).toEqual([
    "Diff",
    "Terminal",
    "Browser",
    "Logs",
    "Profile",
    "Files",
  ]);
  // Up from the first wraps to the last.
  expect(await pressInMenu("ArrowUp", "ArrowUp")).toEqual(["Profile", "Logs"]);
  expect(screen.getByRole("menu")).toBeTruthy();
});

test("in the + menu, Down and Up do not scroll the page", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  await openMenu();

  const down = await act(async () => fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" }));
  const up = await act(async () => fireEvent.keyDown(document.activeElement!, { key: "ArrowUp" }));

  // fireEvent returns false when the handler called preventDefault.
  expect(down).toBe(false);
  expect(up).toBe(false);
});

test("in the + menu, Tab is left to the browser and closes the menu, and a letter opens its panel after an arrow", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  await openMenu();

  const tabKey = await act(async () => fireEvent.keyDown(document.activeElement!, { key: "Tab" }));

  expect(tabKey).toBe(true);
  expect(screen.queryByRole("menu")).toBeNull();

  await openMenu();
  await act(async () => fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" }));
  await act(async () => fireEvent.keyDown(document.activeElement!, { key: "l" }));

  expect(screen.queryByRole("menu")).toBeNull();
  expect(tab("Logs").getAttribute("aria-selected")).toBe("true");
});

test("with the + menu closed, Down and Up are left alone", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");

  const down = await act(async () => fireEvent.keyDown(document.body, { key: "ArrowDown" }));

  expect(down).toBe(true);
});

test("the terminal switch toggles it, and the + menu's T opens it", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  const toggle = terminalToggle();
  expect(toggle.getAttribute("aria-pressed")).toBe("true");

  await click(toggle);

  expect(toggle.getAttribute("aria-pressed")).toBe("false");
  expect(screen.queryByRole("button", { name: "Open shell" })).toBeNull();
  expect((await bridge.workspace(feature)).terminal).toBe(false);

  await openMenu();
  await act(async () => fireEvent.keyDown(document.activeElement!, { key: "t" }));

  expect(toggle.getAttribute("aria-pressed")).toBe("true");
  expect(screen.getByRole("button", { name: "Open shell" })).toBeTruthy();
});

test("a closed terminal hides its shell, which keeps running", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(screen.getByRole("button", { name: "Open shell" }));
  const shell = await screen.findByLabelText("Panel 0");

  await click(terminalToggle());

  expect(shell.hidden).toBe(true);
  expect(bridge.killed).toEqual([]);

  await click(terminalToggle());

  expect(shell.hidden).toBe(false);
});

test("opening the terminal shows a collapsed panel area", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  await click(terminalToggle());
  await click(screen.getByRole("button", { name: "Panel area" }));

  await click(terminalToggle());

  expect(screen.getByRole("region", { name: "Panel area" })).toBeTruthy();
});

test("a workspace the core does not save says why", async () => {
  const bridge = await trackedBridge();
  vi.spyOn(bridge, "setWorkspace").mockRejectedValueOnce("the state file is read-only");
  render(<App bridge={bridge} />);
  await select("feat-36");

  await click(tab("Diff"));

  expect(screen.getByRole("alert").textContent).toBe(
    "Could not save this worktree's layout: the state file is read-only",
  );
});

test("a layout that loads clears an earlier load error", async () => {
  const bridge = await trackedBridge();
  vi.spyOn(bridge, "workspace").mockRejectedValueOnce("the state file is unreadable");
  render(<App bridge={bridge} />);
  await select("feat-36");
  expect(screen.getByRole("alert").textContent).toBe(
    "Could not load this worktree's layout: the state file is unreadable",
  );

  await select("main (main worktree)");

  expect(screen.queryByRole("alert")).toBeNull();
});

test("a slow layout that loads late leaves another worktree's load error in place", async () => {
  const bridge = await trackedBridge();
  const workspace = bridge.workspace.bind(bridge);
  let loadFeature = () => {};
  vi.spyOn(bridge, "workspace").mockImplementation((worktree) =>
    worktree === feature
      ? new Promise((resolve) => (loadFeature = () => resolve(workspace(feature))))
      : Promise.reject("the state file is unreadable"),
  );
  render(<App bridge={bridge} />);
  await select("feat-36");
  await select("main (main worktree)");

  await act(async () => loadFeature());

  expect(screen.getByRole("alert").textContent).toBe(
    "Could not load this worktree's layout: the state file is unreadable",
  );
});

test("no agent session starts in a worktree whose layout does not load", async () => {
  const bridge = await trackedBridge();
  vi.spyOn(bridge, "workspace").mockRejectedValue("the state file is unreadable");
  render(<App bridge={bridge} />);
  await select("feat-36");

  await startClaudeCode();

  expect(bridge.spawnedAgents).toEqual([]);
  expect(screen.getByRole("alert").textContent).toBe(
    "Could not load this worktree's layout: the state file is unreadable",
  );
});

// Issue #204: the session runs, so only the layout failed.
test("a session that starts when the core's copy then does not load shows as started, with a layout error", async () => {
  const bridge = await trackedBridge();
  const spawnAgent = bridge.spawnAgent.bind(bridge);
  vi.spyOn(bridge, "spawnAgent").mockImplementation(async (worktree, agent, at) => {
    const session = await spawnAgent(worktree, agent, at);
    vi.spyOn(bridge, "workspace").mockRejectedValueOnce("the state file is unreadable");
    return session;
  });
  render(<App bridge={bridge} />);
  await select("feat-36");

  await click(screen.getByRole("button", { name: "Start session" }));

  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));
  expect(bridge.spawnedAgents).toHaveLength(1);
  expect(screen.getByRole("alert").textContent).toBe(
    "Could not load this worktree's layout: the state file is unreadable",
  );
});

// The jump waits for the start, so the started session keeps the panel clicked and the jumped-to
// one goes after it.
test("a jump while a session starts gives both sessions a panel", async () => {
  const bridge = await trackedBridge();
  // Waiting on the user, from before the window opened, so it has no panel yet.
  const waiting = bridge.spawnAgentWithoutPanel(feature, "Claude");
  bridge.agentStateChanged({ session: waiting, state: "NeedsInput", kind: "Approval", source: "Protocol" });
  const spawnAgent = bridge.spawnAgent.bind(bridge);
  let finishSpawn = () => {};
  vi.spyOn(bridge, "spawnAgent").mockImplementation(
    (worktree, agent, at) => new Promise((resolve) => (finishSpawn = () => resolve(spawnAgent(worktree, agent, at)))),
  );
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(screen.getByRole("button", { name: "Start session" }));

  await click(screen.getByRole("button", { name: "Needs you 1" }));
  await act(async () => finishSpawn());

  expect(shownPanels()).toEqual([`Panel ${waiting}`, "Panel 1"]);
  await waitFor(async () =>
    expect((await bridge.workspace(feature)).chat.map((panel) => panel.session)).toEqual(["1", waiting]),
  );
});

// Issue #158: the core places a new agent session in its panel as it starts it, so a reload
// between the start and a save can never leave it without one.
test("starting a session in an empty panel saves no workspace, and the core's change draws the panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  await click(screen.getByRole("button", { name: "Start session" }));

  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));
  expect(bridge.workspacesSet).toEqual([]);
  expect((await bridge.workspace(feature)).chat).toEqual([{ agent: "Claude", session: "0", shown: true }]);
});

test("a session that ends before the window loads the core's copy keeps its panel", async () => {
  const bridge = await trackedBridge();
  const spawnAgent = bridge.spawnAgent.bind(bridge);
  vi.spyOn(bridge, "spawnAgent").mockImplementation(async (worktree, agent, at) => {
    const session = await spawnAgent(worktree, agent, at);
    bridge.exit(session);
    return session;
  });
  render(<App bridge={bridge} />);
  await select("feat-36");

  await click(screen.getByRole("button", { name: "Start session" }));

  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));
  expect(bridge.workspacesSet).toEqual([]);
});

test("a reload right after a start shows the session in its panel", async () => {
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);
  await select("feat-36");
  await click(screen.getByRole("button", { name: "Start session" }));
  unmount();

  render(<App bridge={bridge} />);
  await select("feat-36");

  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));
});

test("a start from the sidebar fills the first empty panel, and a split goes after the last", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);

  await twoPanels(bridge);

  expect(bridge.workspacesSet).toEqual([]);
  expect((await bridge.workspace(feature)).chat).toEqual([
    { agent: "Claude", session: "0", shown: true },
    { agent: "Codex", session: "1", shown: true },
  ]);
});

test("a workspace changed outside the window is redrawn", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  await act(() => bridge.setWorkspace(feature, { ...DEFAULT_WORKSPACE, panels: ["Files", "Logs"], selected: "Logs" }));

  await waitFor(() => expect(tab("Logs").getAttribute("aria-selected")).toBe("true"));
});

test("a change made while the core's change loads is kept on top of it", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  const workspace = bridge.workspace.bind(bridge);
  let finishLoad = () => {};
  vi.spyOn(bridge, "workspace").mockImplementation(
    (worktree) => new Promise((resolve) => (finishLoad = () => resolve(workspace(worktree)))),
  );

  // As an agent would, through grove-mcp: the terminal closes.
  await act(() => bridge.setWorkspace(feature, { ...DEFAULT_WORKSPACE, terminal: false }));
  await openMenu();
  await click(within(screen.getByRole("menu", { name: "Open a panel" })).getByRole("menuitemcheckbox", { name: "Logs" }));
  await act(async () => finishLoad());

  await waitFor(() => expect(tab("Logs").getAttribute("aria-selected")).toBe("true"));
  expect(terminalToggle().getAttribute("aria-pressed")).toBe("false");
  await waitFor(async () =>
    expect(await workspace(feature)).toEqual({
      ...DEFAULT_WORKSPACE,
      panels: ["Files", "Diff", "Logs"],
      selected: "Logs",
      terminal: false,
    }),
  );
});

function startButtons() {
  return screen.queryAllByRole("button", { name: "Start session" });
}

test("an ended session keeps its panel through later workspace changes", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(startButtons()[0]);
  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));

  act(() => bridge.exit("0"));
  await click(tab("Diff"));
  await act(async () => {});

  expect(shownPanels()).toEqual(["Panel 0"]);
});

test("a panel after an ended session's panel starts its own agent there", async () => {
  const bridge = await trackedBridge();
  await bridge.setWorkspace(feature, {
    ...DEFAULT_WORKSPACE,
    chat: [
      { agent: "Claude", session: null, shown: true },
      { agent: "Codex", session: null, shown: true },
    ],
  });
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(startButtons()[0]);
  await waitFor(() => expect(startButtons()).toHaveLength(1));
  act(() => bridge.exit("0"));

  await click(startButtons()[0]);

  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0", "Panel 1"]));
  expect(screen.queryByRole("alert")).toBeNull();
  expect(bridge.spawnedAgents.map((s) => s.agent)).toEqual(["Claude", "Codex"]);
  expect((await bridge.workspace(feature)).chat).toEqual([{ agent: "Codex", session: "1", shown: true }]);
});

test("a start lands in the panel clicked, not in the one after it, when an ended session's panel is before it", async () => {
  const bridge = await trackedBridge();
  await bridge.setWorkspace(feature, {
    ...DEFAULT_WORKSPACE,
    chat: [
      { agent: "Claude", session: null, shown: true },
      { agent: "Claude", session: null, shown: true },
      { agent: "Codex", session: null, shown: true },
    ],
  });
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(startButtons()[0]);
  await waitFor(() => expect(startButtons()).toHaveLength(2));
  act(() => bridge.exit("0"));

  await click(startButtons()[0]);

  await waitFor(async () =>
    expect((await bridge.workspace(feature)).chat).toEqual([
      { agent: "Claude", session: "1", shown: true },
      { agent: "Codex", session: null, shown: true },
    ]),
  );
});

test("a change made while a session starts does not save over its panel", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  const spawnAgent = bridge.spawnAgent.bind(bridge);
  let reply = () => {};
  vi.spyOn(bridge, "spawnAgent").mockImplementation(async (worktree, agent, at) => {
    // The core has placed it, and its reply and event are still on the way.
    const session = await spawnAgent(worktree, agent, at);
    return new Promise((resolve) => (reply = () => resolve(session)));
  });
  const release = bridge.holdWorkspaceChanges();

  await click(startButtons()[0]);
  await openMenu();
  await click(within(screen.getByRole("menu", { name: "Open a panel" })).getByRole("menuitemcheckbox", { name: "Logs" }));
  await act(async () => reply());
  await act(async () => release());

  await waitFor(async () => {
    const saved = await bridge.workspace(feature);
    expect(saved.chat).toEqual([{ agent: "Claude", session: "0", shown: true }]);
    expect(saved.panels).toEqual(["Files", "Diff", "Logs"]);
  });
  expect(shownPanels()).toEqual(["Panel 0"]);
});

test("a second click while a session starts starts nothing more", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  // The core's change never reaches the window, and its copy loads only when the test says.
  bridge.holdWorkspaceChanges();
  const spawnAgent = bridge.spawnAgent.bind(bridge);
  const workspace = bridge.workspace.bind(bridge);
  let loaded: Promise<void> | null = null;
  let finishLoad = () => {};
  vi.spyOn(bridge, "spawnAgent").mockImplementation(async (worktree, agent, at) => {
    loaded = new Promise((resolve) => (finishLoad = resolve));
    return spawnAgent(worktree, agent, at);
  });
  vi.spyOn(bridge, "workspace").mockImplementation(async (worktree) => {
    if (loaded) await loaded;
    return workspace(worktree);
  });

  await click(startButtons()[0]);
  // The reply came and the panel is not drawn yet.
  await click(startButtons()[0]);
  await act(async () => finishLoad());

  expect(bridge.spawnedAgents).toHaveLength(1);
  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));
  expect(startButtons()).toEqual([]);
});

test("a start counts two ended sessions' panels before the one clicked", async () => {
  const bridge = await trackedBridge();
  await bridge.setWorkspace(feature, {
    ...DEFAULT_WORKSPACE,
    chat: [
      { agent: "Claude", session: null, shown: true },
      { agent: "Claude", session: null, shown: true },
      { agent: "Codex", session: null, shown: true },
    ],
  });
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(startButtons()[0]);
  await waitFor(() => expect(startButtons()).toHaveLength(2));
  await click(startButtons()[0]);
  await waitFor(() => expect(startButtons()).toHaveLength(1));
  act(() => bridge.exit("0"));
  act(() => bridge.exit("1"));

  await click(startButtons()[0]);

  await waitFor(async () =>
    expect((await bridge.workspace(feature)).chat).toEqual([{ agent: "Codex", session: "2", shown: true }]),
  );
  expect(shownPanels()).toEqual(["Panel 0", "Panel 1", "Panel 2"]);
});

test("a running session whose panel the core no longer lists loses it, even after a mode switch", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(startButtons()[0]);
  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));
  // In the core a mode switch ends the process, then the session runs again.
  act(() => bridge.exit("0"));
  await act(() => bridge.setMode("0", "Terminal"));

  // As an agent would, through grove-mcp.
  await act(() => bridge.setWorkspace(feature, { ...DEFAULT_WORKSPACE, chat: [] }));

  await waitFor(() => expect(shownPanels()).toEqual([]));
});

test("a session keeps its panel when the core's copy comes while it switches mode", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(startButtons()[0]);
  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));
  const release = bridge.holdModeSwitches();
  const switched = bridge.setMode("0", "Terminal");

  // Saved, so the core's copy is loaded again while the old process has ended and the new one
  // has not started.
  await click(tab("Diff"));
  await waitFor(() => expect(bridge.workspacesSet.at(-1)?.workspace.selected).toBe("Diff"));
  await act(async () => {});

  expect(shownPanels()).toEqual(["Panel 0"]);
  release();
  await act(() => switched);
  expect(shownPanels()).toEqual(["Panel 0"]);
});

test("an ended session keeps its panel when the core's copy comes before the window hears it ended", async () => {
  const bridge = await trackedBridge();
  // Each session-exited reaches the window only when the test says.
  const heard: (() => void)[] = [];
  const onSessionExited = bridge.onSessionExited.bind(bridge);
  vi.spyOn(bridge, "onSessionExited").mockImplementation((listener) =>
    onSessionExited((session) => heard.push(() => listener(session))),
  );
  render(<App bridge={bridge} />);
  await select("feat-36");
  await click(startButtons()[0]);
  await waitFor(() => expect(shownPanels()).toEqual(["Panel 0"]));

  act(() => bridge.exit("0"));
  await click(tab("Diff"));
  await act(async () => {});
  act(() => heard.forEach((hear) => hear()));

  expect(shownPanels()).toEqual(["Panel 0"]);
});

test("the fake core keeps an ended session's panel when it places another, as the core does", async () => {
  const bridge = await trackedBridge();
  const ended = await bridge.spawnAgent(feature, "Claude", 0);
  await bridge.setWorkspace(feature, {
    ...DEFAULT_WORKSPACE,
    chat: [
      { agent: "Claude", session: ended, shown: true },
      { agent: "Codex", session: null, shown: true },
    ],
  });
  bridge.exit(ended);

  const placed = await bridge.spawnAgent(feature, "Codex", 0);
  await bridge.setMode(ended, "Terminal");

  expect((await bridge.workspace(feature)).chat).toEqual([
    { agent: "Claude", session: ended, shown: true },
    { agent: "Codex", session: placed, shown: true },
  ]);
});
