import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { StrictMode, useState } from "react";
import { afterEach, beforeEach, expect, onTestFinished, test, vi } from "vitest";

import { App } from "./App";
import type { Agent, AgentState, StateSource, Workspace } from "./bridge";
import { DEFAULT_WORKSPACE, fakeBridge } from "./fakeBridge";
import appStyles from "./styles.css?raw";

// The diff view and xterm.js need a real browser. These tests cover only which panels exist and
// which ones are shown. ChatPanel.test.tsx covers the agent panel itself.
vi.mock("./DiffView", () => ({ DiffView: () => null }));
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
}: {
  session: string;
  agent: Agent;
  shown: boolean;
  focusRequest: number;
  onFocus?: () => void;
  onBlur?: () => void;
  onOpenDiff(): void;
  onChangeAgent?: (agent: Agent) => void;
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
      <input aria-label="Draft" value={draft} onChange={(event) => setDraft(event.target.value)} />
      <button onClick={onOpenDiff}>Review diff</button>
      <button onClick={() => onChangeAgent?.("Codex")}>Switch to Codex</button>
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

beforeEach(() => {
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

// An agent session's row in the sidebar, by the name it shows. Only a worktree with two or more
// sessions has them.
function agentRow(name: string) {
  return screen.getByText(name, { selector: ".session-name" }).closest("button")!;
}

// The state the status dot on a worktree's row names. null when it has none, as for Idle.
function rowState(worktree: string) {
  const row = screen.getByRole("button", { name: worktree }).closest(".wt-row");
  return row?.querySelector(".status-dot")?.getAttribute("aria-label") ?? null;
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

test("starting Claude Code in a worktree opens its panel, and the session nests under the worktree's row", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");

  startAgent();

  const panel = await screen.findByRole("region", { name: "Panel 0" });
  expect(panel.hidden).toBe(false);
  expect(bridge.spawnedAgents).toEqual([
    { session: "0", worktree: feature, agent: "Claude" },
  ]);
  expect(screen.getByRole("button", { name: "feat-36" }).getAttribute("aria-current")).toBe("true");
  expect(rowState("feat-36")).toBeNull();
  expect(agentRow("Claude Code")).toBeTruthy();
});

test.each<[string, AgentState, StateSource]>([
  ["Working", { state: "Working" }, "Hook"],
  ["Approval", { state: "NeedsInput", kind: "Approval" }, "Hook"],
  ["Question", { state: "NeedsInput", kind: "Question" }, "Hook"],
  ["Done", { state: "Done" }, "Hook"],
  ["Idle", { state: "Idle" }, "Heuristic"],
  ["Failed", { state: "Failed" }, "Heuristic"],
  ["Exited", { state: "Exited" }, "Hook"],
])("a worktree's row shows its one agent session's %s, without its source", async (word, state, source) => {
  const shown = word === "Idle" || word === "Exited" ? null : word;
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });

  act(() => bridge.agentStateChanged({ session: "0", ...state, source }));

  expect(rowState("feat-36")).toBe(shown);
  expect(screen.getByRole("button", { name: "feat-36" }).textContent).not.toMatch(source);
});

test("focusing a Done agent session's panel calls focus, and its worktree's row goes to Idle, with no dot", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  const panel = await screen.findByRole("region", { name: "Panel 0" });
  act(() => bridge.agentStateChanged({ session: "0", state: "Done", source: "Hook" }));

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
  startAgent();
  await screen.findByRole("region", { name: "Panel 1" });
  await select("main (main worktree)");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(true);

  await act(async () => fireEvent.click(agentRow("Claude Code")));

  expect(screen.getByRole("button", { name: "feat-36" }).getAttribute("aria-current")).toBe("true");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(screen.getByLabelText("Panel 1").hidden).toBe(false);
  expect(agentRow("Claude Code").getAttribute("aria-current")).toBe("true");
  expect(agentRow("Claude Code 2").getAttribute("aria-current")).toBe("true");
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
  expect(rowState("feat-36")).toBeNull();
});

test("a turn that ends after focus has left its panel stays Done", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  const panel = await screen.findByRole("region", { name: "Panel 0" });
  await act(async () => panel.focus());
  act(() => screen.getByRole("button", { name: "feat-36" }).focus());

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

test("the title bar collapses the sidebar and shows it again", async () => {
  render(<App bridge={await trackedBridge()} />);
  const toggle = screen.getByRole("button", { name: "Sidebar" });
  expect(screen.getByRole("complementary", { name: "Worktrees" })).toBeTruthy();

  fireEvent.click(toggle);

  expect(toggle.getAttribute("aria-pressed")).toBe("false");
  expect(screen.queryByRole("complementary", { name: "Worktrees" })).toBeNull();
  expect(screen.queryByRole("separator", { name: "Resize sidebar" })).toBeNull();

  fireEvent.click(toggle);

  expect(toggle.getAttribute("aria-pressed")).toBe("true");
  expect(await screen.findByRole("button", { name: "feat-36" })).toBeTruthy();
});

test("Cmd-K with the sidebar collapsed shows the sidebar and puts focus in Search", async () => {
  render(<App bridge={await trackedBridge()} />);
  fireEvent.click(screen.getByRole("button", { name: "Sidebar" }));

  fireEvent.keyDown(document.body, { key: "k", metaKey: true });

  const sidebar = screen.getByRole("complementary", { name: "Worktrees" });
  expect(document.activeElement).toBe(within(sidebar).getByRole("searchbox", { name: "Search" }));
  expect(screen.getByRole("button", { name: "Sidebar" }).getAttribute("aria-pressed")).toBe("true");
});

test("the title bar sits in the window's top row, over the chat area and the panel area", async () => {
  const style = document.createElement("style");
  style.textContent = appStyles;
  document.head.append(style);
  onTestFinished(() => style.remove());
  const { container } = render(<App bridge={await trackedBridge()} />);
  const box = (selector: string) => {
    const found = getComputedStyle(container.querySelector(selector)!);
    return [found.gridRow, found.gridColumn];
  };

  expect(box(".window-top")).toEqual(["1", "3 / -1"]);
  expect(box(".chat-area")).toEqual(["2", "3"]);
  expect(box(".sidebar-region")).toEqual(["1 / -1", "1"]);
});

test("the title bar collapses the panel area and shows it again", async () => {
  render(<App bridge={await trackedBridge()} />);
  await select("feat-36");
  const toggle = screen.getByRole("button", { name: "Panel area" });
  expect(screen.getByRole("tab", { name: "Files", selected: true })).toBeTruthy();

  fireEvent.click(toggle);

  expect(toggle.getAttribute("aria-pressed")).toBe("false");
  expect(screen.queryByRole("region", { name: "Panel area" })).toBeNull();
  expect(screen.queryByRole("separator", { name: "Resize panel area" })).toBeNull();

  fireEvent.click(toggle);

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

test("a resized sidebar and panel area keep their widths after a reload", async () => {
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
  });
});

test("an agent session that fails to start shows its error beside an agent panel already shown", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  vi.spyOn(bridge, "spawnAgent").mockRejectedValueOnce("out of ptys");

  startAgent();

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

test("the sidebar footer shows Claude's account, also one reported before the window opened", async () => {
  const bridge = await trackedBridge();
  const account = { email: "ann@example.com", organization: null, plan: "max", apiKeySource: null };
  bridge.accountChanged({ agent: "Claude", account });
  render(<App bridge={bridge} />);
  const footer = screen.getByRole("group", { name: "Account and settings" });
  expect(within(footer).getByText("ann@example.com · max plan")).toBeTruthy();

  act(() => bridge.accountChanged({ agent: "Claude", account: null }));

  expect(within(footer).getByText("Not logged in")).toBeTruthy();
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
  // The Codex session took the Claude one's place, so the worktree has one session.
  expect([...document.querySelectorAll(".session-row")].map((row) => row.textContent)).toEqual(["Codex"]);
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

// The menu items of the worktree named `name`. Leaves the menu open.
function menuItems(name: string) {
  fireEvent.click(screen.getByRole("button", { name: `Actions for ${name}` }));
  return screen.getAllByRole("menuitem").map((item) => item.textContent);
}

function needsYou() {
  return screen.getByRole("button", { name: /^Needs you/ });
}

// The Needs you row's count: the attention queue's length.
function waiting() {
  return needsYou().querySelector(".count")?.textContent;
}

async function pressJump() {
  await act(async () => fireEvent.keyDown(document.body, { key: "j", metaKey: true }));
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
  expect(screen.getByRole("button", { name: "main (main worktree)" }).getAttribute("aria-current")).toBe("true");
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
  await select("feat-36");
  // The approval ranks first, though the question came first.
  await act(async () =>
    bridge.agentStateChanged({ session: "0", state: "NeedsInput", kind: "Question", source: "Protocol" }),
  );
  await act(async () =>
    bridge.agentStateChanged({ session: "1", state: "NeedsInput", kind: "Approval", source: "Protocol" }),
  );
  expect(waiting()).toBe("2");

  await pressJump();

  expect(screen.getByRole("button", { name: "main (main worktree)" }).getAttribute("aria-current")).toBe("true");
  expect(screen.getByLabelText("Panel 1").hidden).toBe(false);
  expect(document.activeElement).toBe(screen.getByLabelText("Panel 1"));
  expect(bridge.focused).toEqual(["1"]);

  // The user answers, and the agent goes back to work.
  await act(async () => bridge.agentStateChanged({ session: "1", state: "Working", source: "Protocol" }));
  expect(waiting()).toBe("1");
  await pressJump();

  expect(screen.getByRole("button", { name: "feat-36" }).getAttribute("aria-current")).toBe("true");
  expect(screen.getByLabelText("Panel 0").hidden).toBe(false);
  expect(screen.getByLabelText("Panel 1").hidden).toBe(true);
  expect(document.activeElement).toBe(screen.getByLabelText("Panel 0"));
  expect(bridge.focused).toEqual(["1", "0"]);
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

  expect(screen.getByRole("button", { name: "feat-36" }).getAttribute("aria-current")).toBe("true");
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

  expect(screen.getByRole("button", { name: "feat-36" }).getAttribute("aria-current")).toBe("true");
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

  expect(screen.getByRole("button", { name: "main (main worktree)" }).getAttribute("aria-current")).toBe("true");
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

test("a new worktree opens selected, with Claude Code started in it", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  fireEvent.click(await screen.findByRole("button", { name: "New worktree in grove" }));
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
  expect(screen.getByRole("button", { name: "feat-52" }).getAttribute("aria-current")).toBe("true");
});

// A repo whose Cargo.toml gets it the Rust crate profile, with its setup script.
async function rustRepo() {
  const bridge = fakeBridge({ [main]: [{ path: main, branch: "main", isMain: true }] }, {}, { [main]: { "Cargo.toml": "" } });
  await bridge.addRepo(main);
  return bridge;
}

async function createWorktree(branch: string) {
  fireEvent.click(await screen.findByRole("button", { name: "New worktree in grove" }));
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
  expect(menuItems("feat-52")).toContain("Start Claude Code");
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
  fireEvent.click(await screen.findByRole("button", { name: "New worktree in grove" }));
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

test("a worktree deleted and made again on the same branch starts with no agent session or shell", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await createFeat52({ startAgent: true });
  await screen.findByRole("region", { name: "Panel 0" });
  fireEvent.click(await screen.findByRole("button", { name: "Open shell" }));
  await screen.findByRole("button", { name: "Close shell" });
  act(() => bridge.agentStateChanged({ session: "0", state: "Working", source: "Protocol" }));

  await deleteFeat52();

  await createFeat52({ startAgent: false });

  expect(screen.getByRole("button", { name: "feat-52" }).getAttribute("aria-current")).toBe("true");
  expect(await screen.findByRole("button", { name: "Open shell" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Close shell" })).toBeNull();
  expect(rowState("feat-52")).toBeNull();
  expect(screen.queryByRole("region", { name: "Panel 0" })).toBeNull();
  // The deleted worktree's Working session ended with it, so Cmd-Q quits at once.
  act(() => bridge.requestQuit());
  await expect.poll(() => bridge.quitCount()).toBe(1);
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

  expect(within(detailsCard()).getByText(feature)).toBeTruthy();
  expect(button.getAttribute("aria-pressed")).toBe("true");

  // Its own button closes it, rather than a click outside that the button then opens again.
  fireEvent.pointerDown(button);
  await act(async () => fireEvent.click(button));
  expect(screen.queryByRole("region", { name: "Details" })).toBeNull();
  expect(button.getAttribute("aria-pressed")).toBe("false");

  await act(async () => fireEvent.click(button));
  fireEvent.keyDown(document.body, { key: "Escape" });
  expect(screen.queryByRole("region", { name: "Details" })).toBeNull();

  await act(async () => fireEvent.click(button));
  fireEvent.pointerDown(screen.getByRole("button", { name: "feat-36" }));
  expect(screen.queryByRole("region", { name: "Details" })).toBeNull();
});

test("the Details card stays closed after its worktree is deleted, until it is opened again", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await createFeat52({ startAgent: false });
  await clickButton("Details");
  expect(detailsCard()).toBeTruthy();

  await deleteFeat52();
  await select("feat-36");

  expect(screen.queryByRole("region", { name: "Details" })).toBeNull();
  expect(screen.getByRole("button", { name: "Details" }).getAttribute("aria-pressed")).toBe("false");
});

test("Start and Stop in the Details card's scripts row follow the run session's state", async () => {
  const bridge = await runBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  await clickButton("Details");
  const dev = () => within(detailsCard()).getByRole("listitem", { name: "dev" });

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

  const dev = within(detailsCard()).getByRole("listitem", { name: "dev" });
  expect(within(dev).getByRole("button", { name: "Stop dev" })).toBeTruthy();
  const servers = within(within(detailsCard()).getByRole("group", { name: "Servers" })).getAllByRole("listitem");
  expect(servers.map((item) => item.textContent)).toEqual(["devlocalhost:5173"]);
});

test("a run session's tab shows at once while its port is still being read", async () => {
  const bridge = await runBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  vi.spyOn(bridge, "runSessions").mockReturnValue(new Promise(() => {}));

  await clickButton("Start dev");

  expect(terminalTab("dev running").getAttribute("aria-selected")).toBe("true");
});

test("the Details card's session row shows the worktree's agent session and where its state comes from", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-36");
  startAgent();
  await screen.findByRole("region", { name: "Panel 0" });
  act(() => bridge.agentStateChanged({ session: "0", state: "Working", source: "Protocol" }));

  await clickButton("Details");

  const session = within(detailsCard()).getByRole("group", { name: "Session" });
  expect(within(session).getByText("Claude Code")).toBeTruthy();
  expect(within(session).getByText("State from Protocol")).toBeTruthy();
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

  const session = within(detailsCard()).getByRole("group", { name: "Session" });
  expect(within(session).getByText("Codex")).toBeTruthy();
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

test("the title bar's switches are in order: details card, terminal, panel area", async () => {
  render(<App bridge={await trackedBridge()} />);
  const titleBar = screen.getByRole("banner");

  const names = within(titleBar)
    .getAllByRole("button")
    .map((button) => button.getAttribute("aria-label"));

  expect(names.slice(-3)).toEqual(["Details", "Terminal", "Panel area"]);
  expect(within(titleBar).queryByRole("button", { name: /waiting|Settings|Appearance/ })).toBeNull();
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
  startAgent();
  await screen.findByRole("region", { name: "Panel 1" });
  await select("main (main worktree)");

  await act(async () => fireEvent.click(agentRow("Claude Code 2")));

  expect(screen.getByLabelText("Panel 1").hidden).toBe(false);
  expect(document.activeElement).toBe(screen.getByLabelText("Panel 1"));
  expect(bridge.focused).toEqual(["1"]);
});
