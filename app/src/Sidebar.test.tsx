import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { createRef } from "react";
import { afterEach, expect, onTestFinished, test, vi } from "vitest";

import type { AgentStateChanged, Worktree } from "./bridge";
import { fakeBridge, type FakeBridge } from "./fakeBridge";
import sidebarStyles from "./styles.css?raw";
import { Sidebar, type SidebarHandle } from "./Sidebar";
import agentStateStyles from "./ui/AgentState.css?raw";
import { menuItems } from "./testing";

afterEach(() => {
  cleanup();
  localStorage.clear();
});

const groveRepo = [
  { path: "/code/grove", branch: "main", isMain: true },
  { path: "/code/grove-wt/feat-35", branch: "feat-35-tauri-sidebar", isMain: false },
  { path: "/code/grove-wt/spike", branch: null, isMain: false },
];

// These tests start no agent session, and nothing waits.
const noAgents = {
  agents: [],
  agentStates: {},
  shownAgents: [],
  onStartAgent() {},
  onStartAgentAfterSetup() {},
  onDeleting() {},
  onRemoved() {},
  onShowAgent() {},
  onFocusAgent() {},
  waiting: 0,
  onJump() {},
  onHideSidebar() {},
  onOpenProfiles() {},
  accounts: {},
  appearance: { theme: "system" as const, palette: "default" as const, appIcon: "auto" as const },
  onAppearance() {},
};

// Picks `path` in the folder picker that Add repo opens. null cancels it.
function addRepo(bridge: FakeBridge, path: string | null) {
  bridge.willPickFolder(path);
  fireEvent.click(screen.getByRole("button", { name: "Add repo" }));
}

// Each worktree row's name and the path its tooltip shows, in order.
function listed(repo: HTMLElement) {
  return within(repo)
    .getAllByRole("button")
    .filter((button) => button.classList.contains("wt-name"))
    .map((button) => [button.textContent, button.closest("li")!.title]);
}

test("picking a folder in Add repo's picker adds it and lists its worktrees, main worktree first", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  expect(screen.queryByRole("textbox", { name: "Repo path" })).toBeNull();

  addRepo(bridge, "/code/grove");

  const repo = await screen.findByRole("region", { name: "grove" });
  expect(listed(repo)).toEqual([
    ["main (main worktree)", "/code/grove"],
    ["feat-35-tauri-sidebar", "/code/grove-wt/feat-35"],
    ["spike", "/code/grove-wt/spike"],
  ]);
});

test("a folder that is not a repo shows the core's error and lists nothing", async () => {
  const bridge = fakeBridge({});
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);

  addRepo(bridge, "/code/not-a-repo");

  expect((await screen.findByRole("alert")).textContent).toBe("/code/not-a-repo is not a git repo");
  expect(screen.queryAllByRole("listitem")).toEqual([]);
});

test("opening the sidebar lists the worktrees the core already tracks", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");

  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);

  const repo = await screen.findByRole("region", { name: "grove" });
  expect(listed(repo)).toEqual([
    ["main (main worktree)", "/code/grove"],
    ["feat-35-tauri-sidebar", "/code/grove-wt/feat-35"],
    ["spike", "/code/grove-wt/spike"],
  ]);
});

test("clicking a worktree selects it, and the selected one is marked", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  const clicked: (string | null)[] = [];
  const { rerender } = render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={(path) => clicked.push(path)}
      {...noAgents}
    />,
  );
  addRepo(bridge, "/code/grove");

  fireEvent.click(await screen.findByRole("button", { name: "feat-35-tauri-sidebar" }));
  rerender(<Sidebar bridge={bridge} selected={clicked[0]} onSelect={() => {}} {...noAgents} />);

  expect(clicked).toEqual(["/code/grove-wt/feat-35"]);
  expect(screen.getByRole("button", { current: true })).toBe(
    screen.getByRole("button", { name: "feat-35-tauri-sidebar" }),
  );
});

test("session rows name the agent, numbered from the second, show the state without its source, and never the session id", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const feature = "/code/grove-wt/feat-35";
  const agents = [
    { session: "7731", worktree: feature, agent: "Claude" as const, panel: "7731" },
    { session: "7732", worktree: feature, agent: "Codex" as const, panel: "7732" },
    { session: "7733", worktree: feature, agent: "Claude" as const, panel: "7733" },
  ];

  render(
    <Sidebar
      bridge={bridge}
      selected={feature}
      onSelect={() => {}}
      {...noAgents}
      agents={agents}
      agentStates={{
        "7731": { session: "7731", state: "NeedsInput", kind: "Approval", source: "Hook" },
        "7733": { session: "7733", state: "Working", source: "Protocol" },
      }}
    />,
  );
  await screen.findByRole("region", { name: "grove" });
  const rows = sessionRows();

  expect(rows.map((row) => row.textContent)).toEqual(["Claude Code", "Codex", "Claude Code 2"]);
  expect(rows.map(dot)).toEqual(["Approval", null, "Working"]);
  const sidebar = screen.getByRole("complementary", { name: "Worktrees" });
  expect(sidebar.textContent).not.toMatch(/Hook|Protocol|773\d/);
  expect(sidebar.innerHTML).not.toMatch(/773\d/);
});
const feature = "/code/grove-wt/feat-35";

// The button that selects a worktree. It holds the name.
async function worktreeRow(name: string) {
  return await screen.findByRole("button", { name });
}

// The nested agent session rows, in order.
function sessionRows() {
  return screen.getAllByRole("button").filter((button) => button.classList.contains("session-row"));
}

// The state a row's status dot names, or null when it has none. A worktree's dot sits beside its
// button, which the dot describes. A session's dot sits in its button.
function dot(row: HTMLElement) {
  const own = row.querySelector(".status-dot") ?? row.closest(".wt-row")?.querySelector(".status-dot");
  return own?.getAttribute("aria-label") ?? null;
}

test("a worktree with a session in Approval and one Working shows the Approval dot", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");

  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agents={[
        { session: "1", worktree: feature, agent: "Claude", panel: "1" },
        { session: "2", worktree: feature, agent: "Codex", panel: "2" },
      ]}
      agentStates={{
        "1": { session: "1", state: "Working", source: "Protocol" },
        "2": { session: "2", state: "NeedsInput", kind: "Approval", source: "Hook" },
      }}
    />,
  );

  const row = await worktreeRow("feat-35-tauri-sidebar");
  expect(dot(row)).toBe("Approval");
  // The dot's word is its tooltip, and a screen reader reads it after the name.
  expect(row.closest(".wt-row")!.querySelector(".status-dot")!.getAttribute("title")).toBe("Approval");
  expect(screen.getByRole("button", { name: "feat-35-tauri-sidebar", description: "Approval" })).toBe(row);
});

test.each([
  ["Idle", { state: "Idle" as const }, null],
  ["Exited", { state: "Exited" as const }, null],
  ["Done", { state: "Done" as const }, "Done"],
  ["Failed", { state: "Failed" as const }, "Failed"],
  ["Question", { state: "NeedsInput" as const, kind: "Question" as const }, "Question"],
])("a worktree whose session is %s shows the dot %s", async (_, state, shown) => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");

  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agents={[{ session: "1", worktree: feature, agent: "Claude", panel: "1" }]}
      agentStates={{ "1": { session: "1", ...state, source: "Protocol" } }}
    />,
  );

  expect(dot(await worktreeRow("feat-35-tauri-sidebar"))).toBe(shown);
});
test("a worktree's one session nests under its row, and clicking the worktree shows that session", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const only = { session: "1", worktree: feature, agent: "Claude" as const, panel: "1" };
  const shown: unknown[] = [];

  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agents={[only]}
      agentStates={{ "1": { session: "1", state: "Idle", source: "Protocol" } }}
      onShowAgent={(agent) => shown.push(agent)}
    />,
  );
  fireEvent.click(await worktreeRow("feat-35-tauri-sidebar"));

  expect(sessionRows().map((row) => row.textContent)).toEqual(["Claude Code"]);
  expect(shown).toEqual([only]);
});
test("two agent sessions in a worktree show as two nested rows, and clicking the second focuses it", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const agents = [
    { session: "1", worktree: feature, agent: "Claude" as const, panel: "1" },
    { session: "2", worktree: feature, agent: "Claude" as const, panel: "2" },
  ];
  const shown: unknown[] = [];

  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} agents={agents} onFocusAgent={(agent) => shown.push(agent)} />);
  await worktreeRow("feat-35-tauri-sidebar");

  expect(sessionRows().map((row) => row.textContent)).toEqual(["Claude Code", "Claude Code 2"]);
  fireEvent.click(sessionRows()[1]);
  expect(shown).toEqual([agents[1]]);
});

test("a worktree's chevron folds and unfolds its session rows", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agents={[{ session: "1", worktree: feature, agent: "Claude", panel: "1" }]}
    />,
  );
  await worktreeRow("feat-35-tauri-sidebar");
  const chevron = screen.getByRole("button", { name: "Sessions of feat-35-tauri-sidebar" });
  expect(chevron.getAttribute("aria-expanded")).toBe("true");

  fireEvent.click(chevron);
  expect(chevron.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryAllByRole("button").filter((b) => b.classList.contains("session-row"))).toEqual([]);

  fireEvent.click(chevron);
  expect(sessionRows()).toHaveLength(1);
  // Only a worktree with sessions has one.
  expect(screen.queryByRole("button", { name: "Sessions of spike" })).toBeNull();
});
test("a worktree whose setup script runs shows Setup running in place of its state", async () => {
  const repo = [
    { path: "/code/grove", branch: "main", isMain: true },
    { path: feature, branch: "feat-35-tauri-sidebar", isMain: false, setup: { state: "Running" as const } },
  ];
  const bridge = fakeBridge({ "/code/grove": repo });
  await bridge.addRepo("/code/grove");

  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agents={[{ session: "1", worktree: feature, agent: "Claude", panel: "1" }]}
      agentStates={{ "1": { session: "1", state: "Working", source: "Protocol" } }}
    />,
  );
  const row = await worktreeRow("feat-35-tauri-sidebar");

  expect(dot(row)).toBe("Setup running");
  expect(screen.getAllByRole("img", { name: "Setup running" })).toHaveLength(1);
});

test("the top rows fit the sidebar's width, so each key hint shows in full", async () => {
  const style = document.createElement("style");
  style.textContent = sidebarStyles;
  document.head.append(style);
  onTestFinished(() => style.remove());
  render(<Sidebar bridge={fakeBridge({})} selected={null} onSelect={() => {}} {...noAgents} />);
  const nav = getComputedStyle(screen.getByRole("navigation", { name: "Sidebar" }));
  const search = getComputedStyle(screen.getByRole("searchbox", { name: "Search" }));

  // An auto column grows to the Search box's own width and pushes the hints past the edge.
  expect(nav.gridTemplateColumns).toBe("minmax(0, 1fr)");
  expect([search.minWidth, search.width]).toEqual(["0px", "0px"]);
});

test("a long worktree name is cut and fades out, and its dot is never squeezed", async () => {
  const style = document.createElement("style");
  style.textContent = sidebarStyles;
  document.head.append(style);
  onTestFinished(() => style.remove());
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");

  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agents={[{ session: "1", worktree: feature, agent: "Claude", panel: "1" }]}
      agentStates={{ "1": { session: "1", state: "NeedsInput", kind: "Question", source: "Hook" } }}
    />,
  );
  const row = await worktreeRow("feat-35-tauri-sidebar");
  const title = getComputedStyle(row.querySelector(".wt-title")!);
  const statusDot = getComputedStyle(row.closest(".wt-row")!.querySelector(".status-dot")!);

  expect([title.overflow, title.whiteSpace, title.minWidth]).toEqual(["hidden", "nowrap", "0px"]);
  expect(statusDot.flexShrink).toBe("0");
});
test("Tab moves from a worktree's row to its chevron, its menu, then its session rows", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");

  render(
    <Sidebar
      bridge={bridge}
      selected={feature}
      onSelect={() => {}}
      {...noAgents}
      agents={[
        { session: "1", worktree: feature, agent: "Claude", panel: "1" },
        { session: "2", worktree: feature, agent: "Codex", panel: "2" },
      ]}
    />,
  );
  const row = await worktreeRow("feat-35-tauri-sidebar");
  const item = row.closest("li")!;

  // No element in the sidebar sets tabIndex, so the order of its buttons is the Tab order.
  expect(within(item).getAllByRole("button")).toEqual([
    row,
    screen.getByRole("button", { name: "Sessions of feat-35-tauri-sidebar" }),
    screen.getByRole("button", { name: "Actions for feat-35-tauri-sidebar" }),
    screen.getByRole("button", { name: "Claude Code" }),
    screen.getByRole("button", { name: "Codex" }),
  ]);
  expect(item.querySelector("[tabindex]")).toBeNull();
});
// The sidebar's frame: the rows at its top and its footer (#29).

const siteRepo = [
  { path: "/code/site", branch: "main", isMain: true },
  { path: "/code/site-wt/fix-12", branch: "fix-12-pty", isMain: false, displayName: "Terminal resize" },
];

async function twoRepos() {
  const bridge = fakeBridge({ "/code/grove": groveRepo, "/code/site": siteRepo });
  await bridge.addRepo("/code/grove");
  await bridge.addRepo("/code/site");
  return bridge;
}

function search(text: string) {
  fireEvent.change(screen.getByRole("searchbox", { name: "Search" }), { target: { value: text } });
}

const rowNames = () =>
  [...document.querySelectorAll(".wt-title")].map((title) => title.textContent);

test("Search hides rows whose display name and branch do not match, and clearing it shows them all", async () => {
  render(<Sidebar bridge={await twoRepos()} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("Terminal resize");
  const all = rowNames();

  // By branch.
  search("PTY");
  expect(rowNames()).toEqual(["Terminal resize"]);
  expect(screen.queryByRole("region", { name: "grove" })).toBeNull();

  // By display name.
  search("resize");
  expect(rowNames()).toEqual(["Terminal resize"]);

  search("feat-35");
  expect(rowNames()).toEqual(["feat-35-tauri-sidebar"]);

  search("");
  expect(rowNames()).toEqual(all);
  expect(all).toHaveLength(5);
});

test("Cmd-K puts focus in Search, and Escape there clears it", async () => {
  render(<Sidebar bridge={await twoRepos()} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("Terminal resize");
  const box = screen.getByRole("searchbox", { name: "Search" });

  fireEvent.keyDown(document.body, { key: "k", metaKey: true });
  expect(document.activeElement).toBe(box);

  search("resize");
  fireEvent.keyDown(box, { key: "Escape" });
  expect((box as HTMLInputElement).value).toBe("");
  expect(rowNames()).toHaveLength(5);
});

test("New worktree and Cmd-N open the new worktree dialog for the selected worktree's repo", async () => {
  const bridge = await twoRepos();
  const asked = vi.spyOn(bridge, "suggestProfile");
  render(<Sidebar bridge={bridge} selected="/code/site-wt/fix-12" onSelect={() => {}} {...noAgents} />);
  await worktreeRow("Terminal resize");

  fireEvent.click(screen.getByRole("button", { name: "New worktree" }));
  const dialog = await screen.findByRole("dialog", { name: "New worktree" });
  expect(asked).toHaveBeenLastCalledWith("/code/site");
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  await expect.poll(() => screen.queryByRole("dialog")).toBeNull();

  fireEvent.keyDown(document.body, { key: "n", metaKey: true });
  expect(await screen.findByRole("dialog", { name: "New worktree" })).toBeTruthy();
  expect(asked).toHaveBeenCalledTimes(2);
});

test("with no worktree selected, New worktree uses the first repo, and with no repo it is off", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  const asked = vi.spyOn(bridge, "suggestProfile");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  expect(screen.getByRole("button", { name: "New worktree" })).toHaveProperty("disabled", true);

  addRepo(bridge, "/code/grove");
  await worktreeRow("feat-35-tauri-sidebar");
  fireEvent.click(screen.getByRole("button", { name: "New worktree" }));

  await screen.findByRole("dialog", { name: "New worktree" });
  expect(asked).toHaveBeenLastCalledWith("/code/grove");
});

test("Needs you shows the attention queue's length, and clicking it jumps", async () => {
  const jumps: number[] = [];
  render(
    <Sidebar
      bridge={fakeBridge({})}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      waiting={3}
      onJump={() => jumps.push(1)}
    />,
  );
  const needsYou = screen.getByRole("button", { name: /^Needs you/ });

  expect(needsYou.querySelector(".count")?.textContent).toBe("3");
  fireEvent.click(needsYou);
  expect(jumps).toEqual([1]);
});

test("Profiles opens the Profile panel, and is off with no worktree selected", () => {
  const opened: number[] = [];
  const { rerender } = render(
    <Sidebar
      bridge={fakeBridge({})}
      selected="/code/grove"
      onSelect={() => {}}
      {...noAgents}
      onOpenProfiles={() => opened.push(1)}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Profiles" }));
  expect(opened).toEqual([1]);

  rerender(<Sidebar bridge={fakeBridge({})} selected={null} onSelect={() => {}} {...noAgents} />);
  expect(screen.getByRole("button", { name: "Profiles" })).toHaveProperty("disabled", true);
});

test("the footer shows each agent's account, or that nobody is logged in", () => {
  render(
    <Sidebar
      bridge={fakeBridge({})}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      accounts={{
        Claude: { email: "ann@example.com", organization: null, plan: "max", apiKeySource: null },
        Codex: null,
      }}
    />,
  );
  const footer = screen.getByRole("group", { name: "Account and settings" });

  expect(within(footer).getByText("ann@example.com · max plan").closest("li")?.title).toBe("Claude Code");
  expect(within(footer).getByText("Not logged in").closest("li")?.title).toBe("Codex");
});

test("an account with an API key and no plan names the key's source", () => {
  const account = { email: null, organization: "Ann's org", plan: null, apiKeySource: "ANTHROPIC_API_KEY" };
  render(
    <Sidebar bridge={fakeBridge({})} selected={null} onSelect={() => {}} {...noAgents} accounts={{ Claude: account }} />,
  );

  const footer = screen.getByRole("group", { name: "Account and settings" });
  expect(within(footer).getByRole("listitem").textContent).toBe("Ann's org · API key from ANTHROPIC_API_KEY");
});

test("Settings in the footer opens the appearance menu, which changes palette, theme and app icon", () => {
  const appearances: unknown[] = [];
  render(
    <Sidebar
      bridge={fakeBridge({})}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      onAppearance={(update) => appearances.push(update(noAgents.appearance))}
    />,
  );
  const footer = screen.getByRole("group", { name: "Account and settings" });

  fireEvent.click(within(footer).getByRole("button", { name: "Settings" }));
  fireEvent.click(within(footer).getByRole("button", { name: "Modern" }));
  fireEvent.click(within(footer).getByRole("button", { name: "Light" }));
  fireEvent.click(within(footer).getByRole("button", { name: "Blue" }));

  expect(appearances).toEqual([
    { theme: "system", palette: "modern", appIcon: "auto" },
    { theme: "light", palette: "default", appIcon: "auto" },
    { theme: "system", palette: "default", appIcon: "blue" },
  ]);
});

// The tree: repo folders, worktree rows and nested agent sessions (#32).

function folder(name: string) {
  return screen.getByRole("button", { name });
}

test("a repo folds and unfolds, and its fold state survives a remount", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const { unmount } = render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("feat-35-tauri-sidebar");
  expect(folder("grove").getAttribute("aria-expanded")).toBe("true");

  fireEvent.click(folder("grove"));

  expect(folder("grove").getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByRole("button", { name: "feat-35-tauri-sidebar" })).toBeNull();
  unmount();

  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await screen.findByRole("region", { name: "grove" });
  expect(folder("grove").getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByRole("button", { name: "feat-35-tauri-sidebar" })).toBeNull();

  fireEvent.click(folder("grove"));
  expect(await worktreeRow("feat-35-tauri-sidebar")).toBeTruthy();
});

test("Search shows the matching rows of a folded repo", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("feat-35-tauri-sidebar");
  fireEvent.click(folder("grove"));

  search("tauri");

  expect(rowNames()).toEqual(["feat-35-tauri-sidebar"]);
});

test("a repo folder cannot fold while Search has text, since Search keeps it open", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("feat-35-tauri-sidebar");
  search("tauri");

  fireEvent.click(folder("grove"));
  search("");

  expect(folder("grove").getAttribute("aria-expanded")).toBe("true");
  expect(rowNames()).toContain("feat-35-tauri-sidebar");
});

test("a repo folder's + opens the new worktree dialog for that repo", async () => {
  const bridge = await twoRepos();
  const asked = vi.spyOn(bridge, "suggestProfile");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("Terminal resize");

  fireEvent.click(screen.getByRole("button", { name: "New worktree in site" }));

  await screen.findByRole("dialog", { name: "New worktree" });
  expect(asked).toHaveBeenLastCalledWith("/code/site");
});


test("the menu keeps every action: Start Claude Code, Change base branch, Forget and Delete", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("feat-35-tauri-sidebar");

  expect(menuItems("feat-35-tauri-sidebar")).toEqual(["Start Claude Code", "Change base branch", "Forget", "Delete"]);
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(menuItems("main")).toEqual(["Start Claude Code", "Change base branch"]);
});

test("Start Claude Code in a worktree's menu selects the worktree and starts a session in it", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const selected: (string | null)[] = [];
  const started: string[] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={(path) => selected.push(path)}
      {...noAgents}
      onStartAgent={(path) => started.push(path)}
    />,
  );
  await worktreeRow("feat-35-tauri-sidebar");

  menuItems("feat-35-tauri-sidebar");
  fireEvent.click(screen.getByRole("menuitem", { name: "Start Claude Code" }));

  expect(selected).toEqual([feature]);
  expect(started).toEqual([feature]);
});

test("a worktree whose setup script runs has no Start Claude Code in its menu", async () => {
  const repo = [
    { path: "/code/grove", branch: "main", isMain: true },
    { path: feature, branch: "feat-35-tauri-sidebar", isMain: false, setup: { state: "Running" as const } },
  ];
  const bridge = fakeBridge({ "/code/grove": repo });
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("feat-35-tauri-sidebar");

  expect(menuItems("feat-35-tauri-sidebar")).toEqual(["Change base branch", "Forget", "Delete"]);
});

test("the selected worktree's row is marked, and a Working dot pulses", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  render(
    <Sidebar
      bridge={bridge}
      selected={feature}
      onSelect={() => {}}
      {...noAgents}
      agents={[{ session: "1", worktree: feature, agent: "Claude", panel: "1" }]}
      agentStates={{ "1": { session: "1", state: "Working", source: "Protocol" } }}
    />,
  );
  const row = await worktreeRow("feat-35-tauri-sidebar");

  expect(row.closest(".wt-row")!.classList.contains("selected")).toBe(true);
  expect(row.closest(".wt-row")!.querySelector(".status-dot")!.classList.contains("status-working")).toBe(true);
  expect(sidebarStyles).toMatch(/\.status-working\s*\{[^}]*animation/);
  expect(sidebarStyles).toMatch(/prefers-reduced-motion[\s\S]*\.status-working[^}]*animation: none/);
});

test("the status dot pulses with the Working icon's one keyframe and timing", () => {
  const animation = (css: string, selector: string) =>
    css.match(new RegExp(`${selector}\\s*\\{[^}]*?animation: ([^;]+);`))?.[1];

  expect(animation(sidebarStyles, "\\.status-working")).toBe("ui-agent-state-pulse var(--working-pulse)");
  expect(animation(agentStateStyles, "\\.ui-agent-state-working svg")).toBe("ui-agent-state-pulse var(--working-pulse)");
  expect(sidebarStyles).not.toMatch(/@keyframes status-pulse/);
});

// Add repo with a folder picker (#17).

test("cancelling the folder picker adds nothing and shows no error", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  const added = vi.spyOn(bridge, "addRepo");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);

  addRepo(bridge, null);

  await expect.poll(() => bridge.foldersPicked).toBe(1);
  expect(added).not.toHaveBeenCalled();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByRole("region", { name: "grove" })).toBeNull();
});

test("a worktree list that answers late never replaces a newer one", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  const listWorktrees = bridge.listWorktrees.bind(bridge);
  let answerFirst: (worktrees: Worktree[]) => void = () => {};
  vi.spyOn(bridge, "listWorktrees")
    .mockImplementationOnce(() => new Promise((resolve) => (answerFirst = resolve)))
    .mockImplementation(listWorktrees);
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  addRepo(bridge, "/code/grove");
  await screen.findByRole("region", { name: "grove" });

  // The list asked for at start, before the repo was added, answers last.
  await act(async () => answerFirst([]));

  expect(screen.getByRole("region", { name: "grove" })).toBeTruthy();
});

test("a repo added after an error clears the error", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  addRepo(bridge, "/code/not-a-repo");
  await screen.findByRole("alert");

  addRepo(bridge, "/code/grove");

  await screen.findByRole("region", { name: "grove" });
  expect(screen.queryByRole("alert")).toBeNull();
});

test("the sidebar's addRepo handle opens the picker and adds the folder, as Add repo does", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  const handle = createRef<SidebarHandle>();
  render(<Sidebar ref={handle} bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  bridge.willPickFolder("/code/grove");

  act(() => handle.current!.addRepo());

  expect(await screen.findByRole("region", { name: "grove" })).toBeTruthy();
  expect(bridge.foldersPicked).toBe(1);
});

// Each worktree row's name and the path of the worktree row it nests under, null at the top level.
function nesting() {
  return screen
    .getAllByRole("button")
    .filter((button) => button.classList.contains("wt-name"))
    .map((button) => [button.textContent, button.closest("li")!.parentElement!.closest("li")?.title ?? null]);
}

const nestingRepo = [
  { path: "/code/grove", branch: "main", isMain: true },
  { path: "/code/grove-wt/feat-35", branch: "feat-35", isMain: false, nestedUnder: "/code/grove-wt/plan" },
  { path: "/code/grove-wt/plan", branch: "plan", isMain: false },
  { path: "/code/grove-wt/deep", branch: "deep", isMain: false, nestedUnder: "/code/grove-wt/feat-35" },
  { path: "/code/grove-wt/spike", branch: "spike", isMain: false, nestedUnder: "/code/other-repo" },
  { path: "/code/grove-wt/loop-a", branch: "loop-a", isMain: false, nestedUnder: "/code/grove-wt/loop-b" },
  { path: "/code/grove-wt/loop-b", branch: "loop-b", isMain: false, nestedUnder: "/code/grove-wt/loop-a" },
];

test("a nested worktree shows one level deep under the top worktree of its chain", async () => {
  const bridge = fakeBridge({ "/code/grove": nestingRepo });
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await screen.findByRole("region", { name: "grove" });

  // A worktree of another repo, or a loop, nests nothing. An old state file may hold either.
  expect(nesting()).toEqual([
    ["main (main worktree)", null],
    ["plan", null],
    ["feat-35", "/code/grove-wt/plan"],
    ["deep", "/code/grove-wt/plan"],
    ["spike", null],
    ["loop-a", null],
    ["loop-b", null],
  ]);
  fireEvent.click(screen.getByRole("button", { name: "Sessions of plan" }));
  expect(nesting().map(([name]) => name)).not.toContain("feat-35");
});

test("a nested worktree's row starts right of the tree line, so the line never crosses its ring", async () => {
  const style = document.createElement("style");
  style.textContent = sidebarStyles;
  document.head.append(style);
  onTestFinished(() => style.remove());
  const bridge = fakeBridge({ "/code/grove": nestingRepo });
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected="/code/grove-wt/feat-35" onSelect={() => {}} {...noAgents} />);
  const row = (await worktreeRow("feat-35")).closest<HTMLElement>(".wt-row")!;

  const box = getComputedStyle(row);
  expect([box.marginLeft, box.paddingLeft]).toEqual(["calc(var(--space-32) - var(--space-2))", "0"]);
});

test("a nested worktree stays nested after the agent session that nested it ended", async () => {
  const bridge = fakeBridge({ "/code/grove": nestingRepo });
  await bridge.addRepo("/code/grove");
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agents={[{ session: "1", worktree: "/code/grove-wt/plan", agent: "Claude", panel: "1" }]}
      agentStates={{ "1": { session: "1", state: "Exited", source: "Protocol" } }}
    />,
  );
  await screen.findByRole("region", { name: "grove" });

  expect(nesting()).toContainEqual(["feat-35", "/code/grove-wt/plan"]);
});

test("a nested worktree keeps its own agent session rows, nested under its row", async () => {
  const bridge = fakeBridge({ "/code/grove": nestingRepo });
  await bridge.addRepo("/code/grove");
  const nested = "/code/grove-wt/feat-35";
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agents={[
        { session: "1", worktree: nested, agent: "Claude", panel: "1" },
        { session: "2", worktree: nested, agent: "Codex", panel: "2" },
      ]}
    />,
  );
  await screen.findByRole("region", { name: "grove" });

  expect(sessionRows().map((row) => [row.textContent, row.closest("ul")!.closest("li")!.title])).toEqual([
    ["Claude Code", nested],
    ["Codex", nested],
  ]);
  fireEvent.click(screen.getByRole("button", { name: "Sessions of feat-35" }));
  expect(screen.queryAllByRole("button").filter((b) => b.classList.contains("session-row"))).toEqual([]);
  expect(nesting()).toContainEqual(["feat-35", "/code/grove-wt/plan"]);
});

test("the first state of a session the sidebar has not seen lists the worktrees again", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const sidebar = (agentStates: Record<string, AgentStateChanged>) => (
    <Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} agentStates={agentStates} />
  );
  const { rerender } = render(sidebar({}));
  await screen.findByRole("region", { name: "grove" });
  expect(nesting()).toContainEqual(["feat-35-tauri-sidebar", null]);

  // A subagent of a session in the main worktree starts in feat-35, which nests it there.
  bridge.nestWorktree(feature, "/code/grove");
  rerender(sidebar({ "9": { session: "9", state: "Working", source: "Protocol" } }));

  await vi.waitFor(() => expect(nesting()).toContainEqual(["feat-35-tauri-sidebar", "/code/grove"]));
});
