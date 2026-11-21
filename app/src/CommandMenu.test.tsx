import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { AgentStateChanged, SlashCommand } from "./bridge";
import { ChatPanel } from "./ChatPanel";
import { fakeBridge, type FakeBridge } from "./fakeBridge";

const worktree = "/code/grove-wt/feat-13";

const review: SlashCommand = {
  name: "review",
  description: "Review a pull request",
  argumentHint: "<pr>",
  invocation: "/review",
};
const init: SlashCommand = { name: "init", description: "Write a CLAUDE.md", argumentHint: "", invocation: "/init" };
const preview: SlashCommand = {
  name: "pr-review",
  description: "Review the open PR",
  argumentHint: "",
  invocation: "/pr-review",
};

// Animation frames run only when a test calls `nextFrame`.
let frames: FrameRequestCallback[] = [];

function nextFrame() {
  const due = frames;
  frames = [];
  act(() => due.forEach((callback) => callback(performance.now())));
}

beforeEach(() => {
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => frames.push(callback));
  vi.stubGlobal("cancelAnimationFrame", () => {});
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  Element.prototype.scrollIntoView = () => {};
});

afterEach(() => {
  cleanup();
  frames = [];
  vi.unstubAllGlobals();
  delete (Element.prototype as Partial<Element>).scrollIntoView;
});

async function started(): Promise<FakeBridge> {
  const bridge = fakeBridge({ [worktree]: [{ path: worktree, branch: "feat-13", isMain: true }] });
  await bridge.addRepo(worktree);
  await bridge.spawnAgent(worktree, "Claude", null);
  return bridge;
}

async function shown(bridge: FakeBridge, state?: AgentStateChanged) {
  render(
    <ChatPanel
      bridge={bridge}
      session="0"
      agent="Claude"
      worktree={worktree}
      shown
      state={state}
      focusRequest={0}
      onOpenDiff={() => {}}
    />,
  );
  await act(async () => {});
}

/** A panel whose agent listed `commands` after it attached. */
async function listing(...commands: SlashCommand[]): Promise<FakeBridge> {
  const bridge = await started();
  await shown(bridge);
  act(() => bridge.messages("0", { type: "Commands", commands }));
  return bridge;
}

function composer() {
  return screen.getByRole("textbox", { name: "Message" }) as HTMLTextAreaElement;
}

function type(text: string) {
  composer().focus();
  fireEvent.change(composer(), { target: { value: text } });
}

function key(name: string, init: KeyboardEventInit = {}) {
  fireEvent.keyDown(composer(), { key: name, ...init });
}

function menu() {
  return screen.queryByRole("listbox", { name: "Commands" });
}

function rows() {
  return within(menu()!).getAllByRole("option");
}

function selected() {
  return within(menu()!).getByRole("option", { selected: true });
}

test("a / at the start of the composer lists the agent's commands with name, description and argument hint", async () => {
  await listing(review, init);

  type("/");

  expect(rows().map((row) => row.textContent)).toEqual([
    "/review<pr>Review a pull request",
    "/initWrite a CLAUDE.md",
  ]);
  expect(selected()).toBe(rows()[0]);
});

test("typing filters the list, names that start with the text first", async () => {
  await listing(preview, review, init);

  type("/rev");
  expect(rows().map((row) => row.textContent)).toEqual([
    "/review<pr>Review a pull request",
    "/pr-reviewReview the open PR",
  ]);

  type("/xyz");
  expect(menu()).toBeNull();
});

test("Up and Down move through the rows and wrap, and Enter picks the row", async () => {
  const bridge = await listing(review, init);
  type("/");

  key("ArrowDown");
  expect(selected()).toBe(rows()[1]);
  key("ArrowDown");
  expect(selected()).toBe(rows()[0]);
  key("ArrowUp");
  expect(selected()).toBe(rows()[1]);
  key("Enter");

  expect(composer().value).toBe("/init ");
  expect(menu()).toBeNull();
  expect(document.activeElement).toBe(composer());
  expect(bridge.sent).toEqual([]);
});

test("Tab picks the row too, and the picked command then sends as a normal message", async () => {
  const bridge = await listing(review, init);
  type("/re");

  key("Tab");
  expect(composer().value).toBe("/review ");
  type("/review 112");
  key("Enter");

  expect(bridge.sent).toEqual([{ session: "0", text: "/review 112" }]);
});

test("a click on a row picks it and keeps focus in the composer", async () => {
  await listing(review, init);
  type("/");

  fireEvent.mouseDown(rows()[1]);
  fireEvent.click(rows()[1]);

  expect(composer().value).toBe("/init ");
  expect(document.activeElement).toBe(composer());
});

test("Escape closes the menu, and Enter then sends what was typed", async () => {
  const bridge = await listing(review);
  type("/rev");

  key("Escape");
  expect(menu()).toBeNull();
  key("Enter");

  expect(bridge.sent).toEqual([{ session: "0", text: "/rev" }]);
});

test("after Escape, a new / draft opens the menu again", async () => {
  await listing(review, init);
  type("/");
  key("Escape");

  type("");
  type("/");
  expect(rows()).toHaveLength(2);

  key("Escape");
  type("/in");
  expect(rows().map((row) => row.textContent)).toEqual(["/initWrite a CLAUDE.md"]);
});

test("a / that is not at the start of the composer opens nothing", async () => {
  await listing(review);

  type("fix the /re");
  expect(menu()).toBeNull();
  type(" /");
  expect(menu()).toBeNull();
});

test("with no commands listed there is no menu, and Enter sends", async () => {
  const bridge = await started();
  await shown(bridge);

  type("/review");
  expect(menu()).toBeNull();
  key("Enter");

  expect(bridge.sent).toEqual([{ session: "0", text: "/review" }]);
});

test("a later list replaces the one before", async () => {
  const bridge = await listing(review, init);

  act(() => bridge.messages("0", { type: "Commands", commands: [init] }));
  type("/");

  expect(rows().map((row) => row.textContent)).toEqual(["/initWrite a CLAUDE.md"]);
});

test("a panel that attaches after the list came still gets it, and the list is not in the conversation", async () => {
  const bridge = await started();
  bridge.messages("0", { type: "Commands", commands: [review] });
  await shown(bridge);
  nextFrame();

  type("/");

  expect(rows()).toHaveLength(1);
  expect(screen.queryByText("Review a pull request", { selector: ".thread *" })).toBeNull();
});

test("the menu is a listbox the composer controls, and the selected row is read out", async () => {
  await listing(review, init);
  type("/");
  key("ArrowDown");

  expect(composer().getAttribute("aria-controls")).toBe(menu()!.id);
  expect(composer().getAttribute("aria-activedescendant")).toBe(selected().id);
  expect(screen.getByRole("status").textContent).toBe("/init, Write a CLAUDE.md");

  key("Escape");
  expect(composer().getAttribute("aria-activedescendant")).toBeNull();
  expect(composer().getAttribute("aria-controls")).toBeNull();
});

test("keys in the approval card go to the card, not the menu", async () => {
  const bridge = await listing(review);
  const answer = vi.spyOn(bridge, "answer");
  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "toolu_01", tool: "Bash", input: { command: "npm test" } },
      { type: "ApprovalRequested", request: "toolu_01", tool: "Bash", input: { command: "npm test" } },
    ),
  );
  nextFrame();
  type("/");
  const card = screen.getByRole("group", { name: "Claude Code wants to run a command" });

  fireEvent.keyDown(card, { key: "y" });
  fireEvent.keyDown(card, { key: "Enter" });

  expect(answer).toHaveBeenCalledWith("0", "toolu_01", { type: "AllowOnce" });
  expect(composer().value).toBe("/");
  expect(menu()).not.toBeNull();
});
