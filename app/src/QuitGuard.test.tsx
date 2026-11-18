import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test } from "vitest";

import type { AgentStateChanged, SessionId } from "./bridge";
import { fakeBridge } from "./fakeBridge";
import { QuitGuard } from "./QuitGuard";

afterEach(cleanup);

function states(...changes: AgentStateChanged[]): Record<SessionId, AgentStateChanged> {
  return Object.fromEntries(changes.map((change) => [change.session, change]));
}

test("Cmd-Q with no agent session working or waiting quits at once", async () => {
  const bridge = fakeBridge({});
  const settled = states(
    { session: "1", state: "Idle", source: "Protocol" },
    { session: "2", state: "Done", source: "Hook" },
    { session: "3", state: "Failed", source: "Protocol" },
    { session: "4", state: "Exited", source: "Protocol" },
  );
  render(<QuitGuard bridge={bridge} agentStates={settled} deleting={[]} />);

  act(() => bridge.requestQuit());

  await expect.poll(() => bridge.quitCount()).toBe(1);
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

test("Cmd-Q with a Working or NeedsInput agent session asks first", async () => {
  const bridge = fakeBridge({});
  const busy = states(
    { session: "1", state: "Working", source: "Protocol" },
    { session: "2", state: "NeedsInput", kind: "Approval", source: "Hook" },
    { session: "3", state: "Idle", source: "Protocol" },
  );
  render(<QuitGuard bridge={bridge} agentStates={busy} deleting={[]} />);

  act(() => bridge.requestQuit());

  const dialog = await screen.findByRole("alertdialog", { name: "Quit grove?" });
  expect(within(dialog).getByText("2 agent sessions are working or waiting on you. Quitting ends them.")).toBeTruthy();
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(bridge.quitCount()).toBe(0);

  act(() => bridge.requestQuit());
  fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Quit" }));

  await expect.poll(() => bridge.quitCount()).toBe(1);
});

test("one waiting agent session is named in the singular", async () => {
  const bridge = fakeBridge({});
  render(
    <QuitGuard
      bridge={bridge}
      agentStates={states({ session: "1", state: "NeedsInput", kind: "Question", source: "Protocol" })}
      deleting={[]}
    />,
  );

  act(() => bridge.requestQuit());

  expect(
    await screen.findByText("1 agent session is working or waiting on you. Quitting ends it."),
  ).toBeTruthy();
});

test("Cmd-Q while worktrees are being deleted asks first, and says quitting waits for them", async () => {
  const bridge = fakeBridge({});
  render(
    <QuitGuard
      bridge={bridge}
      agentStates={states({ session: "1", state: "Working", source: "Protocol" })}
      deleting={["feat-35", "feat-36"]}
    />,
  );

  act(() => bridge.requestQuit());

  const dialog = await screen.findByRole("alertdialog", { name: "Quit grove?" });
  expect(within(dialog).getByText("1 agent session is working or waiting on you. Quitting ends it.")).toBeTruthy();
  expect(within(dialog).getByText("grove is still deleting 2 worktrees. Quitting waits until they are done.")).toBeTruthy();
});
