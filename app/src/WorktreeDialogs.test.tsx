import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { fakeBridge, type FakeBridge, type FakeFiles } from "./fakeBridge";
import { Sidebar } from "./Sidebar";
import { menuItems } from "./testing";

afterEach(cleanup);

const groveRepo = [
  { path: "/code/grove", branch: "main", isMain: true },
  { path: "/code/grove-wt/feat-35", branch: "feat-35-tauri-sidebar", isMain: false },
];

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
  onOpenProfiles() {},
  accounts: {},
  appearance: { theme: "system" as const, palette: "default" as const },
  onAppearance() {},
};

// Renders the sidebar with the grove repo added. Returns what it selected, and the worktrees it
// started an agent session in once setup is done, in order.
async function sidebarWith(
  files: FakeFiles = {},
): Promise<{ bridge: FakeBridge; selected: (string | null)[]; started: string[] }> {
  const bridge = fakeBridge({ "/code/grove": groveRepo }, {}, files);
  await bridge.addRepo("/code/grove");
  const selected: (string | null)[] = [];
  const started: string[] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected={null}
      onSelect={(path) => selected.push(path)}
      {...noAgents}
      onStartAgentAfterSetup={(path) => started.push(path)}
    />,
  );
  await screen.findByRole("region", { name: "grove" });
  return { bridge, selected, started };
}

async function openNewWorktree() {
  fireEvent.click(screen.getByRole("button", { name: "New worktree in grove" }));
  return screen.findByRole("dialog", { name: "New worktree" });
}

function type(dialog: HTMLElement, label: string, value: string) {
  fireEvent.change(within(dialog).getByLabelText(label), { target: { value } });
}

function picked(dialog: HTMLElement): string | undefined {
  const select = within(dialog).getByLabelText<HTMLSelectElement>("Profile");
  return select.selectedOptions[0]?.textContent ?? undefined;
}

function pick(dialog: HTMLElement, profile: string) {
  const select = within(dialog).getByLabelText<HTMLSelectElement>("Profile");
  const option = [...select.options].find((o) => o.textContent === profile)!;
  fireEvent.change(select, { target: { value: option.value } });
}

// The text of each worktree's row, in order.
// Each worktree row's name, which its title gives.
function worktreeNames() {
  return within(screen.getByRole("region", { name: "grove" }))
    .getAllByRole("listitem")
    .map((row) => row.querySelector(".wt-title")!.textContent);
}

async function openMenu(worktree: string, item: string) {
  fireEvent.click(screen.getByRole("button", { name: `Actions for ${worktree}` }));
  fireEvent.click(within(screen.getByRole("menu")).getByRole("menuitem", { name: item }));
}

test("the dialog picks the suggested profile and says which marker file it came from", async () => {
  await sidebarWith({ "/code/grove": { "Cargo.toml": "" } });

  const dialog = await openNewWorktree();

  expect(await within(dialog).findByText("Suggested: Rust crate, because the repo has Cargo.toml.")).toBeTruthy();
  expect(picked(dialog)).toBe("Rust crate");
});

test("the suggestion names the marker the repo has when a profile lists several", async () => {
  await sidebarWith({ "/code/grove": { "next.config.mjs": "" } });

  const dialog = await openNewWorktree();

  expect(await within(dialog).findByText("Suggested: Next.js app, because the repo has next.config.mjs.")).toBeTruthy();
  expect(picked(dialog)).toBe("Next.js app");
});

test("with no suggestion the dialog asks the user to pick, and creates nothing until they do", async () => {
  const { bridge } = await sidebarWith();

  const dialog = await openNewWorktree();
  expect(await within(dialog).findByText("No profile matches this repo's files. Pick one, or None.")).toBeTruthy();
  expect(picked(dialog)).toBe("Pick a profile");

  type(dialog, "Branch name", "feat-52-ui");
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  expect(await within(dialog).findByText("Pick a profile, or None.")).toBeTruthy();
  expect(await bridge.listWorktrees()).toHaveLength(2);

  pick(dialog, "None");
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  await waitForNoDialog();
  expect(worktreeNames()).toEqual(["main (main worktree)", "feat-35-tauri-sidebar", "feat-52-ui"]);
  expect(await bridge.repoProfile("/code/grove")).toBeNull();
});

test("the repo's own profile is picked over the suggestion", async () => {
  const { bridge } = await sidebarWith({ "/code/grove": { "Cargo.toml": "" } });
  await bridge.setRepoProfile("/code/grove", "Next.js app");

  const dialog = await openNewWorktree();

  await within(dialog).findByText("Suggested: Rust crate, because the repo has Cargo.toml.");
  expect(picked(dialog)).toBe("Next.js app");
});

test("creating a worktree adds it to its repo, selects it, sets the repo's profile and starts Claude Code in it", async () => {
  const { bridge, selected, started } = await sidebarWith({ "/code/grove": { "Cargo.toml": "" } });

  const dialog = await openNewWorktree();
  await within(dialog).findByText(/^Suggested/);
  type(dialog, "Branch name", "feat/52-ui");
  type(dialog, "Base branch", "feat-35-tauri-sidebar");
  type(dialog, "Display name", "Worktree UI");
  type(dialog, "Issue link", "#52");
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  await waitForNoDialog();
  expect(worktreeNames()).toEqual(["main (main worktree)", "feat-35-tauri-sidebar", "Worktree UI"]);
  expect(selected).toEqual(["/grovecode/worktrees/grove/feat-52-ui"]);
  expect(started).toEqual(["/grovecode/worktrees/grove/feat-52-ui"]);
  expect((await bridge.listWorktrees())[2]).toEqual({
    path: "/grovecode/worktrees/grove/feat-52-ui",
    branch: "feat/52-ui",
    isMain: false,
    displayName: "Worktree UI",
    issueLink: "#52",
    baseBranch: "feat-35-tauri-sidebar",
    linkedFrom: null,
    setup: { state: "Running" },
  });
  expect(await bridge.repoProfile("/code/grove")).toBe("Rust crate");
});

test("with Start Claude Code unticked, the new worktree gets no agent session", async () => {
  const { selected, started } = await sidebarWith({ "/code/grove": { "Cargo.toml": "" } });

  const dialog = await openNewWorktree();
  await within(dialog).findByText(/^Suggested/);
  type(dialog, "Branch name", "feat-52");
  fireEvent.click(within(dialog).getByLabelText("Start Claude Code"));
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  await waitForNoDialog();
  expect(selected).toEqual(["/grovecode/worktrees/grove/feat-52"]);
  expect(started).toEqual([]);
});

test("empty fields take the core's defaults", async () => {
  const { bridge } = await sidebarWith({ "/code/grove": { "Cargo.toml": "" } });

  const dialog = await openNewWorktree();
  await within(dialog).findByText(/^Suggested/);
  type(dialog, "Branch name", "  feat-52  ");
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  await waitForNoDialog();
  expect((await bridge.listWorktrees())[2]).toMatchObject({
    branch: "feat-52",
    displayName: "feat-52",
    issueLink: null,
    baseBranch: "main",
  });
});

test("a branch the core refuses shows its error and keeps the dialog open", async () => {
  await sidebarWith({ "/code/grove": { "Cargo.toml": "" } });

  const dialog = await openNewWorktree();
  await within(dialog).findByText(/^Suggested/);
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));
  expect(await within(dialog).findByText("Type a branch name.")).toBeTruthy();

  type(dialog, "Branch name", "bad name");
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  expect((await within(dialog).findByRole("alert")).textContent).toBe(
    `"bad name" is not a valid branch name: fatal: 'bad name' is not a valid branch name`,
  );
  expect(worktreeNames()).toEqual(["main (main worktree)", "feat-35-tauri-sidebar"]);
});

test("a create the core refuses leaves the repo's profile as it was", async () => {
  const { bridge } = await sidebarWith({ "/code/grove": { "Cargo.toml": "" } });

  const dialog = await openNewWorktree();
  await within(dialog).findByText(/^Suggested/);
  type(dialog, "Branch name", "bad name");
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  await within(dialog).findByRole("alert");
  expect(await bridge.repoProfile("/code/grove")).toBeNull();

  type(dialog, "Branch name", "feat-52");
  fireEvent.click(within(dialog).getByRole("button", { name: "Create worktree" }));

  await waitForNoDialog();
  expect(await bridge.repoProfile("/code/grove")).toBe("Rust crate");
});

test("a worktree whose setup script runs shows it after a reload, with no Start Claude Code", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  await bridge.setRepoProfile("/code/grove", "Rust crate");
  const added = await bridge.createWorktree("/code/grove", {
    branch: "feat-52",
    displayName: null,
    issueLink: null,
    baseBranch: null,
  });

  render(<Sidebar bridge={bridge} selected={added.path} onSelect={() => {}} {...noAgents} />);

  expect(await screen.findByRole("img", { name: "Setup running" })).toBeTruthy();
  expect(menuItems("feat-52")).not.toContain("Start Claude Code");
  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  act(() => bridge.finishSetup(added.path, null));
  expect(await screen.findByText("Setup failed: was stopped by a signal")).toBeTruthy();
  expect(menuItems("feat-52")).toContain("Start Claude Code");
});

test("changing the base branch saves it, and an unknown branch shows the error", async () => {
  const { bridge } = await sidebarWith();

  await openMenu("feat-35-tauri-sidebar", "Change base branch");
  const dialog = await screen.findByRole("dialog", { name: "Change base branch" });
  expect(within(dialog).getByLabelText<HTMLInputElement>("Base branch").value).toBe("main");

  type(dialog, "Base branch", "develop");
  fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
  expect((await within(dialog).findByRole("alert")).textContent).toBe(
    `the repo at /code/grove has no branch "develop"`,
  );

  type(dialog, "Base branch", "feat-35-tauri-sidebar");
  fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));

  await waitForNoDialog();
  expect((await bridge.listWorktrees())[1].baseBranch).toBe("feat-35-tauri-sidebar");
});

test("the main worktree's menu only starts Claude Code and changes its base branch", async () => {
  await sidebarWith();

  fireEvent.click(screen.getByRole("button", { name: "Actions for main" }));

  const items = within(screen.getByRole("menu")).getAllByRole("menuitem");
  expect(items.map((item) => item.textContent)).toEqual(["Start Claude Code", "Change base branch"]);
});

test("closing a dialog puts focus back on the + that opened it", async () => {
  await sidebarWith();
  const plus = screen.getByRole("button", { name: "New worktree in grove" });
  plus.focus();
  fireEvent.click(plus);
  const dialog = await screen.findByRole("dialog", { name: "New worktree" });
  expect(dialog.contains(document.activeElement)).toBe(true);

  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

  await waitForNoDialog();
  expect(document.activeElement).toBe(plus);
});

test("closing a worktree menu, or the dialog it opened, puts focus back on the menu's button", async () => {
  await sidebarWith();
  const toggle = screen.getByRole("button", { name: "Actions for feat-35-tauri-sidebar" });

  fireEvent.click(toggle);
  fireEvent.keyDown(within(screen.getByRole("menu")).getAllByRole("menuitem")[0], { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();
  expect(document.activeElement).toBe(toggle);

  await openMenu("feat-35-tauri-sidebar", "Forget");
  const dialog = await screen.findByRole("dialog", { name: "Forget worktree" });
  fireEvent.keyDown(dialog, { key: "Escape" });

  await waitForNoDialog();
  expect(document.activeElement).toBe(toggle);
});

test("Tab and Shift+Tab stay inside a dialog, skipping a disabled button", async () => {
  const { bridge } = await sidebarWith();
  bridge.setStatus("/code/grove-wt/feat-35", { uncommitted: true });
  await openMenu("feat-35-tauri-sidebar", "Delete");
  const dialog = await screen.findByRole("dialog", { name: "Delete worktree" });
  const typed = await within(dialog).findByLabelText("Type feat-35-tauri-sidebar to confirm");
  const cancel = within(dialog).getByRole("button", { name: "Cancel" });
  expect(within(dialog).getByRole<HTMLButtonElement>("button", { name: "Delete" }).disabled).toBe(true);

  cancel.focus();
  fireEvent.keyDown(cancel, { key: "Tab" });
  expect(document.activeElement).toBe(typed);

  fireEvent.keyDown(typed, { key: "Tab", shiftKey: true });
  expect(document.activeElement).toBe(cancel);
});

test("forgetting a worktree stops listing it once the user confirms", async () => {
  const { bridge } = await sidebarWith();

  await openMenu("feat-35-tauri-sidebar", "Forget");
  const dialog = await screen.findByRole("dialog", { name: "Forget worktree" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Forget" }));

  await waitForNoDialog();
  expect(worktreeNames()).toEqual(["main (main worktree)"]);
  expect(bridge.deleted).toEqual([]);
});

test("forgetting the selected worktree clears the selection", async () => {
  const bridge = fakeBridge({ "/code/grove": groveRepo });
  await bridge.addRepo("/code/grove");
  const selected: (string | null)[] = [];
  render(
    <Sidebar
      bridge={bridge}
      selected="/code/grove-wt/feat-35"
      onSelect={(path) => selected.push(path)}
      {...noAgents}
    />,
  );
  await screen.findByRole("region", { name: "grove" });

  await openMenu("feat-35-tauri-sidebar", "Forget");
  fireEvent.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Forget" }));

  await waitForNoDialog();
  expect(selected).toEqual([null]);
});

test("deleting a clean worktree asks for no typed name", async () => {
  const { bridge } = await sidebarWith();

  await openMenu("feat-35-tauri-sidebar", "Delete");
  const dialog = await screen.findByRole("dialog", { name: "Delete worktree" });
  const button = await within(dialog).findByRole("button", { name: "Delete" });

  expect(within(dialog).queryByLabelText(/^Type/)).toBeNull();
  fireEvent.click(button);

  await waitForNoDialog();
  expect(worktreeNames()).toEqual(["main (main worktree)"]);
  expect(bridge.deleted).toEqual([{ worktree: "/code/grove-wt/feat-35", confirmed: false, deleteBranch: false }]);
});

test("deleting a worktree with unsaved work needs its name typed", async () => {
  const { bridge } = await sidebarWith();
  bridge.setStatus("/code/grove-wt/feat-35", { uncommitted: true, unpushed: 2 });

  await openMenu("feat-35-tauri-sidebar", "Delete");
  const dialog = await screen.findByRole("dialog", { name: "Delete worktree" });

  expect(
    await within(dialog).findByText("It has uncommitted changes and 2 commits on no remote. Deleting loses them."),
  ).toBeTruthy();
  const button = within(dialog).getByRole<HTMLButtonElement>("button", { name: "Delete" });
  expect(button.disabled).toBe(true);
  type(dialog, "Type feat-35-tauri-sidebar to confirm", "feat-35");
  expect(button.disabled).toBe(true);
  type(dialog, "Type feat-35-tauri-sidebar to confirm", "feat-35-tauri-sidebar");
  fireEvent.click(button);

  await waitForNoDialog();
  expect(bridge.deleted).toEqual([{ worktree: "/code/grove-wt/feat-35", confirmed: true, deleteBranch: false }]);
});

test("deleting the branch is offered only when it is merged", async () => {
  const { bridge } = await sidebarWith();

  await openMenu("feat-35-tauri-sidebar", "Delete");
  let dialog = await screen.findByRole("dialog", { name: "Delete worktree" });
  await within(dialog).findByText("Branch feat-35-tauri-sidebar stays. It is not merged into main.");
  expect(within(dialog).queryByRole("checkbox")).toBeNull();
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));

  bridge.setStatus("/code/grove-wt/feat-35", { merged: true });
  await openMenu("feat-35-tauri-sidebar", "Delete");
  dialog = await screen.findByRole("dialog", { name: "Delete worktree" });
  fireEvent.click(
    await within(dialog).findByRole("checkbox", { name: "Also delete branch feat-35-tauri-sidebar, merged into main" }),
  );
  fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

  await waitForNoDialog();
  expect(bridge.deleted).toEqual([{ worktree: "/code/grove-wt/feat-35", confirmed: false, deleteBranch: true }]);
});

test("a branch git cannot delete is reported after the worktree is gone", async () => {
  const { bridge } = await sidebarWith();
  bridge.setStatus("/code/grove-wt/feat-35", { merged: true, branchNotDeleted: "error: branch is locked" });

  await openMenu("feat-35-tauri-sidebar", "Delete");
  const dialog = await screen.findByRole("dialog", { name: "Delete worktree" });
  fireEvent.click(await within(dialog).findByRole("checkbox"));
  fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

  await waitForNoDialog();
  expect(worktreeNames()).toEqual(["main (main worktree)"]);
  expect(screen.getByRole("alert").textContent).toBe(
    "Deleted feat-35-tauri-sidebar, but branch feat-35-tauri-sidebar is still there: error: branch is locked",
  );
});

test("a delete the core refuses shows why and checks the worktree again", async () => {
  const { bridge } = await sidebarWith();

  await openMenu("feat-35-tauri-sidebar", "Delete");
  const dialog = await screen.findByRole("dialog", { name: "Delete worktree" });
  const button = await within(dialog).findByRole("button", { name: "Delete" });
  // An agent saved a file after the check.
  bridge.setStatus("/code/grove-wt/feat-35", { uncommitted: true });
  fireEvent.click(button);

  expect((await within(dialog).findByRole("alert")).textContent).toBe(
    "/code/grove-wt/feat-35 has uncommitted changes; confirm to delete it",
  );
  expect(await within(dialog).findByLabelText("Type feat-35-tauri-sidebar to confirm")).toBeTruthy();
  expect(worktreeNames()).toEqual(["main (main worktree)", "feat-35-tauri-sidebar"]);
});

async function waitForNoDialog() {
  await expect.poll(() => screen.queryByRole("dialog")).toBeNull();
}

test("a delete that ends after its dialog was cancelled leaves a dialog opened since then open", async () => {
  const { bridge } = await sidebarWith();
  const deleteWorktree = bridge.deleteWorktree.bind(bridge);
  let finishDelete = async () => {};
  vi.spyOn(bridge, "deleteWorktree").mockImplementation(
    (...args) => new Promise((resolve) => (finishDelete = async () => resolve(await deleteWorktree(...args)))),
  );
  await openMenu("feat-35-tauri-sidebar", "Delete");
  const deleting = await screen.findByRole("dialog", { name: "Delete worktree" });
  fireEvent.click(await within(deleting).findByRole("button", { name: "Delete" }));
  fireEvent.click(within(deleting).getByRole("button", { name: "Cancel" }));
  const dialog = await openNewWorktree();
  type(dialog, "Branch name", "feat-52");

  await act(async () => finishDelete());

  await expect.poll(worktreeNames).toEqual(["main (main worktree)"]);
  expect(screen.getByRole("dialog", { name: "New worktree" })).toBe(dialog);
  expect(within(dialog).getByLabelText<HTMLInputElement>("Branch name").value).toBe("feat-52");
});
