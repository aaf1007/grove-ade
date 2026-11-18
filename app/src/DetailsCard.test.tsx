import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { SlidersHorizontal } from "lucide-react";
import { afterEach, expect, test, vi } from "vitest";

import { DetailsCard, type FocusedSession } from "./DetailsCard";
import { FAKE_MODELS, type FakeChange, fakeBridge } from "./fakeBridge";
import { Menu } from "./ui/Menu";

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.restoreAllMocks();
});

const main = "/code/grove";
const feature = "/code/grove-wt/feat-36";

const RUN_SESSIONS = `[[run_session]]\nname = "dev"\ncommand = "pnpm dev"\nport = 5173\n\n[[run_session]]\nname = "tests"\ncommand = "cargo test"\n`;

function change(path: string, added: number, removed: number): FakeChange {
  return {
    file: { status: "Modified", oldPath: path, newPath: path, binary: false, added, removed },
    versions: { old: "", new: "", binary: false },
  };
}

async function setup() {
  const bridge = fakeBridge(
    {
      [main]: [
        { path: main, branch: "main", isMain: true },
        { path: feature, branch: "feat-36", isMain: false, baseBranch: "develop" },
      ],
    },
    { [feature]: { Head: [change("src/a.rs", 3, 1), change("src/b.rs", 4, 0)], MergeBase: [] } },
  );
  await bridge.addRepo(main);
  await bridge.setRepoOverridesText(main, RUN_SESSIONS);
  return bridge;
}

const claude: FocusedSession = {
  agent: "Claude",
  model: { model: FAKE_MODELS.Claude[1], effort: null },
  mode: "Chat",
  source: "Protocol",
};

async function renderCard(
  bridge: Awaited<ReturnType<typeof setup>>,
  props: Partial<Parameters<typeof DetailsCard>[0]> = {},
) {
  const handlers = { onStartRun: vi.fn(), onOpenDiff: vi.fn(), onClose: vi.fn() };
  const card = (more: typeof props) => (
    <DetailsCard bridge={bridge} worktree={feature} runs={[]} session={claude} {...handlers} {...props} {...more} />
  );
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(card({}));
  });
  return { ...handlers, rerender: (more: typeof props) => view.rerender(card(more)) };
}

const row = (name: string) => screen.getByRole("group", { name });

test("the worktree row shows its folder, copies its path and reveals it in Finder", async () => {
  const bridge = await setup();
  const writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
  await renderCard(bridge);

  expect(within(row("Worktree")).getByText(feature)).toBeTruthy();

  await act(async () => fireEvent.click(within(row("Worktree")).getByRole("button", { name: "Copy path" })));
  expect(writeText).toHaveBeenCalledWith(feature);

  await act(async () => fireEvent.click(within(row("Worktree")).getByRole("button", { name: "Reveal in Finder" })));
  expect(bridge.revealed).toEqual([feature]);
});

test("Open in opens the worktree in the chosen editor", async () => {
  const bridge = await setup();
  await renderCard(bridge);

  await act(async () => fireEvent.click(within(row("Open in")).getByRole("button", { name: "Zed" })));
  await act(async () => fireEvent.click(within(row("Open in")).getByRole("button", { name: "VS Code" })));

  expect(bridge.opened).toEqual([
    { editor: "Zed", worktree: feature, file: "", line: 1 },
    { editor: "VsCode", worktree: feature, file: "", line: 1 },
  ]);
});

test("the scripts row lists the repo's run sessions with their state, Start and Stop", async () => {
  const bridge = await setup();
  const dev = await bridge.spawnRun(feature, "dev", 80, 24);
  const { onStartRun } = await renderCard(bridge, { runs: [{ name: "dev", session: dev, running: true, port: null }] });

  const devItem = within(row("Scripts")).getByRole("listitem", { name: "dev" });
  const testsItem = within(row("Scripts")).getByRole("listitem", { name: "tests" });
  expect(within(devItem).getByText("Running")).toBeTruthy();
  expect(within(testsItem).getByText("Not started")).toBeTruthy();

  await act(async () => fireEvent.click(within(testsItem).getByRole("button", { name: "Start tests" })));
  expect(onStartRun).toHaveBeenCalledWith("tests");

  await act(async () => fireEvent.click(within(devItem).getByRole("button", { name: "Stop dev" })));
  expect(bridge.killed).toEqual([dev]);
});

test("a stopped run session says so and can start again", async () => {
  const bridge = await setup();
  const { onStartRun } = await renderCard(bridge, { runs: [{ name: "dev", session: "9", running: false, port: null }] });

  const devItem = within(row("Scripts")).getByRole("listitem", { name: "dev" });
  expect(within(devItem).getByText("Stopped")).toBeTruthy();
  await act(async () => fireEvent.click(within(devItem).getByRole("button", { name: "Start dev" })));
  expect(onStartRun).toHaveBeenCalledWith("dev");
});

test("the servers row shows the port of each running run session that serves one", async () => {
  const bridge = await setup();
  await renderCard(bridge, {
    runs: [
      { name: "dev", session: "1", running: true, port: null },
      { name: "tests", session: "2", running: true, port: null },
    ],
  });

  const servers = within(row("Servers")).getAllByRole("listitem");
  expect(servers.map((item) => item.textContent)).toEqual(["devlocalhost:5173"]);
});

test("the servers row says when none runs", async () => {
  const bridge = await setup();
  await renderCard(bridge, { runs: [{ name: "dev", session: "1", running: false, port: null }] });

  expect(within(row("Servers")).getByText("No server running")).toBeTruthy();
});

test("the branch row shows the branch and its base, and Change base changes the base", async () => {
  const bridge = await setup();
  await renderCard(bridge);

  expect(within(row("Branch")).getByText("feat-36")).toBeTruthy();
  expect(within(row("Branch")).getByText("develop")).toBeTruthy();

  await act(async () => fireEvent.click(within(row("Branch")).getByRole("button", { name: "Change base" })));
  const dialog = screen.getByRole("dialog", { name: "Change base branch" });
  fireEvent.change(within(dialog).getByLabelText("Base branch"), { target: { value: "main" } });
  await act(async () => fireEvent.click(within(dialog).getByRole("button", { name: "Save" })));

  expect(screen.queryByRole("dialog")).toBeNull();
  expect(within(row("Branch")).getByText("main")).toBeTruthy();
});

test("the changes row counts files and lines, and opens the Diff panel", async () => {
  const bridge = await setup();
  const { onOpenDiff } = await renderCard(bridge);

  const changes = within(row("Changes")).getByRole("button", { name: /2 files/ });
  expect(changes.textContent).toBe("2 files+7-1");

  fireEvent.click(changes);
  expect(onOpenDiff).toHaveBeenCalled();
});

test("the changes row follows the diff as it changes", async () => {
  const diffs = { Head: [change("src/a.rs", 1, 0)] };
  const bridge = fakeBridge({ [main]: [{ path: main, branch: "main", isMain: true }] }, { [main]: diffs });
  await bridge.addRepo(main);
  await renderCard(bridge as Awaited<ReturnType<typeof setup>>, { worktree: main });
  expect(within(row("Changes")).getByRole("button").textContent).toBe("1 file+1-0");

  diffs.Head = [];
  await act(async () => bridge.diffChanged(main));

  expect(within(row("Changes")).getByText("No changes")).toBeTruthy();
});

test("the session row shows the focused session's agent, model, effort, mode and state source", async () => {
  const bridge = await setup();
  await renderCard(bridge, {
    session: { ...claude, model: { model: FAKE_MODELS.Claude[0], effort: "max" } },
  });

  expect(within(row("Session")).getByText("Claude Code")).toBeTruthy();
  expect(within(row("Session")).getByText("Default (recommended)")).toBeTruthy();
  expect(within(row("Session")).getByText("max effort")).toBeTruthy();
  expect(within(row("Session")).getByText("Chat mode")).toBeTruthy();
  expect(within(row("Session")).getByText("State from Protocol")).toBeTruthy();
});

test("the session row says when the worktree has no agent session", async () => {
  const bridge = await setup();
  await renderCard(bridge, { session: null });

  expect(within(row("Session")).getByText("No agent session")).toBeTruthy();
});

test("a hidden row stays hidden after the card opens again", async () => {
  const bridge = await setup();
  await renderCard(bridge);

  fireEvent.click(screen.getByRole("button", { name: "Rows" }));
  fireEvent.click(screen.getByRole("menuitemcheckbox", { name: "Servers" }));
  expect(screen.queryByRole("group", { name: "Servers" })).toBeNull();

  cleanup();
  await renderCard(bridge);

  expect(screen.queryByRole("group", { name: "Servers" })).toBeNull();
  expect(row("Scripts")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Rows" }));
  expect(screen.getByRole("menuitemcheckbox", { name: "Servers" }).getAttribute("aria-checked")).toBe("false");
  fireEvent.click(screen.getByRole("menuitemcheckbox", { name: "Servers" }));
  expect(row("Servers")).toBeTruthy();
});

test("Escape and a click outside close the card, a click inside does not", async () => {
  const bridge = await setup();
  const { onClose } = await renderCard(bridge);

  fireEvent.pointerDown(row("Branch"));
  expect(onClose).not.toHaveBeenCalled();

  fireEvent.pointerDown(document.body);
  expect(onClose).toHaveBeenCalledTimes(1);

  fireEvent.keyDown(document.body, { key: "Escape" });
  expect(onClose).toHaveBeenCalledTimes(2);
});

test("Escape in the rows menu or in the Change base dialog leaves the card open", async () => {
  const bridge = await setup();
  const { onClose } = await renderCard(bridge);

  fireEvent.click(screen.getByRole("button", { name: "Rows" }));
  fireEvent.keyDown(screen.getByRole("menuitemcheckbox", { name: "Servers" }), { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();

  fireEvent.click(within(row("Branch")).getByRole("button", { name: "Change base" }));
  const dialog = screen.getByRole("dialog", { name: "Change base branch" });
  fireEvent.pointerDown(within(dialog).getByLabelText("Base branch"));
  fireEvent.keyDown(within(dialog).getByLabelText("Base branch"), { key: "Escape" });

  expect(screen.queryByRole("dialog")).toBeNull();
  expect(onClose).not.toHaveBeenCalled();
});

test("Escape meant for another popup leaves the card open", async () => {
  const bridge = await setup();
  const { onClose } = await renderCard(bridge);
  render(
    <>
      <Menu label="Other" icon={SlidersHorizontal} items={[{ key: "a", label: "A", onSelect: () => {} }]} />
      <div role="dialog" aria-label="Send review batch" />
    </>,
  );

  fireEvent.click(screen.getByRole("button", { name: "Other" }));
  fireEvent.keyDown(screen.getByRole("menuitem", { name: "A" }), { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();

  fireEvent.keyDown(screen.getByRole("dialog", { name: "Send review batch" }), { key: "Escape" });

  expect(onClose).not.toHaveBeenCalled();
});

test("a failed action says why in the card", async () => {
  const bridge = await setup();
  vi.spyOn(bridge, "revealInFinder").mockRejectedValueOnce("Finder is not running");
  await renderCard(bridge);

  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Reveal in Finder" })));

  expect(screen.getByRole("alert").textContent).toBe("Finder is not running");
});

test("the scripts and servers rows follow a change to the repo's run sessions", async () => {
  const bridge = await setup();
  await renderCard(bridge);
  expect(within(row("Scripts")).getByRole("listitem", { name: "dev" })).toBeTruthy();

  await act(async () =>
    bridge.setRepoOverridesText(main, `[[run_session]]\nname = "web"\ncommand = "pnpm dev"\nport = 3000\n`),
  );

  expect(within(row("Scripts")).queryByRole("listitem", { name: "dev" })).toBeNull();
  expect(within(row("Scripts")).getByRole("listitem", { name: "web" })).toBeTruthy();
});

test("a running run session that leaves the settings keeps its row, Stop and port", async () => {
  const bridge = await setup();
  const dev = await bridge.spawnRun(feature, "dev", 80, 24);
  await renderCard(bridge, { runs: [{ name: "dev", session: dev, running: true, port: 5173 }] });

  await act(async () =>
    bridge.setRepoOverridesText(main, `[[run_session]]\nname = "web"\ncommand = "pnpm dev"\nport = 3000\n`),
  );

  const devItem = within(row("Scripts")).getByRole("listitem", { name: "dev" });
  expect(within(devItem).getByText("Running")).toBeTruthy();
  const servers = within(row("Servers")).getAllByRole("listitem");
  expect(servers.map((item) => item.textContent)).toEqual(["devlocalhost:5173"]);

  await act(async () => fireEvent.click(within(devItem).getByRole("button", { name: "Stop dev" })));
  expect(bridge.killed).toEqual([dev]);
});

test("a run session that left the settings loses its row once it exits", async () => {
  const bridge = await setup();
  const { rerender } = await renderCard(bridge, { runs: [{ name: "dev", session: "1", running: true, port: 5173 }] });
  await act(async () =>
    bridge.setRepoOverridesText(main, `[[run_session]]\nname = "web"\ncommand = "pnpm dev"\nport = 3000\n`),
  );
  expect(within(row("Scripts")).getByRole("listitem", { name: "dev" })).toBeTruthy();

  rerender({ runs: [{ name: "dev", session: "1", running: false, port: 5173 }] });

  expect(within(row("Scripts")).queryByRole("listitem", { name: "dev" })).toBeNull();
  expect(within(row("Scripts")).getByRole("listitem", { name: "web" })).toBeTruthy();
  expect(within(row("Servers")).getByText("No server running")).toBeTruthy();
});

test("a running run session keeps its row, Stop and port when the settings cannot be read", async () => {
  const bridge = await setup();
  vi.spyOn(bridge, "runSessions").mockRejectedValue("the profile cannot be read");
  await renderCard(bridge, { runs: [{ name: "dev", session: "1", running: true, port: 5173 }] });

  const devItem = within(row("Scripts")).getByRole("listitem", { name: "dev" });
  expect(within(devItem).getByRole("button", { name: "Stop dev" })).toBeTruthy();
  const servers = within(row("Servers")).getAllByRole("listitem");
  expect(servers.map((item) => item.textContent)).toEqual(["devlocalhost:5173"]);
  expect(screen.getByRole("alert").textContent).toBe("the profile cannot be read");
});
