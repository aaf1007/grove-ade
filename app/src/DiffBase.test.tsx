import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { App } from "./App";
import type { FileDiff } from "./bridge";
import { fakeBridge, type FakeChange } from "./fakeBridge";
import type { MonacoDiffProps } from "./MonacoDiff";

// The diff base each agent session's Diff panel shows (issue #68): the real Diff panel in the real
// window, against the fake core. Monaco needs a real browser, so the stub only shows the new text.
vi.mock("./MonacoDiff", () => ({
  MonacoDiff: ({ modified }: MonacoDiffProps) => <pre aria-label="Modified">{modified}</pre>,
}));
vi.mock("./FilesPanel", () => ({
  FilesPanel: ({ worktree }: { worktree: string }) => <p>Files of {worktree}</p>,
}));
vi.mock("./TerminalPanel", () => ({
  TerminalPanel: ({ session, shown }: { session: string; shown: boolean }) => (
    <section aria-label={`Panel ${session}`} hidden={!shown} />
  ),
}));

beforeEach(() => {
  vi.stubGlobal("requestAnimationFrame", () => 0);
  vi.stubGlobal("cancelAnimationFrame", () => {});
  const observer = class {
    observe() {}
    disconnect() {}
  };
  vi.stubGlobal("ResizeObserver", observer);
  vi.stubGlobal("IntersectionObserver", observer);
  vi.stubGlobal("matchMedia", (media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  }));
});

afterEach(() => {
  cleanup();
  localStorage.clear();
  vi.unstubAllGlobals();
});

const main = "/code/grove";
const feature = "/code/grove-wt/feat-68";

function change(newPath: string, added: number): FakeChange {
  const file: FileDiff = { status: "Modified", oldPath: newPath, newPath, added, removed: 0, binary: false };
  return { file, versions: { old: "", new: "x\n", binary: false } };
}

// feat-68 with one uncommitted change, and one more committed on its branch.
async function trackedBridge() {
  const bridge = fakeBridge(
    {
      [main]: [
        { path: main, branch: "main", isMain: true },
        { path: feature, branch: "feat-68", isMain: false },
      ],
    },
    {
      [feature]: {
        Head: [change("uncommitted.rs", 1)],
        MergeBase: [change("committed.rs", 4), change("uncommitted.rs", 1)],
      },
    },
  );
  await bridge.addRepo(main);
  return bridge;
}

async function click(element: HTMLElement) {
  await act(async () => fireEvent.click(element));
}

// The sidebar row of the `nth` agent session on `branch`, in the order they started.
function sessionRow(branch: string, nth = 0): HTMLElement | undefined {
  return [...document.querySelectorAll<HTMLElement>(".session-row")].filter(
    (row) => row.title.split("\n")[1]?.split(" · ").pop() === branch,
  )[nth];
}

// Selects the worktree by its row, or by its `nth` agent session's row once it has one.
async function select(branch: string, nth = 0) {
  const row = await vi.waitFor(() => {
    const found = sessionRow(branch, nth) ?? screen.queryByRole("button", { name: branch });
    if (!found) throw new Error(`the sidebar has no row for ${branch}`);
    return found;
  });
  await click(row);
}

// Picks `place` in the sidebar's New session picker, which starts a session there and selects it.
async function newSessionIn(place: string) {
  const nav = await screen.findByRole("navigation", { name: "Sidebar" });
  const newSession = within(nav).getByRole("button", { name: "New session" });
  await waitFor(() => expect(newSession).toHaveProperty("disabled", false));
  fireEvent.click(newSession);
  await click(screen.getByRole("menuitem", { name: place }));
}

function base(name: string) {
  return within(screen.getByRole("group", { name: "Diff base" })).getByRole("button", { name });
}

// The base the Diff panel's switch shows.
function shownBase() {
  return ["HEAD", "Merge base"].find((name) => base(name).getAttribute("aria-pressed") === "true");
}

// The changed files the Diff panel lists.
function changedFiles() {
  return within(screen.getByRole("list", { name: "Changed files" }))
    .queryAllByRole("listitem")
    .map((file) => file.getAttribute("aria-label"));
}

test("a new session's Diff panel opens on uncommitted changes, and the base the user picks comes back after a reload", async () => {
  const bridge = await trackedBridge();
  const { unmount } = render(<App bridge={bridge} />);
  await newSessionIn("feat-68");
  await screen.findByRole("region", { name: "Panel 0" });
  await click(screen.getByRole("tab", { name: "Diff" }));

  expect(shownBase()).toBe("HEAD");
  await waitFor(() => expect(changedFiles()).toEqual(["uncommitted.rs"]));
  expect((await bridge.workspace("0")).diffBase).toBe("Head");

  // The whole branch is one click away.
  await click(base("Merge base"));

  expect(shownBase()).toBe("Merge base");
  await waitFor(() => expect(changedFiles()).toEqual(["committed.rs", "uncommitted.rs"]));
  await waitFor(async () => expect((await bridge.workspace("0")).diffBase).toBe("MergeBase"));
  unmount();

  // The fake core kept the session's workspace, as the core does across a restart.
  render(<App bridge={bridge} />);
  await select("feat-68");

  expect(screen.getByRole("tab", { name: "Diff", selected: true })).toBeTruthy();
  expect(shownBase()).toBe("Merge base");
  await waitFor(() => expect(changedFiles()).toEqual(["committed.rs", "uncommitted.rs"]));
});

test("each session keeps its own diff base, also two sessions in one place", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await newSessionIn("feat-68");
  await screen.findByRole("region", { name: "Panel 0" });
  await click(screen.getByRole("tab", { name: "Diff" }));
  await click(base("Merge base"));
  await waitFor(() => expect(changedFiles()).toEqual(["committed.rs", "uncommitted.rs"]));

  // A second session there opens on uncommitted changes, whatever the first one shows.
  await newSessionIn("feat-68");
  await screen.findByRole("region", { name: "Panel 1" });
  await click(screen.getByRole("tab", { name: "Diff" }));

  expect(shownBase()).toBe("HEAD");
  await waitFor(() => expect(changedFiles()).toEqual(["uncommitted.rs"]));

  await select("feat-68", 0);

  expect(shownBase()).toBe("Merge base");
  await waitFor(() => expect(changedFiles()).toEqual(["committed.rs", "uncommitted.rs"]));
  expect((await bridge.workspace("0")).diffBase).toBe("MergeBase");
  expect((await bridge.workspace("1")).diffBase).toBe("Head");
});

test("a session started where the place showed the whole branch still opens on uncommitted changes", async () => {
  const bridge = await trackedBridge();
  render(<App bridge={bridge} />);
  await select("feat-68");
  // Another tab too, so the new session takes the layout the place showed.
  await click(screen.getByRole("button", { name: "Open a panel" }));
  await click(within(screen.getByRole("menu", { name: "Open a panel" })).getByRole("menuitemcheckbox", { name: "Logs" }));
  await click(screen.getByRole("tab", { name: "Diff" }));
  await click(base("Merge base"));
  expect(shownBase()).toBe("Merge base");

  await click(screen.getByRole("button", { name: "Start session" }));
  await screen.findByRole("region", { name: "Panel 0" });

  await waitFor(async () => expect((await bridge.workspace("0")).panels).toEqual(["Files", "Diff", "Logs"]));
  expect(shownBase()).toBe("HEAD");
  expect((await bridge.workspace("0")).diffBase).toBe("Head");
});

test("a place that is not a git repo says it has no diff, and shows no error", async () => {
  const bridge = await trackedBridge();
  bridge.makePlainFolder(feature);
  render(<App bridge={bridge} />);
  await newSessionIn("feat-68");
  await screen.findByRole("region", { name: "Panel 0" });

  await click(screen.getByRole("tab", { name: "Diff" }));

  expect(await screen.findByText("This folder is not a git repo, so it has no diff.")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
  expect(screen.queryByRole("group", { name: "Diff base" })).toBeNull();
});
