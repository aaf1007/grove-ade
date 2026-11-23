import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { DetailsCard, type FocusedSession } from "./DetailsCard";
import type { UsageReading } from "./bridge";
import { FAKE_MODELS, type FakeChange, fakeBridge } from "./fakeBridge";

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.restoreAllMocks();
  vi.useRealTimers();
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
  session: "0",
  agent: "Claude",
  model: { model: FAKE_MODELS.Claude[1], effort: null },
  mode: "Chat",
  state: { session: "0", state: "Working", source: "Protocol" },
};

async function renderCard(
  bridge: Awaited<ReturnType<typeof setup>>,
  props: Partial<Parameters<typeof DetailsCard>[0]> = {},
) {
  const handlers = {
    onStartRun: vi.fn(),
    onOpenDiff: vi.fn(),
    onOpenProfile: vi.fn(),
  };
  const card = (more: typeof props) => (
    <DetailsCard
      bridge={bridge}
      worktree={feature}
      runs={[]}
      session={claude}
      {...handlers}
      {...props}
      {...more}
    />
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

  expect(within(row("Worktree")).getByTitle(feature)).toBeTruthy();

  await act(async () => fireEvent.click(within(row("Worktree")).getByRole("button", { name: "Reveal in Finder" })));
  expect(bridge.revealed).toEqual([feature]);
});

test("a long worktree path is cut in the middle: its start gives way, its last folder stays whole, the tooltip has it all", async () => {
  const long = "/private/tmp/grove-verify.MTLB9xQ2/worktrees/feat-82-details-card-pin";
  const bridge = await setup();
  await renderCard(bridge, { worktree: long });

  const path = within(row("Worktree")).getByTitle(long);
  expect(within(path).getByText("/private/tmp/grove-verify.MTLB9xQ2/worktrees").className).toBe("details-path-start");
  expect(within(path).getByText("/feat-82-details-card-pin").className).toBe("details-path-end");
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

test("the changes row counts the lines since the base branch, and opens the Diff panel on the merge base", async () => {
  const bridge = await setup();
  const { onOpenDiff } = await renderCard(bridge);

  const changes = within(row("Changes")).getByRole("button");
  expect(changes.textContent).toBe("Changesvs develop+15 −3");

  fireEvent.click(changes);
  expect(onOpenDiff).toHaveBeenCalledWith("MergeBase");
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
  const { onOpenDiff } = await renderCard(bridge as Awaited<ReturnType<typeof setup>>, {
    worktree: main,
  });
  expect(within(row("Changes")).getByRole("button").textContent).toBe("Changesvs HEAD+1 −0");
  fireEvent.click(within(row("Changes")).getByRole("button"));
  expect(onOpenDiff).toHaveBeenCalledWith("Head");

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

// A time and a day as the card writes them, in the machine's locale.
const clock = (at: Date) => at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
const day = (at: Date) => at.toLocaleDateString([], { month: "short", day: "numeric" });

test("the session block shows the focused session's title, its state in words since when, and where the state came from", async () => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(new Date(2026, 10, 20, 9, 30));
  const bridge = await setup();
  const session = await bridge.spawnAgent(feature, "Claude");
  await bridge.sendMessage(session, "Fix the login bug\nthen run the tests");
  vi.setSystemTime(new Date(2026, 10, 20, 14, 5));
  const approval = { session, state: "NeedsInput", kind: "Approval", source: "Hook" } as const;
  bridge.agentStateChanged(approval);
  const { rerender } = await renderCard(bridge, { session: { ...claude, session, state: approval } });

  const block = () => within(card()).getByRole("group", { name: "Agent session" });
  // The title is cut to one line, and its tooltip holds it all.
  expect(within(block()).getByRole("heading", { name: "Fix the login bug" }).title).toBe("Fix the login bug");
  expect(within(block()).getByText("Approval")).toBeTruthy();
  expect(within(block()).getByText(`since ${clock(new Date(2026, 10, 20, 14, 5))}`)).toBeTruthy();
  expect(within(block()).getByText("State from Hook")).toBeTruthy();

  vi.setSystemTime(new Date(2026, 10, 20, 14, 20));
  const working = { session, state: "Working", source: "Protocol" } as const;
  await act(async () => bridge.agentStateChanged(working));
  await act(async () => rerender({ session: { ...claude, session, state: working } }));
  expect(within(block()).getByText("Working")).toBeTruthy();
  expect(within(block()).getByText(`since ${clock(new Date(2026, 10, 20, 14, 20))}`)).toBeTruthy();
});

test("before any new state reaches the window, the session block shows the state the core lists, without a source", async () => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(new Date(2026, 10, 20, 14, 5));
  const bridge = await setup();
  const session = await bridge.spawnAgent(feature, "Claude");
  bridge.agentStateChanged({ session, state: "Working", source: "Protocol" });
  await renderCard(bridge, { session: { ...claude, session, state: undefined } });

  const block = within(card()).getByRole("group", { name: "Agent session" });
  expect(within(block).getByText("Working")).toBeTruthy();
  expect(within(block).getByText(`since ${clock(new Date(2026, 10, 20, 14, 5))}`)).toBeTruthy();
  expect(within(block).queryByText(/^State from/)).toBeNull();
});

test("a new state shows no since time until the core lists the time it was entered", async () => {
  const bridge = await setup();
  const session = await bridge.spawnAgent(feature, "Claude");
  const working = { session, state: "Working", source: "Protocol" } as const;
  bridge.agentStateChanged(working);
  const { rerender } = await renderCard(bridge, { session: { ...claude, session, state: working } });
  const block = () => within(card()).getByRole("group", { name: "Agent session" });
  expect(within(block()).getByText(/^since /)).toBeTruthy();

  vi.spyOn(bridge, "agentSessions").mockReturnValue(new Promise(() => {}));
  const approval = { session, state: "NeedsInput", kind: "Approval", source: "Protocol" } as const;
  await act(async () => bridge.agentStateChanged(approval));
  await act(async () => rerender({ session: { ...claude, session, state: approval } }));

  expect(within(block()).getByText("Approval")).toBeTruthy();
  expect(within(block()).queryByText(/^since /)).toBeNull();
});

test("a state entered on another day shows its date with its time", async () => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(new Date(2026, 10, 19, 14, 5));
  const bridge = await setup();
  const session = await bridge.spawnAgent(feature, "Claude");
  const idle = { session, state: "Idle", source: "Protocol" } as const;
  bridge.agentStateChanged(idle);
  vi.setSystemTime(new Date(2026, 10, 20, 9, 30));
  await renderCard(bridge, { session: { ...claude, session, state: idle } });

  const block = within(card()).getByRole("group", { name: "Agent session" });
  const entered = new Date(2026, 10, 19, 14, 5);
  expect(within(block).getByText(`since ${day(entered)}, ${clock(entered)}`)).toBeTruthy();
});

test("a listing of the agent sessions that fails after a later one succeeded says nothing", async () => {
  const bridge = await setup();
  const session = await bridge.spawnAgent(feature, "Claude");
  const listings: { resolve: () => void; reject: () => void }[] = [];
  const list = bridge.agentSessions.bind(bridge);
  vi.spyOn(bridge, "agentSessions").mockImplementation(
    () =>
      new Promise((resolve, reject) =>
        listings.push({ resolve: () => void list().then(resolve), reject: () => reject("the core is busy") }),
      ),
  );
  await renderCard(bridge, { session: { ...claude, session } });
  await act(async () => bridge.agentStateChanged({ session, state: "Working", source: "Protocol" }));

  await act(async () => listings[1].resolve());
  await act(async () => listings[0].reject());

  expect(within(card()).queryByRole("alert")).toBeNull();
});

test("the session row shows the focused session's agent, model, effort and mode", async () => {
  const bridge = await setup();
  await renderCard(bridge, {
    session: {
      ...claude,
      model: { model: FAKE_MODELS.Claude[0], effort: "max" },
    },
  });

  expect(within(row("Session")).getByText("Claude Code")).toBeTruthy();
  expect(within(row("Session")).getByText("Default (recommended) · max · Chat")).toBeTruthy();
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

const fiveHourReset = new Date(2026, 10, 22, 14, 0);
const weeklyReset = new Date(2026, 10, 27, 9, 30);
const twoWindows: UsageReading = {
  limits: {
    kind: "Windows",
    windows: [
      { name: "5-hour", percentUsed: 42, resetsAtUnixMs: fiveHourReset.getTime() },
      { name: "Weekly", percentUsed: 18, resetsAtUnixMs: weeklyReset.getTime() },
    ],
  },
  readAtUnixMs: new Date(2026, 10, 22, 11, 48).getTime(),
  latestReadFailed: false,
};

test("the usage limits row shows each window of the agent's plan with its percent and reset time", async () => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(new Date(2026, 10, 22, 12, 0));
  const bridge = await setup();
  bridge.usageReadingChanged({ agent: "Claude", reading: twoWindows });
  await renderCard(bridge);

  const usage = within(row("Usage limits"));
  expect(within(usage.getByRole("group", { name: "5-hour" })).getByText(`42% · resets ${clock(fiveHourReset)}`)).toBeTruthy();
  expect(
    within(usage.getByRole("group", { name: "Weekly" })).getByText(
      `18% · resets ${day(weeklyReset)}, ${clock(weeklyReset)}`,
    ),
  ).toBeTruthy();
  // A reading that is up to date says nothing of its age.
  expect(usage.queryByText(/ago/)).toBeNull();
});

test("an account with no plan limits says so in the usage limits row", async () => {
  const bridge = await setup();
  bridge.usageReadingChanged({
    agent: "Claude",
    reading: { limits: { kind: "NoPlanLimits" }, readAtUnixMs: Date.now(), latestReadFailed: false },
  });
  await renderCard(bridge);

  expect(within(row("Usage limits")).getByText("No plan limits")).toBeTruthy();
  expect(within(row("Usage limits")).queryByRole("group")).toBeNull();
});

test("after a failed read the usage limits row keeps the last good reading and shows its age, which goes on counting", async () => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  vi.setSystemTime(new Date(2026, 10, 22, 12, 0));
  const bridge = await setup();
  bridge.usageReadingChanged({ agent: "Claude", reading: twoWindows });
  await renderCard(bridge);

  await act(async () => bridge.usageReadingChanged({ agent: "Claude", reading: { ...twoWindows, latestReadFailed: true } }));

  const usage = within(row("Usage limits"));
  expect(usage.getByText("last read 12 min ago")).toBeTruthy();
  expect(within(usage.getByRole("group", { name: "5-hour" })).getByText(/^42% · resets/)).toBeTruthy();
  expect(within(usage.getByRole("group", { name: "Weekly" })).getByText(/^18% · resets/)).toBeTruthy();

  await act(async () => vi.advanceTimersByTime(3 * 60_000));
  expect(usage.getByText("last read 15 min ago")).toBeTruthy();
});

test("the card has no usage limits row before a reading, for another agent's reading, or once the agent cannot give one", async () => {
  const bridge = await setup();
  bridge.usageReadingChanged({ agent: "Codex", reading: twoWindows });
  await renderCard(bridge);
  expect(rowNames()).not.toContain("Usage limits");

  await act(async () => bridge.usageReadingChanged({ agent: "Claude", reading: twoWindows }));
  expect(rowNames()).toContain("Usage limits");

  await act(async () => bridge.usageReadingChanged({ agent: "Claude", reading: null }));
  expect(rowNames()).not.toContain("Usage limits");
});

test("the context window row shows how full the focused session's context is and its tokens so far, after each turn", async () => {
  const bridge = await setup();
  const session = await bridge.spawnAgent(feature, "Claude");
  const other = await bridge.spawnAgent(feature, "Claude");
  await renderCard(bridge, { session: { ...claude, session } });
  expect(rowNames()).not.toContain("Context window");

  await act(async () =>
    bridge.contextReadingChanged({
      session,
      reading: { sessionTotalTokens: 101_000, contextWindow: { usedTokens: 84_000, sizeTokens: 200_000 } },
    }),
  );
  const context = within(row("Context window"));
  expect(context.getByText("42% · 84k of 200k")).toBeTruthy();
  expect(within(context.getByRole("group", { name: "Session tokens" })).getByText("101k")).toBeTruthy();

  // Another session's reading leaves the row as it is.
  await act(async () =>
    bridge.contextReadingChanged({
      session: other,
      reading: { sessionTotalTokens: 5, contextWindow: { usedTokens: 5, sizeTokens: 200_000 } },
    }),
  );
  expect(context.getByText("42% · 84k of 200k")).toBeTruthy();

  await act(async () =>
    bridge.contextReadingChanged({
      session,
      reading: { sessionTotalTokens: 1_240_000, contextWindow: { usedTokens: 950, sizeTokens: 1_000_000 } },
    }),
  );
  expect(context.getByText("0% · 950 of 1M")).toBeTruthy();
  expect(within(context.getByRole("group", { name: "Session tokens" })).getByText("1.2M")).toBeTruthy();
});

test("the context window row shows the reading a session reported before the card opened, and the focused session's own", async () => {
  const bridge = await setup();
  const first = await bridge.spawnAgent(feature, "Claude");
  const second = await bridge.spawnAgent(feature, "Claude");
  bridge.contextReadingChanged({
    session: first,
    reading: { sessionTotalTokens: 101_000, contextWindow: { usedTokens: 84_000, sizeTokens: 200_000 } },
  });
  const { rerender } = await renderCard(bridge, { session: { ...claude, session: first } });
  expect(within(row("Context window")).getByText("42% · 84k of 200k")).toBeTruthy();

  await act(async () => rerender({ session: { ...claude, session: second } }));

  expect(rowNames()).not.toContain("Context window");
});

test("when the agent does not say how full the context is, the context window row shows the session's tokens alone", async () => {
  const bridge = await setup();
  const session = await bridge.spawnAgent(feature, "Codex");
  bridge.contextReadingChanged({ session, reading: { sessionTotalTokens: 30_500, contextWindow: null } });
  await renderCard(bridge, { session: { ...claude, agent: "Codex", session } });

  const context = within(row("Context window"));
  expect(within(context.getByRole("group", { name: "Session tokens" })).getByText("30.5k")).toBeTruthy();
  expect(context.queryByText(/%/)).toBeNull();
});
