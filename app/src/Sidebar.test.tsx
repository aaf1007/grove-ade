import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, onTestFinished, test, vi } from "vitest";

import { fakeBridge } from "./fakeBridge";
import sidebarStyles from "./styles.css?raw";
import agentStateStyles from "./ui/AgentState.css?raw";
import { Sidebar } from "./Sidebar";

afterEach(cleanup);

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
  waiting: 0,
  onJump() {},
  onOpenProfiles() {},
  accounts: {},
  appearance: { theme: "system" as const, palette: "default" as const },
  onAppearance() {},
};

function addRepo(path: string) {
  fireEvent.change(screen.getByLabelText("Repo path"), { target: { value: path } });
  fireEvent.click(screen.getByRole("button", { name: "Add repo" }));
}

// Each worktree row's name and its branch line, in order.
function listed(repo: HTMLElement) {
  return within(repo)
    .getAllByRole("listitem")
    .map((row) => {
      const button = within(row).getAllByRole("button")[0];
      return [document.getElementById(button.getAttribute("aria-labelledby")!)!.textContent, button.querySelector(".wt-meta")!.textContent];
    });
}

test("adding a repo lists its worktrees, main worktree first", async () => {
  render(
    <Sidebar
      bridge={fakeBridge({ "/code/grove": groveRepo })}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
    />,
  );

  addRepo("/code/grove");

  const repo = await screen.findByRole("region", { name: "grove" });
  expect(listed(repo)).toEqual([
    ["main (main worktree)", "main"],
    ["feat-35-tauri-sidebar", "feat-35-tauri-sidebar"],
    ["spike", "detached HEAD"],
  ]);
});

test("a repo the core rejects shows its error and lists nothing", async () => {
  render(<Sidebar bridge={fakeBridge({})} selected={null} onSelect={() => {}} {...noAgents} />);

  addRepo("/code/not-a-repo");

  expect((await screen.findByRole("alert")).textContent).toBe("/code/not-a-repo is not a git repo");
  expect(screen.queryAllByRole("listitem")).toEqual([]);
});

test("opening the sidebar lists the worktrees the core already tracks", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");

  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);

  const repo = await screen.findByRole("region", { name: "grove" });
  expect(listed(repo)).toEqual([
    ["main (main worktree)", "main"],
    ["feat-35-tauri-sidebar", "feat-35-tauri-sidebar"],
    ["spike", "detached HEAD"],
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
  addRepo("/code/grove");

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
  const rows = within(await screen.findByRole("region", { name: "grove" }))
    .getAllByRole("button")
    .filter((button) => button.classList.contains("session-row"));

  expect(rows.map((row) => row.textContent)).toEqual(["Claude Code Approval", "Codex ", "Claude Code 2 Working"]);
  const sidebar = screen.getByRole("complementary", { name: "Worktrees" });
  expect(sidebar.textContent).not.toMatch(/Hook|Protocol|773\d/);
  expect(sidebar.innerHTML).not.toMatch(/773\d/);
});

const feature = "/code/grove-wt/feat-35";

// The button that selects a worktree. It holds the name, chip, branch and counts.
async function worktreeRow(name: string) {
  return await screen.findByRole("button", { name });
}

test("a worktree's row shows its most urgent session state", async () => {
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
  expect(row.querySelector(".ui-agent-state")?.textContent).toBe("Approval");
  // A screen reader reads the state after the name.
  expect(screen.getByRole("button", { name: "feat-35-tauri-sidebar", description: /^Approval/ })).toBe(row);
});

test("a worktree with one session has no nested row, and clicking the worktree shows that session", async () => {
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

  expect(screen.queryByRole("button", { name: /Claude Code/ })).toBeNull();
  expect(shown).toEqual([only]);
});

test("a worktree with two sessions keeps a nested row for each", async () => {
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
        { session: "2", worktree: feature, agent: "Claude", panel: "2" },
      ]}
    />,
  );
  await worktreeRow("feat-35-tauri-sidebar");

  expect(screen.getAllByRole("button", { name: /^Claude Code/ }).map((row) => row.textContent)).toEqual([
    "Claude Code ",
    "Claude Code 2 ",
  ]);
});

// A changed file in a fake diff, with `added` and `removed` lines.
function change(path: string, added: number, removed: number) {
  return {
    file: { status: "Modified" as const, oldPath: path, newPath: path, binary: false, added, removed },
    versions: { old: "", new: "", binary: false },
  };
}

// The added and removed counts on a worktree's branch line, or null when it shows none.
function counts(row: HTMLElement) {
  const added = row.querySelector(".add")?.textContent;
  const removed = row.querySelector(".del")?.textContent;
  return added === undefined && removed === undefined ? null : [added, removed];
}

test("a worktree's counts are its whole-branch diff, and change when the diff does", async () => {
  const diffs = {
    [feature]: {
      Head: [change("src/a.ts", 1, 1)],
      MergeBase: [change("src/a.ts", 10, 2), change("src/b.ts", 5, 1)],
    },
  };
  const bridge = fakeBridge({ "/code/grove": groveRepo }, diffs);
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  const row = await worktreeRow("feat-35-tauri-sidebar");

  await expect.poll(() => counts(row)).toEqual(["+15", "−3"]);

  diffs[feature].MergeBase = [change("src/a.ts", 20, 4)];
  act(() => bridge.diffChanged(feature));

  await expect.poll(() => counts(row)).toEqual(["+20", "−4"]);
});

test("a worktree with no base branch counts its Head diff, and one with no changes shows no counts", async () => {
  const repo = [
    { path: "/code/grove", branch: "main", isMain: true },
    { path: feature, branch: "feat-35-tauri-sidebar", isMain: false, baseBranch: null },
  ];
  const diffs = {
    "/code/grove": { Head: [], MergeBase: [] },
    [feature]: { Head: [change("src/a.ts", 7, 0)] },
  };
  const bridge = fakeBridge({ "/code/grove": repo }, diffs);
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);

  await expect.poll(async () => counts(await worktreeRow("feat-35-tauri-sidebar"))).toEqual(["+7", "−0"]);
  expect(counts(await worktreeRow("main (main worktree)"))).toBeNull();
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

  expect(row.querySelector(".ui-agent-state")?.textContent).toBe("Setup running");
  expect(screen.getAllByText("Setup running")).toHaveLength(1);
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

test("a long worktree name is cut with an ellipsis, and its chip is never cut", async () => {
  const style = document.createElement("style");
  style.textContent = sidebarStyles + agentStateStyles;
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
  const chip = getComputedStyle(row.querySelector(".wt-state")!);
  const word = getComputedStyle(row.querySelector(".ui-agent-state")!);

  expect([title.overflow, title.textOverflow, title.whiteSpace, title.minWidth]).toEqual([
    "hidden",
    "ellipsis",
    "nowrap",
    "0px",
  ]);
  expect([chip.flexShrink, word.whiteSpace]).toEqual(["0", "nowrap"]);
});

test("Tab moves from a worktree's row to its menu, its session rows, then Start Claude Code", async () => {
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
    screen.getByRole("button", { name: "Actions for feat-35-tauri-sidebar" }),
    screen.getByRole("button", { name: "Claude Code" }),
    screen.getByRole("button", { name: "Codex" }),
    screen.getByRole("button", { name: "Start Claude Code" }),
  ]);
  expect(item.querySelector("[tabindex]")).toBeNull();
});

test("a worktree's counts are asked again when its base branch changes to another branch", async () => {
  const diffs = { [feature]: { MergeBase: [change("src/a.ts", 10, 2)] } };
  const bridge = fakeBridge({ "/code/grove": groveRepo }, diffs);
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  const row = await worktreeRow("feat-35-tauri-sidebar");
  await expect.poll(() => counts(row)).toEqual(["+10", "−2"]);

  // The core sends no DiffChanged for a new base, so only the change of base can ask again.
  diffs[feature].MergeBase = [change("src/a.ts", 3, 1)];
  fireEvent.click(screen.getByRole("button", { name: "Actions for feat-35-tauri-sidebar" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Change base branch" }));
  const dialog = await screen.findByRole("dialog", { name: "Change base branch" });
  fireEvent.change(within(dialog).getByLabelText("Base branch"), { target: { value: "feat-35-tauri-sidebar" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));

  await expect.poll(() => counts(row)).toEqual(["+3", "−1"]);
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

  addRepo("/code/grove");
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

test("Settings in the footer opens the appearance menu, which changes palette and theme", () => {
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

  expect(appearances).toEqual([
    { theme: "system", palette: "modern" },
    { theme: "light", palette: "default" },
  ]);
});
