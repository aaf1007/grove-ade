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
    file: {
      status: "Modified",
      oldPath: path,
      newPath: path,
      binary: false,
      added,
      removed,
    },
    versions: { old: "", new: "", binary: false },
  };
}

async function setup({ runSessions = RUN_SESSIONS } = {}) {
  const bridge = fakeBridge(
    {
      [main]: [
        { path: main, branch: "main", isMain: true },
        {
          path: feature,
          branch: "feat-36",
          isMain: false,
          baseBranch: "develop",
        },
      ],
    },
    {
      [feature]: {
        Head: [change("src/a.rs", 3, 1)],
        MergeBase: [change("src/a.rs", 10, 2), change("src/b.rs", 4, 0), change("src/c.rs", 1, 1)],
      },
    },
  );
  await bridge.addRepo(main);
  await bridge.setRepoOverridesText(main, runSessions);
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
  const handlers = {
    onStartRun: vi.fn(),
    onOpenDiff: vi.fn(),
    onOpenProfile: vi.fn(),
    onClose: vi.fn(),
  };
  const card = (more: typeof props) => (
    <DetailsCard bridge={bridge} worktree={feature} runs={[]} session={claude} {...handlers} {...props} {...more} />
  );
  let view!: ReturnType<typeof render>;
  await act(async () => {
    view = render(card({}));
  });
  return {
    ...handlers,
    rerender: (more: typeof props) => view.rerender(card(more)),
  };
}

const card = () => screen.getByRole("region", { name: "Details" });
const row = (name: string) => within(card()).getByRole("listitem", { name });
const rowNames = () =>
  within(card())
    .getAllByRole("listitem")
    .map((item) => item.getAttribute("aria-label"));

test("the card lists its rows in order, with a divider before the branch", async () => {
  const bridge = await setup();
  await renderCard(bridge, {
    runs: [{ name: "dev", session: "1", running: true, port: null }],
  });

  expect(rowNames()).toEqual([
    "Worktree",
    "Open in",
    "Run dev",
    "Run tests",
    "Add run session",
    "Branch",
    "Changes",
    "Servers",
    "Session",
  ]);
  expect(within(card()).getByRole("separator").nextElementSibling?.getAttribute("aria-label")).toBe("Branch");
});

test("the card has no row for data the worktree does not have", async () => {
  const bridge = await setup({ runSessions: "" });
  await renderCard(bridge, { session: null });

  expect(rowNames()).toEqual(["Worktree", "Open in", "Add run session", "Branch", "Changes"]);
});

test("the worktree row shows its folder and reveals it in Finder", async () => {
  const bridge = await setup();
  await renderCard(bridge);

  expect(within(row("Worktree")).getByText(feature)).toBeTruthy();

  await act(async () => fireEvent.click(within(row("Worktree")).getByRole("button", { name: "Reveal in Finder" })));
  expect(bridge.revealed).toEqual([feature]);
});

test("Open in opens the worktree in the editor picked from its menu", async () => {
  const bridge = await setup();
  await renderCard(bridge);

  fireEvent.click(within(row("Open in")).getByRole("button", { name: "Open in" }));
  await act(async () => fireEvent.click(screen.getByRole("menuitem", { name: "Zed" })));
  fireEvent.click(within(row("Open in")).getByRole("button", { name: "Open in" }));
  await act(async () => fireEvent.click(screen.getByRole("menuitem", { name: "VS Code" })));

  expect(bridge.opened).toEqual([
    { editor: "Zed", worktree: feature, file: "", line: 1 },
    { editor: "VsCode", worktree: feature, file: "", line: 1 },
  ]);
});

test("a run session row starts its run session, and a running one shows it and stops it", async () => {
  const bridge = await setup();
  const dev = await bridge.spawnRun(feature, "dev", 80, 24);
  const { onStartRun } = await renderCard(bridge, {
    runs: [{ name: "dev", session: dev, running: true, port: null }],
  });

  expect(within(row("Run tests")).getByText("cargo test")).toBeTruthy();
  await act(async () => fireEvent.click(within(row("Run tests")).getByRole("button", { name: "Start tests" })));
  expect(onStartRun).toHaveBeenCalledWith("tests");

  expect(within(row("Run dev")).getByText("Running")).toBeTruthy();
  await act(async () => fireEvent.click(within(row("Run dev")).getByRole("button", { name: "Stop dev" })));
  expect(bridge.killed).toEqual([dev]);
});

test("a stopped run session says so and can start again", async () => {
  const bridge = await setup();
  const { onStartRun } = await renderCard(bridge, {
    runs: [{ name: "dev", session: "9", running: false, port: null }],
  });

  expect(within(row("Run dev")).getByText("Stopped")).toBeTruthy();
  await act(async () => fireEvent.click(within(row("Run dev")).getByRole("button", { name: "Start dev" })));
  expect(onStartRun).toHaveBeenCalledWith("dev");
});

test("Add run session opens the Profile panel", async () => {
  const bridge = await setup();
  const { onOpenProfile } = await renderCard(bridge);

  fireEvent.click(
    within(row("Add run session")).getByRole("button", {
      name: "Add run session",
    }),
  );

  expect(onOpenProfile).toHaveBeenCalled();
});

test("the branch row shows the branch, and Change base in its menu changes the base", async () => {
  const bridge = await setup();
  await renderCard(bridge);

  expect(within(row("Branch")).getByText("feat-36")).toBeTruthy();
  expect(within(row("Changes")).getByText("vs develop")).toBeTruthy();

  fireEvent.click(within(row("Branch")).getByRole("button", { name: "Branch" }));
  await act(async () => fireEvent.click(screen.getByRole("menuitem", { name: "Change base" })));
  const dialog = screen.getByRole("dialog", { name: "Change base branch" });
  fireEvent.change(within(dialog).getByLabelText("Base branch"), {
    target: { value: "main" },
  });
  await act(async () => fireEvent.click(within(dialog).getByRole("button", { name: "Save" })));

  expect(screen.queryByRole("dialog")).toBeNull();
  expect(within(row("Changes")).getByText("vs main")).toBeTruthy();
});

test("the changes row hides the old base's counts while the new base's diff loads", async () => {
  const bridge = await setup();
  await renderCard(bridge);
  expect(within(row("Changes")).getByText("vs develop")).toBeTruthy();
  vi.spyOn(bridge, "diff").mockReturnValue(new Promise(() => {}));

  fireEvent.click(within(row("Branch")).getByRole("button", { name: "Branch" }));
  await act(async () => fireEvent.click(screen.getByRole("menuitem", { name: "Change base" })));
  const dialog = screen.getByRole("dialog", { name: "Change base branch" });
  fireEvent.change(within(dialog).getByLabelText("Base branch"), { target: { value: "main" } });
  await act(async () => fireEvent.click(within(dialog).getByRole("button", { name: "Save" })));

  expect(within(card()).queryByRole("listitem", { name: "Changes" })).toBeNull();
});

test("the changes row counts the lines since the base branch, and opens the Diff panel", async () => {
  const bridge = await setup();
  const { onOpenDiff } = await renderCard(bridge);

  const changes = within(row("Changes")).getByRole("button");
  expect(changes.textContent).toBe("Changesvs develop+15 −3");

  fireEvent.click(changes);
  expect(onOpenDiff).toHaveBeenCalled();
});

test("on a worktree with no base branch, the changes row counts uncommitted changes and follows them", async () => {
  const diffs = { Head: [change("src/a.rs", 1, 0)] };
  const bridge = fakeBridge(
    {
      [main]: [{ path: main, branch: "main", isMain: true, baseBranch: null }],
    },
    { [main]: diffs },
  );
  await bridge.addRepo(main);
  await renderCard(bridge as Awaited<ReturnType<typeof setup>>, {
    worktree: main,
  });
  expect(within(row("Changes")).getByRole("button").textContent).toBe("Changesvs HEAD+1 −0");

  diffs.Head = [];
  await act(async () => bridge.diffChanged(main));

  expect(within(row("Changes")).getByRole("button").textContent).toBe("Changesvs HEAD+0 −0");
});

test("a diff that cannot be read drops the changes row and says why", async () => {
  const bridge = await setup();
  vi.spyOn(bridge, "diff").mockRejectedValue("the base branch is gone");
  await renderCard(bridge);

  expect(within(card()).queryByRole("listitem", { name: "Changes" })).toBeNull();
  expect(screen.getByRole("alert").textContent).toBe("the base branch is gone");
});

test("the servers row shows the port of each running run session that serves one", async () => {
  const bridge = await setup();
  await renderCard(bridge, {
    runs: [
      { name: "dev", session: "1", running: true, port: null },
      { name: "tests", session: "2", running: true, port: null },
    ],
  });

  expect(within(row("Servers")).getByText(":5173 dev")).toBeTruthy();
});

test("the session row shows the focused session's agent, model, effort and mode, and its state source on hover", async () => {
  const bridge = await setup();
  await renderCard(bridge, {
    session: {
      ...claude,
      model: { model: FAKE_MODELS.Claude[0], effort: "max" },
    },
  });

  expect(within(row("Session")).getByText("Claude Code")).toBeTruthy();
  expect(within(row("Session")).getByText("Default (recommended) · max · Chat")).toBeTruthy();
  expect(row("Session").title).toBe("State from Protocol");
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

test("Escape in the branch menu or in the Change base dialog leaves the card open", async () => {
  const bridge = await setup();
  const { onClose } = await renderCard(bridge);

  fireEvent.click(within(row("Branch")).getByRole("button", { name: "Branch" }));
  fireEvent.keyDown(screen.getByRole("menuitem", { name: "Change base" }), {
    key: "Escape",
  });
  expect(screen.queryByRole("menu")).toBeNull();

  fireEvent.click(within(row("Branch")).getByRole("button", { name: "Branch" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Change base" }));
  const dialog = screen.getByRole("dialog", { name: "Change base branch" });
  fireEvent.pointerDown(within(dialog).getByLabelText("Base branch"));
  fireEvent.keyDown(within(dialog).getByLabelText("Base branch"), {
    key: "Escape",
  });

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
  fireEvent.keyDown(screen.getByRole("menuitem", { name: "A" }), {
    key: "Escape",
  });
  expect(screen.queryByRole("menu")).toBeNull();

  fireEvent.keyDown(screen.getByRole("dialog", { name: "Send review batch" }), {
    key: "Escape",
  });

  expect(onClose).not.toHaveBeenCalled();
});

test("a failed action says why in the card", async () => {
  const bridge = await setup();
  vi.spyOn(bridge, "revealInFinder").mockRejectedValueOnce("Finder is not running");
  await renderCard(bridge);

  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Reveal in Finder" })));

  expect(screen.getByRole("alert").textContent).toBe("Finder is not running");
});

test("the run session rows follow a change to the repo's run sessions", async () => {
  const bridge = await setup();
  await renderCard(bridge);
  expect(row("Run dev")).toBeTruthy();

  await act(async () =>
    bridge.setRepoOverridesText(main, `[[run_session]]\nname = "web"\ncommand = "pnpm dev"\nport = 3000\n`),
  );

  expect(within(card()).queryByRole("listitem", { name: "Run dev" })).toBeNull();
  expect(row("Run web")).toBeTruthy();
});

test("a running run session that leaves the settings keeps its row, Stop and port", async () => {
  const bridge = await setup();
  const dev = await bridge.spawnRun(feature, "dev", 80, 24);
  await renderCard(bridge, {
    runs: [{ name: "dev", session: dev, running: true, port: 5173 }],
  });

  await act(async () =>
    bridge.setRepoOverridesText(main, `[[run_session]]\nname = "web"\ncommand = "pnpm dev"\nport = 3000\n`),
  );

  expect(within(row("Run dev")).getByText("Running")).toBeTruthy();
  expect(within(row("Servers")).getByText(":5173 dev")).toBeTruthy();

  await act(async () => fireEvent.click(within(row("Run dev")).getByRole("button", { name: "Stop dev" })));
  expect(bridge.killed).toEqual([dev]);
});

test("a run session that left the settings loses its row once it exits", async () => {
  const bridge = await setup();
  const { rerender } = await renderCard(bridge, {
    runs: [{ name: "dev", session: "1", running: true, port: 5173 }],
  });
  await act(async () =>
    bridge.setRepoOverridesText(main, `[[run_session]]\nname = "web"\ncommand = "pnpm dev"\nport = 3000\n`),
  );
  expect(row("Run dev")).toBeTruthy();

  rerender({
    runs: [{ name: "dev", session: "1", running: false, port: 5173 }],
  });

  expect(within(card()).queryByRole("listitem", { name: "Run dev" })).toBeNull();
  expect(row("Run web")).toBeTruthy();
  expect(within(card()).queryByRole("listitem", { name: "Servers" })).toBeNull();
});

test("a running run session keeps its row, Stop and port when the settings cannot be read", async () => {
  const bridge = await setup();
  vi.spyOn(bridge, "runSessions").mockRejectedValue("the profile cannot be read");
  await renderCard(bridge, {
    runs: [{ name: "dev", session: "1", running: true, port: 5173 }],
  });

  expect(within(row("Run dev")).getByRole("button", { name: "Stop dev" })).toBeTruthy();
  expect(within(row("Servers")).getByText(":5173 dev")).toBeTruthy();
  expect(screen.getByRole("alert").textContent).toBe("the profile cannot be read");
});
