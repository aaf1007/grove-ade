import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test } from "vitest";

import { fakeBridge } from "./fakeBridge";
import { Warnings } from "./Warnings";

afterEach(cleanup);

const main = "/code/grove";
const feature = "/code/grove-wt/feat-36";
const cause = `git2 failed in ${feature}: could not find repository`;

async function unwatchedBridge() {
  const bridge = fakeBridge({
    [main]: [
      { path: main, branch: "main", isMain: true },
      { path: feature, branch: "feat-36", isMain: false },
    ],
  });
  await bridge.addRepo(main);
  bridge.warn({ kind: "WatcherNotStarted", worktree: feature, reason: cause });
  return bridge;
}

test("a watcher that could not start shows its worktree and the cause", async () => {
  render(<Warnings bridge={await unwatchedBridge()} />);

  const warning = screen.getByRole("status");

  expect(warning.querySelector("p")!.textContent).toBe(
    `grove could not watch ${feature}, so its diff does not update live: ${cause}.`,
  );
});

test("Retry retries the worktree's watcher and the warning goes once it starts", async () => {
  const bridge = await unwatchedBridge();
  render(<Warnings bridge={bridge} />);

  fireEvent.click(within(screen.getByRole("status")).getByRole("button", { name: "Retry" }));

  expect(bridge.watcherRetries).toEqual([feature]);
  await act(async () => {});
  expect(screen.queryByRole("status")).toBeNull();
});

test("a failed Retry keeps the warning and shows the new cause", async () => {
  const bridge = await unwatchedBridge();
  bridge.failWatcherRetry(feature, "could not watch: too many open files");
  render(<Warnings bridge={bridge} />);

  fireEvent.click(within(screen.getByRole("status")).getByRole("button", { name: "Retry" }));

  await expect
    .poll(() => screen.getByRole("status").querySelector("p")!.textContent)
    .toBe(`grove could not watch ${feature}, so its diff does not update live: could not watch: too many open files.`);
});

test("Codex hooks that are not approved show the worktree and say to approve them in /hooks", () => {
  const bridge = fakeBridge({});
  bridge.warn({ kind: "CodexHooksNotTrusted", session: "7", worktree: feature });
  render(<Warnings bridge={bridge} />);

  expect(screen.getByRole("status").querySelector("p")!.textContent).toBe(
    `Codex is not running grove's hooks in ${feature}, so grove guesses the session's state. Approve the hooks in Codex's /hooks.`,
  );
});

test("no warning shows no bar", () => {
  render(<Warnings bridge={fakeBridge({})} />);

  expect(screen.queryByRole("status")).toBeNull();
});

test("other warnings have no Retry", async () => {
  const bridge = fakeBridge({});
  bridge.warn({ kind: "LoginShellEnv", reason: "/bin/zsh did not print its environment within 5s" });
  render(<Warnings bridge={bridge} />);

  expect(within(screen.getByRole("status")).queryByRole("button", { name: "Retry" })).toBeNull();
});
