import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { createRef } from "react";
import { afterEach, expect, onTestFinished, test, vi } from "vitest";

import type { Worktree } from "./bridge";
import { fakeBridge, type FakeBridge } from "./fakeBridge";
import sidebarStyles from "./styles.css?raw";
import { keyHint, shortcut, type ShortcutId } from "./shortcuts";
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
  agentStates: {},
  shownAgents: [],
  onStartAgent() {},
  onStartAgentAfterSetup() {},
  onNewSession() {},
  onDeleting() {},
  onRemoved() {},
  onFocusAgent() {},
  onSessionResumed() {},
  onSessionsDeleted() {},
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

// Each session row's title, branch and status dot, and whether it has the worktree badge, in order.
function sessionRowsShown() {
  return sessionRows().map((row) => ({
    title: row.querySelector(".session-title")?.textContent,
    branch: row.querySelector(".session-branch")?.textContent,
    worktree: within(row).queryByRole("img", { name: "Worktree" }) !== null,
    dot: dot(row),
  }));
}

test("session rows come from the core's list under their project, with the state, title, branch and a worktree badge", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const local = await bridge.spawnAgent("/code/grove", "Claude", null);
  await bridge.sendMessage(local, "\n  fix the login bug \nthen test");
  const inWorktree = await bridge.spawnAgent(feature, "Codex", null);
  bridge.agentStateChanged({ session: inWorktree, state: "NeedsInput", kind: "Approval", source: "Hook" });

  // Neither worktree was opened, and the sidebar has no state for either session.
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);

  await vi.waitFor(() => expect(sessionRows()).toHaveLength(2));
  const repo = screen.getByRole("region", { name: "grove" });
  expect(sessionRows().every((row) => repo.contains(row))).toBe(true);
  expect(sessionRowsShown()).toEqual([
    { title: "fix the login bug", branch: "main", worktree: false, dot: null },
    { title: "New session", branch: "feat-35-tauri-sidebar", worktree: true, dot: "Approval" },
  ]);
  expect(repo.textContent).not.toMatch(/Hook/);
});

const feature = "/code/grove-wt/feat-35";

// The button that selects a worktree with no session. It holds the name.
async function worktreeRow(name: string) {
  return await screen.findByRole("button", { name });
}

// The agent session rows' buttons, in order.
function sessionRows() {
  return screen.queryAllByRole("button").filter((button) => button.classList.contains("session-row"));
}

// A session row's whole row: its button, its menu and its dot.
function sessionItem(row: HTMLElement) {
  return row.closest<HTMLElement>(".session-item")!;
}

// The state a row's status dot names, or null when it has none. The dot sits beside the row's
// button, which it describes.
function dot(row: HTMLElement) {
  const own = row.closest(".wt-row, .session-item")?.querySelector(".status-dot");
  return own?.getAttribute("aria-label") ?? null;
}

// A tracked grove repo with one Claude Code session in feat-35, which the sidebar lists.
async function oneSession() {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const session = await bridge.spawnAgent(feature, "Claude", null);
  return { bridge, session };
}

test("a session's dot shows its latest state, without its source, and never the session id", async () => {
  const { bridge, session } = await oneSession();
  bridge.agentStateChanged({ session, state: "NeedsInput", kind: "Approval", source: "Hook" });

  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agentStates={{ [session]: { session, state: "Working", source: "Protocol" } }}
    />,
  );

  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));
  // The state the window got last beats the one the list has.
  expect(dot(sessionRows()[0])).toBe("Working");
  // The dot's word is its tooltip, and a screen reader reads it after the row's name.
  expect(sessionItem(sessionRows()[0]).querySelector(".status-dot")!.getAttribute("title")).toBe("Working");
  expect(screen.getByRole("button", { description: "Working" })).toBe(sessionRows()[0]);
  const sidebar = screen.getByRole("complementary", { name: "Worktrees" });
  expect(sidebar.textContent).not.toMatch(/Hook|Protocol/);
  expect(sidebar.innerHTML).not.toMatch(new RegExp(`"${session}"`));
});

test.each([
  ["Idle", { state: "Idle" as const }, null],
  ["Exited", { state: "Exited" as const }, null],
  ["Done", { state: "Done" as const }, "Done"],
  ["Failed", { state: "Failed" as const }, "Failed"],
  ["Question", { state: "NeedsInput" as const, kind: "Question" as const }, "Question"],
])("a session that is %s shows the dot %s", async (_, state, shown) => {
  const { bridge, session } = await oneSession();

  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agentStates={{ [session]: { session, ...state, source: "Protocol" } }}
    />,
  );

  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));
  expect(dot(sessionRows()[0])).toBe(shown);
});

test("selecting a running session focuses it, and its row is marked while its panel shows", async () => {
  const { bridge, session } = await oneSession();
  const resumed = vi.spyOn(bridge, "resumeAgentSession");
  const focused: unknown[] = [];
  const sidebar = (shownAgents: string[]) => (
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      shownAgents={shownAgents}
      onFocusAgent={(agent) => focused.push(agent)}
    />
  );
  const { rerender } = render(sidebar([]));
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));

  await act(async () => fireEvent.click(sessionRows()[0]));
  rerender(sidebar([session]));

  expect(focused).toEqual([{ session, worktree: feature, agent: "Claude", panel: session }]);
  expect(resumed).not.toHaveBeenCalled();
  expect(sessionRows()[0].getAttribute("aria-current")).toBe("true");
  expect(sessionItem(sessionRows()[0]).classList.contains("selected")).toBe(true);
});

test("sessions sharing a worktree each get a row, in the order they started", async () => {
  const { bridge, session } = await oneSession();
  const second = await bridge.spawnAgent(feature, "Codex", null);
  await bridge.sendMessage(second, "review the diff");

  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);

  await vi.waitFor(() => expect(sessionRows()).toHaveLength(2));
  expect(sessionRowsShown().map(({ title }) => title)).toEqual(["New session", "review the diff"]);
  expect(session).not.toBe(second);
});

test("an ended session is dimmed, and selecting it resumes it before focusing it", async () => {
  const { bridge, session } = await oneSession();
  await bridge.exit(session);
  const steps: string[] = [];
  const resume = bridge.resumeAgentSession.bind(bridge);
  vi.spyOn(bridge, "resumeAgentSession").mockImplementation(async (id) => {
    steps.push(`resume ${id}`);
    return resume(id);
  });
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      onSessionResumed={(id) => steps.push(`resumed ${id}`)}
      onFocusAgent={({ session: id }) => steps.push(`focus ${id}`)}
    />,
  );
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));
  expect(sessionItem(sessionRows()[0]).classList.contains("dimmed")).toBe(true);

  await act(async () => fireEvent.click(sessionRows()[0]));

  // The panel learns of the new process before it shows.
  expect(steps).toEqual([`resume ${session}`, `resumed ${session}`, `focus ${session}`]);
  // The core says it runs again, and the row is no longer dimmed.
  await vi.waitFor(() => expect(sessionItem(sessionRows()[0]).classList.contains("dimmed")).toBe(false));
});

test("an ended session that cannot resume says why, and is not focused", async () => {
  const { bridge, session } = await oneSession();
  await bridge.exit(session);
  vi.spyOn(bridge, "resumeAgentSession").mockRejectedValueOnce("could not start the session's command");
  const focused: unknown[] = [];
  const resumed: unknown[] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      onSessionResumed={(id) => resumed.push(id)}
      onFocusAgent={(agent) => focused.push(agent)}
    />,
  );
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));

  await act(async () => fireEvent.click(sessionRows()[0]));

  expect(screen.getByRole("alert").textContent).toBe("could not start the session's command");
  expect(focused).toEqual([]);
  expect(resumed).toEqual([]);
});

test("a subagent session sits under the session that spawned it, in another worktree too", async () => {
  const { bridge, session } = await oneSession();
  const subagent = bridge.spawnAgentWithoutPanel("/code/grove-wt/spike", "Claude", session);
  const nested = bridge.spawnAgentWithoutPanel(feature, "Codex", subagent);
  await bridge.sendMessage(subagent, "write the tests");
  await bridge.sendMessage(nested, "check the tests");

  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);

  await vi.waitFor(() => expect(sessionRows()).toHaveLength(3));
  // Each row's title, and the title of the row it sits under. null at the top of its project.
  const under = (row: HTMLElement) =>
    row.closest("li")!.parentElement!.closest("li")?.querySelector(".session-title")?.textContent ?? null;
  expect(sessionRows().map((row) => [row.querySelector(".session-title")!.textContent, under(row)])).toEqual([
    ["New session", null],
    ["write the tests", "New session"],
    ["check the tests", "write the tests"],
  ]);
  expect(sessionRowsShown()[1].branch).toBe("spike");
});

// Opens the session row's menu and picks Delete session.
function pickDeleteSession(row: HTMLElement) {
  fireEvent.click(within(sessionItem(row)).getByRole("button", { name: /^Actions for / }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Delete session" }));
}

test("Delete session asks first, then deletes the session, and its row goes", async () => {
  const { bridge, session } = await oneSession();
  await bridge.sendMessage(session, "fix the login bug");
  const deleted: string[][] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      onSessionsDeleted={(sessions) => deleted.push(sessions)}
    />,
  );
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));

  pickDeleteSession(sessionRows()[0]);
  const dialog = screen.getByRole("dialog", { name: "Delete session" });
  expect(dialog.textContent).toContain("grove ends “fix the login bug” and deletes its conversation for good.");
  expect(dialog.textContent).not.toMatch(/subagent/);
  expect(await bridge.agentSessions()).toHaveLength(1);
  await act(async () => fireEvent.click(within(dialog).getByRole("button", { name: "Delete session" })));

  expect(screen.queryByRole("dialog")).toBeNull();
  expect(sessionRows()).toEqual([]);
  expect(await bridge.agentSessions()).toEqual([]);
  expect(deleted).toEqual([[session]]);
  // Its worktree stays, now with no session.
  expect(await worktreeRow("feat-35-tauri-sidebar")).toBeTruthy();
});

test("Delete session says its subagent sessions are deleted too, and deletes them", async () => {
  const { bridge, session } = await oneSession();
  const subagent = bridge.spawnAgentWithoutPanel(feature, "Claude", session);
  const nested = bridge.spawnAgentWithoutPanel(feature, "Claude", subagent);
  const deleted: string[][] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      onSessionsDeleted={(sessions) => deleted.push(sessions)}
    />,
  );
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(3));

  pickDeleteSession(sessionRows()[0]);
  const dialog = screen.getByRole("dialog", { name: "Delete session" });
  expect(dialog.textContent).toContain("Its 2 subagent sessions are deleted too.");
  await act(async () => fireEvent.click(within(dialog).getByRole("button", { name: "Delete session" })));

  expect(sessionRows()).toEqual([]);
  expect(deleted).toEqual([[session, subagent, nested]]);
  expect(bridge.killed).toEqual([session, subagent, nested]);
});

test("Cancel in Delete session deletes nothing, and a failed delete says why", async () => {
  const { bridge, session } = await oneSession();
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));

  pickDeleteSession(sessionRows()[0]);
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("dialog")).toBeNull();
  expect(sessionRows()).toHaveLength(1);

  const why = `sessions in ${feature} did not exit in time, so grove left it as it was`;
  vi.spyOn(bridge, "deleteAgentSession").mockRejectedValueOnce(why);
  pickDeleteSession(sessionRows()[0]);
  const dialog = screen.getByRole("dialog", { name: "Delete session" });
  await act(async () => fireEvent.click(within(dialog).getByRole("button", { name: "Delete session" })));

  expect(within(dialog).getByRole("alert").textContent).toBe(why);
  expect(sessionRows()).toHaveLength(1);
  expect(session).toBe("0");
});

test("a worktree with no session is dimmed at the bottom of its project, and selecting it selects the worktree", async () => {
  const { bridge } = await oneSession();
  const selected: (string | null)[] = [];
  render(<Sidebar bridge={bridge} selected={null} onSelect={(path) => selected.push(path)} {...noAgents} />);
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));
  const repo = screen.getByRole("region", { name: "grove" });

  // feat-35 has the session, so it has no worktree row.
  expect(listed(repo)).toEqual([
    ["main (main worktree)", "/code/grove"],
    ["spike", "/code/grove-wt/spike"],
  ]);
  const rows = [...repo.querySelectorAll(".session-item, .wt-row")];
  expect(rows.map((row) => row.classList.contains("session-item"))).toEqual([true, false, false]);
  expect(rows.slice(1).every((row) => row.classList.contains("dimmed"))).toBe(true);
  expect(menuItems("spike")).toEqual(["Start Claude Code", "Change base branch", "Forget", "Delete"]);
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });

  fireEvent.click(screen.getByRole("button", { name: "spike" }));
  expect(selected).toEqual(["/code/grove-wt/spike"]);
});

test("dimmed rows show in the muted color", () => {
  expect(sidebarStyles).toMatch(/\.dimmed \.session-title[^{]*\{[^}]*color: var\(--muted\)/);
  expect(sidebarStyles).toMatch(/\.dimmed \.wt-name[^{]*\{[^}]*color: var\(--muted\)/);
});

test("the core saying the sessions changed lists them again", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await screen.findByRole("region", { name: "grove" });
  expect(sessionRows()).toEqual([]);

  await act(async () => void (await bridge.spawnAgent(feature, "Claude", null)));

  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));
});

test("a worktree whose setup script runs shows Setup running on its row", async () => {
  const repo = [
    { path: "/code/grove", branch: "main", isMain: true },
    { path: feature, branch: "feat-35-tauri-sidebar", isMain: false, setup: { state: "Running" as const } },
  ];
  const bridge = fakeBridge({ "/code/grove": repo });
  await bridge.addRepo("/code/grove");

  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
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

test("a long worktree name or session title is cut and fades out, and the dot is never squeezed", async () => {
  const style = document.createElement("style");
  style.textContent = sidebarStyles;
  document.head.append(style);
  onTestFinished(() => style.remove());
  const repo = [
    { path: "/code/grove", branch: "main", isMain: true },
    { path: "/code/grove-wt/spike", branch: "spike", isMain: false, setup: { state: "Running" as const } },
  ];
  const bridge = fakeBridge({ "/code/grove": repo });
  await bridge.addRepo("/code/grove");
  const session = await bridge.spawnAgent("/code/grove", "Claude", null);

  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      agentStates={{ [session]: { session, state: "NeedsInput", kind: "Question", source: "Hook" } }}
    />,
  );
  const row = await worktreeRow("spike");
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));

  for (const title of [row.querySelector(".wt-title")!, sessionRows()[0].querySelector(".session-title")!]) {
    const box = getComputedStyle(title);
    expect([box.overflow, box.whiteSpace, box.minWidth]).toEqual(["hidden", "nowrap", "0px"]);
  }
  // The title gives way first. The branch keeps its width, up to a third of the row.
  const branch = getComputedStyle(sessionRows()[0].querySelector(".session-branch")!);
  expect([branch.flexShrink, branch.maxWidth]).toEqual(["0", "33%"]);
  for (const each of [row, sessionRows()[0]]) {
    expect(getComputedStyle(each.closest(".wt-row, .session-item")!.querySelector(".status-dot")!).flexShrink).toBe("0");
  }
});

test("Tab moves from a session's row to its menu, then to its subagent session's row", async () => {
  const { bridge, session } = await oneSession();
  bridge.spawnAgentWithoutPanel(feature, "Codex", session);
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(2));
  const item = sessionRows()[0].closest("li")!;

  // No element in the sidebar sets tabIndex, so the order of its buttons is the Tab order.
  expect(within(item).getAllByRole("button")).toEqual([
    sessionRows()[0],
    within(sessionItem(sessionRows()[0])).getByRole("button", { name: /^Actions for / }),
    sessionRows()[1],
    within(sessionItem(sessionRows()[1])).getByRole("button", { name: /^Actions for / }),
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

test("Search finds a session by its title or its worktree's branch", async () => {
  const bridge = await twoRepos();
  const login = await bridge.spawnAgent("/code/grove", "Claude", null);
  await bridge.sendMessage(login, "fix the login bug");
  await bridge.spawnAgent("/code/site-wt/fix-12", "Claude", null);
  render(<Sidebar bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(2));

  search("LOGIN");
  expect(sessionRowsShown().map(({ title }) => title)).toEqual(["fix the login bug"]);
  expect(rowNames()).toEqual([]);

  search("pty");
  expect(sessionRowsShown().map(({ branch }) => branch)).toEqual(["fix-12-pty"]);
  expect(screen.queryByRole("region", { name: "grove" })).toBeNull();
});

test("focusing Search puts focus in it, and Escape there clears it", async () => {
  const handle = createRef<SidebarHandle>();
  render(<Sidebar ref={handle} bridge={await twoRepos()} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("Terminal resize");
  const box = screen.getByRole("searchbox", { name: "Search" });

  act(() => handle.current!.focusSearch());
  expect(document.activeElement).toBe(box);

  search("resize");
  fireEvent.keyDown(box, { key: "Escape" });
  expect((box as HTMLInputElement).value).toBe("");
  expect(rowNames()).toHaveLength(5);
});

// The places in the open picker named `menu`, by name, in order.
function placesIn(menu: string) {
  return within(screen.getByRole("menu", { name: menu }))
    .getAllByRole("menuitem")
    .map((item) => item.getAttribute("aria-label"));
}

// The main New session button's places. Its picker must be open.
const places = () => placesIn("New session");

function pickPlace(name: string) {
  fireEvent.click(screen.getByRole("button", { name: "New session" }));
  fireEvent.click(screen.getByRole("menuitem", { name }));
}

test("New session, by click or by key, opens the place picker for the selected worktree's project", async () => {
  const handle = createRef<SidebarHandle>();
  render(<Sidebar ref={handle} bridge={await twoRepos()} selected="/code/site-wt/fix-12" onSelect={() => {}} {...noAgents} />);
  await worktreeRow("Terminal resize");
  // New session is the only way to make a worktree from the sidebar.
  expect(screen.queryByRole("button", { name: /New worktree/ })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "New session" }));
  expect(places()).toEqual(["Local checkout", "Terminal resize", "New worktree…"]);
  // The local checkout runs on its current branch, and a worktree names its branch.
  expect(within(screen.getByRole("menuitem", { name: "Local checkout" })).getByText("main")).toBeTruthy();
  expect(within(screen.getByRole("menuitem", { name: "Terminal resize" })).getByText("fix-12-pty")).toBeTruthy();
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();

  act(() => handle.current!.newSession());
  expect(places()).toEqual(["Local checkout", "Terminal resize", "New worktree…"]);
});

test("the sidebar's key hints come from the shortcut list", () => {
  render(<Sidebar bridge={fakeBridge({})} selected={null} onSelect={() => {}} {...noAgents} />);
  const rows: [ShortcutId, HTMLElement][] = [
    ["newSession", screen.getByRole("button", { name: "New session" })],
    ["search", screen.getByRole("searchbox", { name: "Search" })],
    ["jump", screen.getByRole("button", { name: /^Needs you/ })],
  ];
  for (const [id, control] of rows) {
    const { keys } = shortcut(id);
    expect(control.getAttribute("aria-keyshortcuts")).toBe(keys);
    expect(control.closest(".nav-row")!.querySelector("kbd")!.textContent).toBe(keyHint(keys));
  }
});

test("with nothing selected, New session uses the first project, and with no project it is off", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  const handle = createRef<SidebarHandle>();
  render(<Sidebar ref={handle} bridge={bridge} selected={null} onSelect={() => {}} {...noAgents} />);
  expect(screen.getByRole("button", { name: "New session" })).toHaveProperty("disabled", true);
  act(() => handle.current!.newSession());
  expect(screen.queryByRole("menu")).toBeNull();

  addRepo(bridge, "/code/grove");
  await worktreeRow("feat-35-tauri-sidebar");
  fireEvent.click(screen.getByRole("button", { name: "New session" }));

  expect(places()).toEqual(["Local checkout", "feat-35-tauri-sidebar", "spike", "New worktree…"]);
});

test("the picker works from the keyboard: opened by key it focuses the first place, arrows move, Escape closes it", async () => {
  const shown: string[] = [];
  const handle = createRef<SidebarHandle>();
  render(
    <Sidebar
      ref={handle}
      bridge={await twoRepos()}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      onShowSidebar={() => shown.push("shown")}
    />,
  );
  await worktreeRow("Terminal resize");

  act(() => handle.current!.newSession());
  // A collapsed sidebar shows first, since a hidden picker takes no focus.
  expect(shown).toEqual(["shown"]);
  expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Local checkout" }));
  fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
  expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "feat-35-tauri-sidebar" }));
  fireEvent.keyDown(document.activeElement!, { key: "ArrowUp" });
  fireEvent.keyDown(document.activeElement!, { key: "ArrowUp" });
  expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "New worktree…" }));

  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "New session" }));
});

test("picking the local checkout or a worktree selects it and starts a session there", async () => {
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
      onNewSession={(path) => started.push(path)}
    />,
  );
  await worktreeRow("feat-35-tauri-sidebar");

  pickPlace("Local checkout");
  pickPlace("feat-35-tauri-sidebar");

  expect(selected).toEqual(["/code/grove", feature]);
  expect(started).toEqual(["/code/grove", feature]);
  expect(screen.queryByRole("menu")).toBeNull();
});

test("a place that already has a session is offered, and picking it starts another session there", async () => {
  const { bridge } = await oneSession();
  const started: string[] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected={feature}
      onSelect={() => {}}
      {...noAgents}
      onNewSession={(path) => started.push(path)}
    />,
  );
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));

  pickPlace("feat-35-tauri-sidebar");

  expect(started).toEqual([feature]);
});

test("a worktree whose setup script runs gets its session once the setup ends", async () => {
  const repo = [
    { path: "/code/grove", branch: "main", isMain: true },
    { path: feature, branch: "feat-35-tauri-sidebar", isMain: false, setup: { state: "Running" as const } },
  ];
  const bridge = fakeBridge({ "/code/grove": repo });
  await bridge.addRepo("/code/grove");
  const now: string[] = [];
  const afterSetup: string[] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={() => {}}
      {...noAgents}
      onNewSession={(path) => now.push(path)}
      onStartAgentAfterSetup={(path) => afterSetup.push(path)}
    />,
  );
  await worktreeRow("feat-35-tauri-sidebar");

  pickPlace("feat-35-tauri-sidebar");

  expect(now).toEqual([]);
  expect(afterSetup).toEqual([feature]);
});

test("a project's + opens New session for that project, though a row of another project is selected", async () => {
  const bridge = await twoRepos();
  const selected: (string | null)[] = [];
  const started: string[] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected={feature}
      onSelect={(path) => selected.push(path)}
      {...noAgents}
      onNewSession={(path) => started.push(path)}
    />,
  );
  await worktreeRow("Terminal resize");
  const plus = screen.getByRole("button", { name: "New session in site" });
  expect(plus.title).toBe("New session in site");

  fireEvent.click(plus);
  expect(placesIn("New session in site")).toEqual(["Local checkout", "Terminal resize", "New worktree…"]);
  fireEvent.click(screen.getByRole("menuitem", { name: "Terminal resize" }));

  expect(selected).toEqual(["/code/site-wt/fix-12"]);
  expect(started).toEqual(["/code/site-wt/fix-12"]);
  // The main button still uses the selected worktree's project.
  fireEvent.click(screen.getByRole("button", { name: "New session" }));
  expect(places()).toEqual(["Local checkout", "feat-35-tauri-sidebar", "spike", "New worktree…"]);
});

test("a folded project's + opens New session for it, and Escape gives focus back to the +", async () => {
  render(<Sidebar bridge={await twoRepos()} selected={null} onSelect={() => {}} {...noAgents} />);
  await worktreeRow("Terminal resize");
  fireEvent.click(folder("site"));
  expect(screen.queryByRole("button", { name: "Terminal resize" })).toBeNull();
  const plus = screen.getByRole("button", { name: "New session in site" });
  // A button, so Tab reaches it and Enter or Space opens it.
  expect(plus.tagName).toBe("BUTTON");
  expect(plus.tabIndex).toBe(0);

  plus.focus();
  fireEvent.click(plus);
  expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Local checkout" }));
  expect(placesIn("New session in site")).toEqual(["Local checkout", "Terminal resize", "New worktree…"]);

  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();
  expect(document.activeElement).toBe(plus);
});

test("a project's + shows on hover or focus, and stays while its picker is open", () => {
  expect(sidebarStyles).toMatch(/\.repo-folder \.ui-menu \{\s*opacity: 0;/);
  expect(sidebarStyles).toMatch(/\.repo-folder:hover \.ui-menu,\s*\.repo-folder \.ui-menu:focus-within \{\s*opacity: 1;/);
});

test("New worktree… opens the new worktree dialog for the project, which makes the worktree", async () => {
  const bridge = await twoRepos();
  const asked = vi.spyOn(bridge, "suggestProfile");
  const selected: (string | null)[] = [];
  render(
    <Sidebar bridge={bridge} selected="/code/site-wt/fix-12" onSelect={(path) => selected.push(path)} {...noAgents} />,
  );
  await worktreeRow("Terminal resize");

  pickPlace("New worktree…");
  const dialog = await screen.findByRole("dialog", { name: "New worktree" });
  expect(asked).toHaveBeenLastCalledWith("/code/site");
  fireEvent.change(within(dialog).getByLabelText("Branch name"), { target: { value: "fix-13" } });
  fireEvent.change(within(dialog).getByLabelText("Profile"), { target: { value: "none" } });
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  expect(await worktreeRow("fix-13")).toBeTruthy();
  expect(selected).toHaveLength(1);
  expect(selected[0]).toMatch(/fix-13$/);
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

test("the selected worktree's row is marked, and a session's Working dot pulses", async () => {
  const { bridge, session } = await oneSession();
  render(
    <Sidebar
      bridge={bridge}
      selected="/code/grove-wt/spike"
      onSelect={() => {}}
      {...noAgents}
      agentStates={{ [session]: { session, state: "Working", source: "Protocol" } }}
    />,
  );
  const row = await worktreeRow("spike");
  await vi.waitFor(() => expect(sessionRows()).toHaveLength(1));

  expect(row.closest(".wt-row")!.classList.contains("selected")).toBe(true);
  expect(row.getAttribute("aria-current")).toBe("true");
  expect(sessionItem(sessionRows()[0]).querySelector(".status-dot")!.classList.contains("status-working")).toBe(true);
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
