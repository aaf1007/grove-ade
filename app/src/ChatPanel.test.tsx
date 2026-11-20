import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { Profiler } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { Agent, AgentStateChanged, Question, Reply } from "./bridge";
import { ChatPanel } from "./ChatPanel";
import { fakeBridge, type FakeBridge, type FakeChange, type FakeDiffs, type FakeMessage } from "./fakeBridge";

// TerminalPanel.test.tsx covers the terminal itself. The stub says which session it shows.
vi.mock("./TerminalPanel", () => ({
  TerminalPanel: ({ session, shown }: { session: string; shown: boolean }) => (
    <div role="region" aria-label={`Terminal of session ${session}`} hidden={!shown} />
  ),
}));

const worktree = "/code/grove-wt/feat-116";

// jsdom lays nothing out. Tests measure the composer as WebKit would: one 20px row per line, plus
// one per `wraps` extra row the panel's width makes, and nothing while hidden.
let wraps = 0;
const resizeObservers: ResizeObserverCallback[] = [];
class FakeResizeObserver {
  constructor(callback: ResizeObserverCallback) {
    resizeObservers.push(callback);
  }
  observe() {}
  disconnect() {}
}

function narrowTo(extraRows: number, width: number) {
  wraps = extraRows;
  const entry = { contentRect: { width, height: 0 } } as ResizeObserverEntry;
  act(() => resizeObservers.forEach((callback) => callback([entry], {} as ResizeObserver)));
}

// Animation frames run only when a test calls `nextFrame`.
let frames: FrameRequestCallback[] = [];

function nextFrame() {
  const due = frames;
  frames = [];
  act(() => due.forEach((callback) => callback(performance.now())));
}

beforeEach(() => {
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => frames.push(callback));
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    frames[id - 1] = () => {};
  });
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  // jsdom has no layout, so nothing to scroll.
  Element.prototype.scrollIntoView = () => {};
  vi.spyOn(HTMLTextAreaElement.prototype, "scrollHeight", "get").mockImplementation(function (
    this: HTMLTextAreaElement,
  ) {
    return this.closest("[hidden]") ? 0 : (this.value.split("\n").length + wraps) * 20;
  });
});

afterEach(() => {
  cleanup();
  frames = [];
  wraps = 0;
  resizeObservers.length = 0;
  vi.unstubAllGlobals();
  delete (Element.prototype as Partial<Element>).scrollIntoView;
  vi.restoreAllMocks();
});

async function started(agent: Agent = "Claude"): Promise<FakeBridge> {
  const bridge = fakeBridge({ [worktree]: [{ path: worktree, branch: "feat-116", isMain: true }] });
  await bridge.addRepo(worktree);
  await bridge.spawnAgent(worktree, agent, null);
  return bridge;
}

function working(state: AgentStateChanged["state"]): AgentStateChanged {
  return { session: "0", state, source: "Protocol" } as AgentStateChanged;
}

function Panel({
  bridge,
  agent = "Claude",
  shown = true,
  state,
  focusRequest = 0,
  onOpenDiff = () => {},
  onChangeAgent,
}: {
  bridge: FakeBridge;
  agent?: Agent;
  shown?: boolean;
  state?: AgentStateChanged;
  focusRequest?: number;
  onOpenDiff?: () => void;
  onChangeAgent?: (agent: Agent) => Promise<void>;
}) {
  return (
    <ChatPanel
      bridge={bridge}
      session="0"
      agent={agent}
      worktree={worktree}
      shown={shown}
      state={state}
      focusRequest={focusRequest}
      onOpenDiff={onOpenDiff}
      onChangeAgent={onChangeAgent}
    />
  );
}

async function shownPanel(bridge: FakeBridge, props: Partial<Parameters<typeof Panel>[0]> = {}) {
  const view = render(<Panel bridge={bridge} {...props} />);
  await act(async () => {});
  return view;
}

function composer() {
  return screen.getByRole("textbox", { name: "Message" });
}

function strip() {
  return screen.getByRole("group", { name: "Branch and profile" });
}

test("a stream of deltas renders at most once per animation frame and ends with the full text", async () => {
  const bridge = await started();
  let commits = 0;
  render(
    <Profiler id="panel" onRender={() => commits++}>
      <Panel bridge={bridge} />
    </Profiler>,
  );
  await act(async () => {});
  commits = 0;

  act(() => {
    bridge.messages("0", { type: "TurnStarted" });
    bridge.messages("0", { type: "TextDelta", text: "Adding " });
    bridge.messages("0", { type: "TextDelta", text: "the test" });
  });

  expect(commits).toBe(0);
  expect(frames).toHaveLength(1);
  nextFrame();
  expect(commits).toBe(1);
  act(() => bridge.messages("0", { type: "TextDelta", text: " now." }));
  nextFrame();

  expect(commits).toBe(2);
  expect(screen.getByText("Adding the test now.")).toBeTruthy();
});

test("sending a message calls send_message and shows it as a bubble at once", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  fireEvent.change(composer(), { target: { value: "add a test for the parser" } });
  fireEvent.keyDown(composer(), { key: "Enter" });

  expect(bridge.sent).toEqual([{ session: "0", text: "add a test for the parser" }]);
  expect(screen.getAllByText("add a test for the parser")).toHaveLength(1);
  expect((composer() as HTMLTextAreaElement).value).toBe("");
  // The core's copy of the message replaces the one shown at once.
  nextFrame();
  expect(screen.getAllByText("add a test for the parser")).toHaveLength(1);
});

test("Shift+Enter adds a line rather than sending", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  fireEvent.change(composer(), { target: { value: "first line" } });
  fireEvent.keyDown(composer(), { key: "Enter", shiftKey: true });

  expect(bridge.sent).toEqual([]);
});

test("a message that fails to send comes back to the composer with the core's error", async () => {
  const bridge = await started();
  vi.spyOn(bridge, "sendMessage").mockRejectedValueOnce("session 0 is not running");
  await shownPanel(bridge);

  fireEvent.change(composer(), { target: { value: "hello" } });
  await act(async () => fireEvent.keyDown(composer(), { key: "Enter" }));

  expect(screen.getByRole("alert").textContent).toBe("session 0 is not running");
  expect((composer() as HTMLTextAreaElement).value).toBe("hello");
  expect(screen.queryByText("hello", { selector: ".msg-user" })).toBeNull();
});

test("a message that fails to send goes back above what the user typed since", async () => {
  const bridge = await started();
  let fail: (error: string) => void = () => {};
  vi.spyOn(bridge, "sendMessage").mockReturnValueOnce(
    new Promise((_, reject) => (fail = reject)),
  );
  await shownPanel(bridge);

  fireEvent.change(composer(), { target: { value: "hello" } });
  fireEvent.keyDown(composer(), { key: "Enter" });
  fireEvent.change(composer(), { target: { value: "and then" } });
  await act(async () => fail("session 0 is not running"));

  expect((composer() as HTMLTextAreaElement).value).toBe("hello\nand then");
});

test("messages that fail to send come back in the order they were sent", async () => {
  const bridge = await started();
  const fails: ((error: string) => void)[] = [];
  vi.spyOn(bridge, "sendMessage").mockImplementation(
    () => new Promise((_, reject) => fails.push(reject)),
  );
  await shownPanel(bridge);

  for (const text of ["first", "second"]) {
    fireEvent.change(composer(), { target: { value: text } });
    fireEvent.keyDown(composer(), { key: "Enter" });
  }
  fireEvent.change(composer(), { target: { value: "third" } });
  await act(async () => fails[0]("session 0 is not running"));
  await act(async () => fails[1]("session 0 is not running"));

  expect((composer() as HTMLTextAreaElement).value).toBe("first\nsecond\nthird");
});

test("the composer grows with its draft, also after it was hidden or its width changed", async () => {
  const bridge = await started();
  let fail: (error: string) => void = () => {};
  vi.spyOn(bridge, "sendMessage").mockReturnValueOnce(
    new Promise((_, reject) => (fail = reject)),
  );
  const view = await shownPanel(bridge);
  const height = () => (composer() as HTMLTextAreaElement).style.height;

  fireEvent.change(composer(), { target: { value: "one\ntwo\nthree" } });
  expect(height()).toBe("60px");

  // The failed message comes back while the panel is hidden, where nothing has a size.
  fireEvent.keyDown(composer(), { key: "Enter" });
  view.rerender(<Panel bridge={bridge} shown={false} />);
  await act(async () => fail("session 0 is not running"));
  view.rerender(<Panel bridge={bridge} shown />);
  await act(async () => {});
  expect(height()).toBe("60px");

  narrowTo(2, 300);
  expect(height()).toBe("100px");
});

test("each tool call is one row with a past-tense label and no JSON, and opening it shows its output", async () => {
  const bridge = await started();
  await shownPanel(bridge, { state: working("Working") });

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "toolu_01", tool: "Bash", input: { command: "npm test" } },
      { type: "ToolResult", id: "toolu_01", output: "3 passed", isError: false },
      { type: "ToolCall", id: "toolu_02", tool: "Read", input: { file_path: `${worktree}/src/a.ts` } },
      { type: "ToolCall", id: "toolu_03", tool: "git/status", input: { short: true } },
    ),
  );
  nextFrame();

  const ran = screen.getByRole("button", { name: "Ran npm test" });
  const read = screen.getByRole("button", { name: "Read src/a.ts" });
  const used = screen.getByRole("button", { name: "Used git/status" });
  expect([ran, read, used].map((row) => row.getAttribute("aria-expanded"))).toEqual(["false", "false", "false"]);
  expect(screen.queryByText("3 passed")).toBeNull();

  fireEvent.click(ran);
  fireEvent.click(read);

  expect(ran.getAttribute("aria-expanded")).toBe("true");
  expect(screen.getByText("3 passed")).toBeTruthy();
  expect(screen.getByText("Running")).toBeTruthy();
  expect(document.querySelector(".messages")!.textContent).not.toMatch(/[{}"]/);
});

test("three Read calls then two Bash calls fold into two rows, and each opens to list its calls", async () => {
  const bridge = await started();
  await shownPanel(bridge, { state: working("Working") });

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "1", tool: "Read", input: { file_path: `${worktree}/src/a.ts` } },
      { type: "ToolResult", id: "1", output: "a", isError: false },
      { type: "ToolCall", id: "2", tool: "Read", input: { file_path: `${worktree}/src/b.ts` } },
      { type: "ToolCall", id: "3", tool: "Read", input: { file_path: "/elsewhere/c.ts" } },
      { type: "ToolCall", id: "4", tool: "Bash", input: { command: "npm test" } },
      { type: "ToolCall", id: "5", tool: "Bash", input: { command: "npm run lint\n--fix" } },
    ),
  );
  nextFrame();

  const read = screen.getByRole("button", { name: "Read 3 files" });
  const ran = screen.getByRole("button", { name: "Ran 2 commands" });
  expect(screen.queryByRole("list", { name: "Read 3 files" })).toBeNull();

  fireEvent.click(read);
  fireEvent.click(ran);

  expect(read.getAttribute("aria-expanded")).toBe("true");
  const calls = (name: string) =>
    within(screen.getByRole("list", { name })).getAllByRole("listitem").map((item) => item.textContent);
  expect(calls("Read 3 files")).toEqual(["src/a.ts", "src/b.ts", "/elsewhere/c.ts"]);
  expect(calls("Ran 2 commands")).toEqual(["npm test", "npm run lint"]);
});

test("edits fold by file, and calls of other kinds stay apart", async () => {
  const bridge = await started();
  await shownPanel(bridge, { state: working("Working") });

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "1", tool: "Edit", input: { file_path: `${worktree}/a.ts` } },
      { type: "ToolCall", id: "2", tool: "Write", input: { file_path: `${worktree}/b.ts` } },
      { type: "ToolCall", id: "3", tool: "MultiEdit", input: { file_path: `${worktree}/a.ts` } },
      { type: "ToolCall", id: "4", tool: "Grep", input: { pattern: "TODO" } },
      { type: "ToolCall", id: "5", tool: "Grep", input: { pattern: "FIXME" } },
      { type: "ToolCall", id: "6", tool: "Read", input: { file_path: `${worktree}/a.ts` } },
    ),
  );
  nextFrame();

  const rows = [...document.querySelectorAll(".thread .tool-label")].map((label) => label.textContent);
  expect(rows).toEqual(["Edited 2 files", "Searched for TODO", "Searched for FIXME", "Read a.ts"]);
});

test("when its turn ends, a turn's rows fold into one line that opens to them", async () => {
  const bridge = await started();
  const view = await shownPanel(bridge, { state: working("Working") });

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "1", tool: "commandExecution", input: { command: "/bin/zsh -lc 'touch qa.txt'" } },
      { type: "ToolResult", id: "1", output: "", isError: false },
      { type: "ToolCall", id: "2", tool: "Edit", input: { file_path: `${worktree}/a.ts` } },
      { type: "ToolResult", id: "2", output: "", isError: false },
    ),
  );
  nextFrame();
  expect(screen.getByRole("button", { name: "Ran touch qa.txt" })).toBeTruthy();

  act(() => bridge.messages("0", { type: "TurnEnded", outcome: "Completed" }));
  nextFrame();
  view.rerender(<Panel bridge={bridge} state={working("Idle")} />);

  expect(screen.queryByRole("button", { name: "Ran touch qa.txt" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Ran 1 command, edited 1 file" }));
  expect(screen.getByRole("button", { name: "Ran touch qa.txt" })).toBeTruthy();
});

test("a failed call is a row of its own in failed, with its output under it", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "1", tool: "Read", input: { file_path: `${worktree}/a.ts` } },
      { type: "ToolCall", id: "2", tool: "Read", input: { file_path: `${worktree}/b.ts` } },
      { type: "ToolResult", id: "2", output: "no such file", isError: true },
      { type: "ToolCall", id: "3", tool: "Read", input: { file_path: `${worktree}/c.ts` } },
    ),
  );
  nextFrame();

  expect(screen.getByRole("button", { name: "Read a.ts" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Read c.ts" })).toBeTruthy();
  const failed = screen.getByText("Read b.ts").closest(".tool")!;
  expect(failed.classList.contains("failed")).toBe(true);
  expect(screen.getByText("no such file").classList.contains("failed")).toBe(true);
});

test("a turn's work sits under one header, Working while the turn runs and Worked after", async () => {
  const bridge = await started();
  const view = await shownPanel(bridge, { state: working("Working") });

  act(() =>
    bridge.messages(
      "0",
      { type: "User", text: "fix it" },
      { type: "TurnStarted" },
      { type: "TextDelta", text: "Looking first." },
      { type: "ToolCall", id: "1", tool: "Read", input: { file_path: `${worktree}/a.ts` } },
      { type: "TextDelta", text: "Now the fix." },
      { type: "ToolCall", id: "2", tool: "Edit", input: { file_path: `${worktree}/a.ts` } },
    ),
  );
  nextFrame();

  const heads = () => [...document.querySelectorAll(".work-head")].map((head) => head.textContent);
  expect(heads()).toEqual(["Working"]);
  const head = document.querySelector(".work-head")!;
  const text = screen.getByText("Looking first.");
  expect(head.compareDocumentPosition(text) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

  act(() => bridge.messages("0", { type: "TurnEnded", outcome: "Completed" }));
  nextFrame();
  view.rerender(<Panel bridge={bridge} state={working("Done")} />);
  expect(heads()).toEqual(["Worked"]);

  // A turn that only talks has its own header too.
  act(() =>
    bridge.messages(
      "0",
      { type: "User", text: "thanks" },
      { type: "TurnStarted" },
      { type: "TextDelta", text: "Any time." },
      { type: "TurnEnded", outcome: "Completed" },
    ),
  );
  nextFrame();
  expect(heads()).toEqual(["Worked", "Worked"]);
});

test("a running call's label shimmers until it has a result or its turn ends", async () => {
  const bridge = await started();
  const view = await shownPanel(bridge, { state: working("Working") });
  const shimmering = () => [...document.querySelectorAll(".shimmer")].map((label) => label.textContent);

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "1", tool: "Bash", input: { command: "npm test" } },
    ),
  );
  nextFrame();
  expect(shimmering()).toEqual(["Working", "Ran npm test"]);

  act(() =>
    bridge.messages(
      "0",
      { type: "ToolResult", id: "1", output: "ok", isError: false },
      { type: "ToolCall", id: "2", tool: "Bash", input: { command: "npm run lint" } },
    ),
  );
  nextFrame();
  expect(shimmering()).toEqual(["Working", "Ran 2 commands"]);

  act(() => bridge.messages("0", { type: "TurnEnded", outcome: "Interrupted" }));
  nextFrame();
  view.rerender(<Panel bridge={bridge} state={working("Idle")} />);
  expect(shimmering()).toEqual([]);
});

test.each<[string, unknown, string]>([
  ["Bash", { command: "cargo test\n--quiet" }, "Ran cargo test"],
  ["commandExecution", { command: "npm run lint", cwd: worktree }, "Ran npm run lint"],
  ["Read", { file_path: `${worktree}/app/src/bridge.ts` }, "Read app/src/bridge.ts"],
  ["Write", { file_path: "/elsewhere/notes.md" }, "Wrote /elsewhere/notes.md"],
  ["Edit", { file_path: `${worktree}/a.ts` }, "Edited a.ts"],
  ["MultiEdit", { file_path: `${worktree}/a.ts` }, "Edited a.ts"],
  ["NotebookEdit", { notebook_path: `${worktree}/n.ipynb` }, "Edited n.ipynb"],
  ["fileChange", { changes: [{ path: `${worktree}/a.ts` }, { path: `${worktree}/b.ts` }] }, "Edited a.ts and 1 more"],
  ["Grep", { pattern: "TODO" }, "Searched for TODO"],
  ["Glob", { pattern: "**/*.css" }, "Listed **/*.css"],
  ["WebFetch", { url: "https://example.com/a" }, "Fetched https://example.com/a"],
  ["WebSearch", { query: "tauri channels" }, "Searched the web for tauri channels"],
  ["Task", { description: "Find the tests" }, "Ran a subagent: Find the tests"],
  ["TodoWrite", { todos: [] }, "Updated the to-do list"],
  ["Bash", {}, "Used Bash"],
])("a %s call reads as a sentence", async (tool, input, label) => {
  const bridge = await started();
  await shownPanel(bridge);

  act(() => bridge.messages("0", { type: "TurnStarted" }, { type: "ToolCall", id: "t", tool, input }));
  nextFrame();

  expect(screen.getByRole("button", { name: label })).toBeTruthy();
});

test("the work header counts the turn from its start while it runs, then says how long it took", async () => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  try {
    const bridge = await started();
    const view = await shownPanel(bridge, { state: working("Working") });
    const head = () => document.querySelector(".work-head")?.textContent;
    const startedAt = Date.now();

    act(() =>
      bridge.messages(
        "0",
        { type: "TurnStarted", timeUnixMs: startedAt },
        { type: "ToolCall", id: "1", tool: "Bash", input: { command: "npm test" }, timeUnixMs: startedAt },
      ),
    );
    nextFrame();
    expect(head()).toBe("Working for 0s");

    act(() => vi.advanceTimersByTime(63_000));
    expect(head()).toBe("Working for 1m 3s");

    // Waiting on the user is part of the turn, so the count goes on.
    view.rerender(<Panel bridge={bridge} state={{ session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" }} />);
    act(() => vi.advanceTimersByTime(3_600_000));
    expect(head()).toBe("Working for 1h 1m");

    act(() => bridge.messages("0", { type: "TurnEnded", outcome: "Completed", timeUnixMs: startedAt + 134_000 }));
    nextFrame();
    view.rerender(<Panel bridge={bridge} state={working("Done")} />);
    expect(head()).toBe("Worked for 2m 14s");
    expect(screen.queryByText(/^Working for/)).toBeNull();
  } finally {
    vi.useRealTimers();
  }
});

test("a turn with no tool call has the work header too, counting from its start and then saying how long it took", async () => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  try {
    const bridge = await started();
    const view = await shownPanel(bridge, { state: working("Working") });
    const heads = () => [...document.querySelectorAll(".work-head")].map((head) => head.textContent);
    const startedAt = Date.now();

    act(() => bridge.messages("0", { type: "User", text: "hi" }, { type: "TurnStarted", timeUnixMs: startedAt }));
    nextFrame();
    expect(heads()).toEqual(["Working for 0s"]);

    act(() => vi.advanceTimersByTime(5_000));
    expect(heads()).toEqual(["Working for 5s"]);

    act(() =>
      bridge.messages(
        "0",
        { type: "TextDelta", text: "Hello." },
        { type: "TurnEnded", outcome: "Completed", timeUnixMs: startedAt + 42_000 },
      ),
    );
    nextFrame();
    view.rerender(<Panel bridge={bridge} state={working("Done")} />);
    expect(heads()).toEqual(["Worked for 42s"]);
  } finally {
    vi.useRealTimers();
  }
});

test("a tool row shows the clock time of its call, and a call with no time shows none", async () => {
  const bridge = await started();
  await shownPanel(bridge, { state: working("Working") });
  const at = new Date(2026, 10, 19, 14, 2, 30).getTime();

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted", timeUnixMs: at },
      { type: "ToolCall", id: "1", tool: "Bash", input: { command: "npm test" }, timeUnixMs: at },
      { type: "ToolCall", id: "2", tool: "Grep", input: { pattern: "TODO" }, timeUnixMs: null },
    ),
  );
  nextFrame();

  const timed = screen.getByRole("button", { name: "Ran npm test" });
  expect(timed.querySelector(".tool-time")?.textContent).toBe(clock(at));
  const untimed = screen.getByRole("button", { name: "Searched for TODO" });
  expect(untimed.querySelector(".tool-time")).toBeNull();
});

test("a tool row's time is its description, so a screen reader hears it after what the call did", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  const at = new Date(2026, 10, 19, 14, 2).getTime();

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted", timeUnixMs: at },
      { type: "ToolCall", id: "1", tool: "Bash", input: { command: "npm test" }, timeUnixMs: at },
      { type: "ToolCall", id: "2", tool: "Grep", input: { pattern: "TODO" }, timeUnixMs: at },
      { type: "ToolResult", id: "2", output: "no such file", isError: true },
    ),
  );
  nextFrame();

  const row = screen.getByRole("button", { name: "Ran npm test" });
  expect(document.getElementById(row.getAttribute("aria-describedby") ?? "")?.textContent).toBe(clock(at));
  // A failed row is not a button, so its time is read as its text.
  expect(document.querySelector(".tool.failed .tool-time")?.getAttribute("aria-hidden")).toBeNull();
});

test("a folded row shows the time of its first call that has one", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  const at = new Date(2026, 10, 19, 14, 2).getTime();

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted", timeUnixMs: at },
      { type: "ToolCall", id: "1", tool: "Read", input: { file_path: `${worktree}/a.ts` }, timeUnixMs: null },
      { type: "ToolCall", id: "2", tool: "Read", input: { file_path: `${worktree}/b.ts` }, timeUnixMs: at },
    ),
  );
  nextFrame();

  const read = screen.getByRole("button", { name: "Read 2 files" });
  expect(read.querySelector(".tool-time")?.textContent).toBe(clock(at));
});

test("a fake message whose time is set to undefined has no time, as one that leaves it out", async () => {
  const bridge = await started();
  await shownPanel(bridge, { state: working("Working") });

  act(() => bridge.messages("0", { type: "TurnStarted", timeUnixMs: undefined }));
  nextFrame();

  expect(document.querySelector(".work-head")?.textContent).toBe("Working");
});

/** A time as the tool rows show it, in the runtime's locale. */
function clock(time: number) {
  return new Date(time).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
}

test.each<[string, AgentStateChanged | undefined, string]>([
  ["no state", undefined, "Send"],
  ["Idle", working("Idle"), "Send"],
  ["Done", working("Done"), "Send"],
  ["Failed", working("Failed"), "Send"],
  ["Approval", { session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" }, "Send"],
  ["Working", working("Working"), "Interrupt"],
])("with %s the composer shows only %s", async (_, state, shown) => {
  const bridge = await started();
  await shownPanel(bridge, { state });

  const buttons = ["Send", "Interrupt"].filter((name) => screen.queryByRole("button", { name }));
  expect(buttons).toEqual([shown]);
});

test("interrupt calls interrupt and shows only while Working", async () => {
  const bridge = await started();
  const view = await shownPanel(bridge, { state: working("Working") });

  fireEvent.click(screen.getByRole("button", { name: "Interrupt" }));
  expect(bridge.interrupted).toEqual(["0"]);

  const approval: AgentStateChanged = { session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" };
  for (const state of [approval, working("Done"), working("Idle"), working("Failed")]) {
    view.rerender(<Panel bridge={bridge} state={state} />);
    expect(screen.queryByRole("button", { name: "Interrupt" })).toBeNull();
  }
  view.rerender(<Panel bridge={bridge} />);
  expect(screen.queryByRole("button", { name: "Interrupt" })).toBeNull();
});

test("the composer does not send while a turn runs", async () => {
  const bridge = await started();
  await shownPanel(bridge, { state: working("Working") });

  fireEvent.change(composer(), { target: { value: "and another thing" } });
  fireEvent.keyDown(composer(), { key: "Enter" });

  expect(bridge.sent).toEqual([]);
});

test("reopening a panel calls subscribe and shows the history without repeats", async () => {
  const bridge = await started();
  bridge.messages(
    "0",
    { type: "User", text: "fix the parser" },
    { type: "TurnStarted" },
    { type: "TextDelta", text: "Fixed it." },
    { type: "TurnEnded", outcome: "Completed" },
  );
  const view = await shownPanel(bridge);
  expect(bridge.subscribed).toEqual(["0"]);
  expect(screen.getAllByText("fix the parser")).toHaveLength(1);

  view.rerender(<Panel bridge={bridge} shown={false} />);
  act(() => bridge.messages("0", { type: "User", text: "now the lexer" }));
  nextFrame();
  view.rerender(<Panel bridge={bridge} shown />);
  await act(async () => {});

  expect(bridge.subscribed).toEqual(["0", "0"]);
  expect(screen.getAllByText("fix the parser")).toHaveLength(1);
  expect(screen.getAllByText("Fixed it.")).toHaveLength(1);
  expect(screen.getAllByText("now the lexer")).toHaveLength(1);
});

function headChange(newPath: string, added: number, removed: number): FakeChange {
  return {
    file: { status: "Modified", oldPath: newPath, newPath, binary: false, added, removed },
    versions: { old: "", new: "", binary: false },
  };
}

// A panel on a worktree whose Head diff is `changes`. Tests may change the list, then call `diffChanged`.
async function withChanges(changes: FakeChange[], props: Partial<Parameters<typeof Panel>[0]> = {}) {
  const diffs: FakeDiffs = { [worktree]: { Head: changes } };
  const bridge = fakeBridge({ [worktree]: [{ path: worktree, branch: "feat-116", isMain: true }] }, diffs);
  await bridge.addRepo(worktree);
  await bridge.spawnAgent(worktree, "Claude", null);
  const view = await shownPanel(bridge, props);
  return { bridge, diffs, view };
}

const aTurn: FakeMessage[] = [
  { type: "User", text: "fix it" },
  { type: "TurnStarted" },
  { type: "TextDelta", text: "Fixed." },
  { type: "TurnEnded", outcome: "Completed" },
];

function changedCard() {
  return screen.queryByRole("group", { name: /changed files?$/ });
}

test("a completed turn shows the worktree's changed files with their counts and the totals", async () => {
  const { bridge } = await withChanges([headChange("src/a.ts", 250, 10), headChange("src/b.ts", 12, 5)]);

  act(() => bridge.messages("0", ...aTurn));
  nextFrame();
  await act(async () => {});

  const card = changedCard()!;
  expect(card.getAttribute("aria-label")).toBe("2 changed files");
  expect(card.querySelector(".files-head")!.textContent).toBe("2 changed files+262−15Review diff");
  expect(within(card).getAllByRole("listitem").map((item) => item.textContent)).toEqual([
    "src/a.ts+250−10",
    "src/b.ts+12−5",
  ]);
  const text = screen.getByText("Fixed.");
  expect(text.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

test("Review diff opens the diff, and a file opens the diff on that file", async () => {
  const onOpenDiff = vi.fn();
  const { bridge } = await withChanges([headChange("src/a.ts", 1, 0), headChange("src/b.ts", 2, 1)], { onOpenDiff });
  act(() => bridge.messages("0", ...aTurn));
  nextFrame();
  await act(async () => {});

  fireEvent.click(within(changedCard()!).getByRole("button", { name: "Review diff" }));
  fireEvent.click(within(changedCard()!).getByRole("button", { name: /^src\/b\.ts/ }));

  expect(onOpenDiff.mock.calls).toEqual([[], ["src/b.ts"]]);
});

test("the card follows the worktree's diff, and goes when the diff is empty", async () => {
  const { bridge, diffs } = await withChanges([headChange("src/a.ts", 1, 0)]);
  act(() => bridge.messages("0", ...aTurn));
  nextFrame();
  await act(async () => {});
  expect(changedCard()!.getAttribute("aria-label")).toBe("1 changed file");

  diffs[worktree].Head!.push(headChange("src/c.ts", 4, 4));
  await act(async () => bridge.diffChanged(worktree));
  expect(changedCard()!.getAttribute("aria-label")).toBe("2 changed files");

  diffs[worktree].Head = [];
  await act(async () => bridge.diffChanged(worktree));
  expect(changedCard()).toBeNull();
});

test("no card while a turn runs, and after a second turn only the second has one", async () => {
  const { bridge, view } = await withChanges([headChange("src/a.ts", 1, 0)]);
  act(() => bridge.messages("0", ...aTurn));
  nextFrame();
  await act(async () => {});
  expect(changedCard()).toBeTruthy();

  view.rerender(<Panel bridge={bridge} state={working("Working")} />);
  act(() => bridge.messages("0", { type: "User", text: "and the docs" }, { type: "TurnStarted" }));
  nextFrame();
  expect(changedCard()).toBeNull();

  act(() => bridge.messages("0", { type: "TextDelta", text: "Docs done." }, { type: "TurnEnded", outcome: "Completed" }));
  nextFrame();
  view.rerender(<Panel bridge={bridge} state={working("Done")} />);
  expect(screen.getAllByRole("group", { name: "1 changed file" })).toHaveLength(1);
  const second = screen.getByText("Docs done.");
  expect(second.compareDocumentPosition(changedCard()!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

test("a conversation with no ended turn shows no card", async () => {
  const { bridge } = await withChanges([headChange("src/a.ts", 1, 0)]);
  act(() => bridge.messages("0", { type: "User", text: "fix it" }, { type: "TurnStarted" }));
  nextFrame();
  await act(async () => {});

  expect(changedCard()).toBeNull();
});

test("a review batch shows as a card with each comment's place and what it says", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  act(() =>
    bridge.messages("0", {
      type: "ReviewBatch",
      comments: [
        { path: "src/a.rs", startLine: 10, endLine: 11, text: "line 10\nline 11", body: "split this" },
        { path: "src/b.rs", startLine: 3, endLine: 3, text: "let x = 1;", body: "name it" },
      ],
    }),
  );
  nextFrame();

  const card = screen.getByRole("group", { name: "Review batch" });
  expect(within(card).getByText("2 comments sent")).toBeTruthy();
  expect(within(card).getAllByRole("listitem").map((item) => item.textContent)).toEqual([
    "src/a.rs:10-11split this",
    "src/b.rs:3name it",
  ]);
});

test("the agent is fixed once a review batch started the conversation", async () => {
  const bridge = await started();
  bridge.messages("0", {
    type: "ReviewBatch",
    comments: [{ path: "a.rs", startLine: 1, endLine: 1, text: "x", body: "y" }],
  });
  await shownPanel(bridge, { onChangeAgent: async () => {} });

  expect(option(openPicker(), /^Codex/).disabled).toBe(true);
});

test("agent text is shown as markdown, with raw HTML as text and its line breaks kept", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  act(() =>
    bridge.messages("0", { type: "TextDelta", text: "Run `npm test` then <b>ship</b>.\nDone." }),
  );
  nextFrame();

  expect(screen.getByText("npm test").tagName).toBe("CODE");
  // A single newline stays in the paragraph, and the stylesheet shows it as a line break.
  expect(screen.getByText("npm test").parentElement!.textContent).toBe("Run npm test then <b>ship</b>.\nDone.");
});

test("an interrupted or failed turn says so", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnEnded", outcome: "Interrupted" },
      { type: "TurnEnded", outcome: "Failed", kind: "overloaded", message: "API Error: 529 overloaded" },
    ),
  );
  nextFrame();

  expect(screen.getByText("You interrupted the turn.")).toBeTruthy();
  expect(screen.getByText("API Error: 529 overloaded")).toBeTruthy();
});

test("a message the agent never got says so, with its text", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  act(() =>
    bridge.messages("0", { type: "User", text: "add a test" }, { type: "NotDelivered", text: "add a test" }),
  );
  nextFrame();

  const card = screen.getByRole("group", { name: "Not delivered" });
  expect(card.textContent).toContain("Claude Code ended before it started on this message.");
  expect(card.textContent).toContain("add a test");
});

test("the header shows the agent and its state with the source, and the strip shows Chat mode", async () => {
  const bridge = await started();
  await shownPanel(bridge, { state: working("Working") });

  expect(document.querySelector(".pane-head b")!.textContent).toBe("Claude Code");
  expect(document.querySelector(".pane-head .ui-agent-state")!.textContent).toBe("Working · Protocol");
  expect(within(document.querySelector<HTMLElement>(".pane-head")!).queryByRole("group", { name: "Mode" })).toBeNull();
  const mode = within(strip()).getByRole("group", { name: "Mode" });
  expect(within(mode).getByRole("button", { name: "Chat" }).getAttribute("aria-pressed")).toBe("true");
  expect((within(mode).getByRole("button", { name: "Terminal" }) as HTMLButtonElement).disabled).toBe(true);
});

test("the strip under the composer shows the worktree's branch and the repo's profile", async () => {
  const bridge = await started();
  await bridge.setRepoProfile(worktree, "Rust crate");
  await shownPanel(bridge);

  expect(strip().textContent).toContain("feat-116");
  expect(strip().textContent).toContain("Rust crate");

  await act(() => bridge.setRepoProfile(worktree, "Next.js app"));
  expect(strip().textContent).toContain("Next.js app");
  await act(() => bridge.setRepoProfile(worktree, null));
  expect(strip().textContent).not.toMatch(/Rust crate|Next\.js app/);
});

test("the composer's footer shows the agent's mark, then the model and its effort, then send", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  const row = document.querySelector(".composer-row")!;
  expect(within(row as HTMLElement).getByTitle("Claude Code")).toBeTruthy();
  expect(pickerButton().querySelector("b")!.textContent).toBe("Default (recommended)");
  expect(pickerButton().querySelector(".picker-effort")!.textContent).toBe("high");
  const buttons = within(row as HTMLElement).getAllByRole("button");
  expect(buttons.map((b) => b.getAttribute("aria-label"))).toEqual(["Agent and model", "Send"]);
});

test("the composer starts on the placeholder for its agent", async () => {
  const bridge = await started("Codex");
  await shownPanel(bridge, { agent: "Codex" });

  expect(composer().getAttribute("placeholder")).toBe("Ask Codex anything. @ files, / commands");
  expect(within(document.querySelector<HTMLElement>(".composer-row")!).getByTitle("Codex")).toBeTruthy();
});

// The account line moved to the sidebar's footer (#29).
test("the composer has no account line", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  expect(document.querySelector(".composer-foot")).toBeNull();
  expect(pickerButton().textContent).toContain("Default (recommended) high");
});

test("the pane head names the session as the sidebar does", async () => {
  const bridge = await started();
  await act(async () => {
    render(
      <ChatPanel
        bridge={bridge}
        session="0"
        agent="Claude"
        sessionName="Claude Code 2"
        worktree={worktree}
        shown
        state={undefined}
        focusRequest={0}
        onOpenDiff={() => {}}
      />,
    );
  });

  expect(document.querySelector(".pane-head b")!.textContent).toBe("Claude Code 2");
  expect(document.querySelector(".pane-head .ui-agent-state")).toBeNull();
});

function openPicker() {
  fireEvent.click(screen.getByRole("button", { name: "Agent and model" }));
  return screen.getByRole("dialog", { name: "Agent and model" });
}

function option(picker: HTMLElement, name: string | RegExp) {
  return within(picker).getByRole("button", { name }) as HTMLButtonElement;
}

function pickerButton() {
  return screen.getByRole("button", { name: "Agent and model" });
}

test("the agent can change until the first message, and is fixed from then on", async () => {
  const bridge = await started();
  const changed: Agent[] = [];
  await shownPanel(bridge, {
    onChangeAgent: async (agent) => {
      changed.push(agent);
    },
  });

  const codex = option(openPicker(), /^Codex/);
  await act(async () => fireEvent.click(codex));
  expect(changed).toEqual(["Codex"]);
  expect(screen.queryByRole("dialog")).toBeNull();

  fireEvent.change(composer(), { target: { value: "add a test" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  const picker = openPicker();

  expect(option(picker, /^Codex/).disabled).toBe(true);
  expect(option(picker, /^Claude Code/).disabled).toBe(false);
  expect(within(picker).getByText(/Fixed after the first message/)).toBeTruthy();
  fireEvent.click(option(picker, /^Codex/));
  expect(changed).toEqual(["Codex"]);
});

// A send to the old session would be lost when the switch ends it.
test("nothing sends while the agent is switching", async () => {
  const bridge = await started();
  let switched = () => {};
  await shownPanel(bridge, {
    onChangeAgent: () => new Promise<void>((resolve) => (switched = resolve)),
  });
  fireEvent.change(composer(), { target: { value: "add a test" } });

  fireEvent.click(option(openPicker(), /^Codex/));
  fireEvent.keyDown(composer(), { key: "Enter" });

  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  expect((composer() as HTMLTextAreaElement).value).toBe("add a test");
  await act(async () => switched());
  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(false);
});

test("the agent is fixed in a panel whose conversation already has a message", async () => {
  const bridge = await started();
  bridge.messages("0", { type: "User", text: "earlier" });
  await shownPanel(bridge, { onChangeAgent: async () => {} });

  expect(option(openPicker(), /^Codex/).disabled).toBe(true);
});

test("a new panel starts on the agent's default model and effort, and the picker shows each change", async () => {
  const bridge = await started("Codex");
  await shownPanel(bridge, { agent: "Codex" });

  expect(document.querySelector(".pane-head b")!.textContent).toBe("Codex");
  expect(bridge.modelsSet).toEqual([{ session: "0", model: "gpt-5.5", effort: "medium" }]);
  expect(pickerButton().textContent).toContain("GPT-5.5 medium");

  // A model that takes the effort keeps it.
  fireEvent.click(option(openPicker(), /^GPT-5.5 mini/));
  expect(pickerButton().textContent).toContain("GPT-5.5 mini medium");
  const efforts = within(openPicker()).getByRole("group", { name: "Effort" });
  expect(within(efforts).getAllByRole("button").map((b) => b.textContent)).toEqual(["low", "medium"]);
  fireEvent.click(within(efforts).getByRole("button", { name: "low" }));

  expect(pickerButton().textContent).toContain("GPT-5.5 mini low");
  expect(bridge.modelsSet.slice(1)).toEqual([
    { session: "0", model: "gpt-5.5-mini", effort: "medium" },
    { session: "0", model: "gpt-5.5-mini", effort: "low" },
  ]);
});

// A mode switch and a restart both remount the panel.
test("a remounted panel shows the session's own choice and sends no default", async () => {
  const bridge = await started("Codex");
  const first = await shownPanel(bridge, { agent: "Codex" });
  fireEvent.click(option(openPicker(), /^GPT-5.5 mini/));
  const efforts = within(openPicker()).getByRole("group", { name: "Effort" });
  fireEvent.click(within(efforts).getByRole("button", { name: "low" }));
  first.unmount();
  const sent = bridge.modelsSet.length;

  await shownPanel(bridge, { agent: "Codex" });

  expect(pickerButton().textContent).toContain("GPT-5.5 mini low");
  expect(bridge.modelsSet).toHaveLength(sent);
});

test("a model that does not take the current effort gets its own default, or none", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  fireEvent.click(option(openPicker(), /^Haiku 5.5/));

  expect(pickerButton().textContent).toBe("Haiku 5.5");
  expect(bridge.modelsSet.at(-1)).toEqual({ session: "0", model: "haiku", effort: null });
});

test("the model can change after the first message, mid-turn too", async () => {
  const bridge = await started();
  bridge.messages("0", { type: "User", text: "go" }, { type: "TurnStarted" });
  await shownPanel(bridge, { state: working("Working") });

  fireEvent.click(option(openPicker(), /^Haiku 5.5/));

  expect(bridge.modelsSet.at(-1)).toEqual({ session: "0", model: "haiku", effort: null });
});

test("nothing can be sent until the agent has listed its models", async () => {
  const bridge = await started();
  vi.spyOn(bridge, "models").mockReturnValue(new Promise(() => {}));
  await shownPanel(bridge);

  fireEvent.change(composer(), { target: { value: "add a test" } });

  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  expect(pickerButton().textContent).toContain("Loading models");
  expect(bridge.modelsSet).toEqual([]);
});

test("when the agent lists no models, the panel says so and sends on the agent's own default", async () => {
  const bridge = await started();
  vi.spyOn(bridge, "models").mockRejectedValue("session 0 is not running");
  await shownPanel(bridge);

  fireEvent.change(composer(), { target: { value: "add a test" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));

  expect(screen.getByRole("alert").textContent).toBe("session 0 is not running");
  expect(pickerButton().textContent).toContain("Agent's default model");
  expect(bridge.sent).toEqual([{ session: "0", text: "add a test" }]);
});

const runner: Question = {
  question: "Which test runner?",
  header: "Runner",
  multiSelect: false,
  options: [
    { label: "vitest", description: "Fast, Vite native" },
    { label: "jest", description: "What the repo used before" },
  ],
};

const kinds: Question = {
  question: "Which kinds of test?",
  header: "Kinds",
  multiSelect: true,
  options: [
    { label: "unit", description: "One module at a time" },
    { label: "integration", description: "Through the core" },
    { label: "e2e", description: "The whole app" },
  ],
};

// A panel showing a turn that waits on its last message, a request.
async function asked(...messages: FakeMessage[]) {
  const bridge = await started();
  await shownPanel(bridge, { state: { session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" } });
  act(() => bridge.messages("0", { type: "TurnStarted" }, ...messages));
  nextFrame();
  return bridge;
}

// As Claude Code sends it: the tool call, then the request to run it, under the call's id.
const bashCall: FakeMessage = { type: "ToolCall", id: "toolu_01", tool: "Bash", input: { command: "npm test" } };
const approval: FakeMessage = {
  type: "ApprovalRequested",
  request: "toolu_01",
  tool: "Bash",
  input: { command: "npm test", description: "Run the tests" },
};

// Lets the answer settle and the core's echo of it draw.
async function settle() {
  await act(async () => {});
  nextFrame();
}

function approvalLine() {
  return screen.getByRole("group", { name: "Claude Code wants to use Bash" });
}

test("an approval shows under its tool row, with no card and no second row", async () => {
  await asked(bashCall, approval);

  expect(screen.getAllByRole("button", { name: "Ran npm test" })).toHaveLength(1);
  const line = approvalLine();
  expect(line.classList.contains("card")).toBe(false);
  expect(within(line).getByText("Waiting for approval")).toBeTruthy();
  expect(within(line).getAllByRole("button").map((b) => b.getAttribute("aria-label") ?? b.textContent)).toEqual([
    "Decline",
    "Approve",
    "More approval options",
  ]);
});

test("an approval with no tool call of its own shows its own row", async () => {
  await asked({ ...approval, request: "item_7", tool: "commandExecution", input: { command: "ls -la" } });

  expect(screen.getByRole("button", { name: "Ran ls -la" })).toBeTruthy();
  expect(screen.getByRole("group", { name: "Claude Code wants to use commandExecution" })).toBeTruthy();
});

test.each<[string, Reply, string]>([
  ["Approve", { type: "AllowOnce" }, "Approved"],
  ["Decline", { type: "Deny", message: null }, "Declined"],
])("an approval's %s button sends its reply and the line becomes a record", async (button, reply, record) => {
  const bridge = await asked(bashCall, approval);

  fireEvent.click(within(approvalLine()).getByRole("button", { name: button }));
  await settle();

  expect(bridge.answered).toEqual([{ session: "0", request: "toolu_01", reply }]);
  expect(screen.queryByRole("group", { name: "Claude Code wants to use Bash" })).toBeNull();
  expect(screen.getByText(record)).toBeTruthy();
});

test("two approvals from one run each answer by button, one after the other", async () => {
  const lint: FakeMessage = { type: "ToolCall", id: "toolu_02", tool: "Bash", input: { command: "npm run lint" } };
  const bridge = await asked(bashCall, lint, approval);
  fireEvent.click(within(approvalLine()).getByRole("button", { name: "Approve" }));
  await settle();

  act(() =>
    bridge.messages(
      "0",
      { type: "ToolResult", id: "toolu_01", output: "ok", isError: false },
      { type: "ApprovalRequested", request: "toolu_02", tool: "Bash", input: { command: "npm run lint" } },
    ),
  );
  nextFrame();
  const approve = within(approvalLine()).getByRole("button", { name: "Approve" }) as HTMLButtonElement;
  expect(approve.disabled).toBe(false);
  fireEvent.click(approve);
  await settle();

  expect(bridge.answered.map((a) => a.request)).toEqual(["toolu_01", "toolu_02"]);
});

test("a declined call shows as declined, not as a failed call, with the message once", async () => {
  const bridge = await asked(bashCall, approval);
  fireEvent.change(composer(), { target: { value: "use vitest instead" } });
  fireEvent.keyDown(composer(), { key: "Enter" });
  await settle();

  // The agent reports the declined call as a failed result carrying the message.
  act(() => bridge.messages("0", { type: "ToolResult", id: "toolu_01", output: "use vitest instead", isError: true }));
  nextFrame();

  expect(document.querySelector(".tool.failed")).toBeNull();
  expect(screen.queryByRole("button", { name: "Ran npm test" })).toBeNull();
  expect(document.querySelector(".tool.declined")!.textContent).toBe("npm test");
  expect(screen.getAllByText(/use vitest instead/).map((el) => el.textContent)).toEqual([
    "Declined: use vitest instead",
  ]);
});

test("a declined call keeps its call's clock time", async () => {
  const at = new Date(2026, 10, 19, 9, 5).getTime();
  const bridge = await asked({ ...bashCall, timeUnixMs: at }, approval);
  fireEvent.change(composer(), { target: { value: "no" } });
  fireEvent.keyDown(composer(), { key: "Enter" });
  await settle();

  act(() => bridge.messages("0", { type: "ToolResult", id: "toolu_01", output: "no", isError: true }));
  nextFrame();

  expect(document.querySelector(".tool.declined .tool-time")?.textContent).toBe(clock(at));
});

test("the approval's menu allows the tool for the rest of the session", async () => {
  const bridge = await asked(bashCall, approval);

  fireEvent.click(within(approvalLine()).getByRole("button", { name: "More approval options" }));
  fireEvent.click(screen.getByRole("menuitem", { name: "Always allow this session" }));
  await settle();

  expect(bridge.answered).toEqual([{ session: "0", request: "toolu_01", reply: { type: "AllowForSession" } }]);
  expect(screen.getByText("Approved for this session")).toBeTruthy();
});

test("an approval's buttons send once, even when pressed again before the answer is back", async () => {
  const bridge = await asked(bashCall, approval);

  const approve = screen.getByRole("button", { name: "Approve" });
  fireEvent.click(approve);
  fireEvent.click(approve);
  fireEvent.click(screen.getByRole("button", { name: "Decline" }));
  await settle();

  expect(bridge.answered).toHaveLength(1);
});

test("while an approval waits, a message from the composer declines it with that message", async () => {
  const bridge = await asked(bashCall, approval);

  expect(composer().getAttribute("placeholder")).toBe("Send a message to decline and redirect the agent");
  fireEvent.change(composer(), { target: { value: "use vitest instead" } });
  fireEvent.keyDown(composer(), { key: "Enter" });
  await settle();

  expect(bridge.answered).toEqual([
    { session: "0", request: "toolu_01", reply: { type: "Deny", message: "use vitest instead" } },
  ]);
  expect(bridge.sent).toEqual([]);
  expect((composer() as HTMLTextAreaElement).value).toBe("");
  expect(screen.getByText("Declined: use vitest instead")).toBeTruthy();
  expect(composer().getAttribute("placeholder")).toBe("Ask Claude Code anything. @ files, / commands");
});

test("a decline the core refuses puts the message back in the composer with its error", async () => {
  const bridge = await asked(bashCall, approval);
  vi.spyOn(bridge, "answer").mockRejectedValue("agent session 0 has no pending request toolu_01");

  fireEvent.change(composer(), { target: { value: "use vitest instead" } });
  fireEvent.click(screen.getByRole("button", { name: "Send" }));
  await settle();

  expect((composer() as HTMLTextAreaElement).value).toBe("use vitest instead");
  expect(screen.getByRole("alert").textContent).toBe("agent session 0 has no pending request toolu_01");
});

function questionCard() {
  return screen.getByRole("group", { name: "Claude Code is asking" });
}

test("a question shows as a card above the composer and sends the chosen option", async () => {
  const bridge = await asked(
    { type: "ToolCall", id: "toolu_02", tool: "AskUserQuestion", input: { questions: [runner] } },
    { type: "QuestionAsked", request: "toolu_02", questions: [runner] },
  );

  const card = questionCard();
  expect(card.closest(".composer-wrap")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Used AskUserQuestion" })).toBeNull();
  expect(within(card).getByText("Which test runner?")).toBeTruthy();
  expect(within(card).getByText("Fast, Vite native")).toBeTruthy();
  const submit = within(card).getByRole("button", { name: "Submit answer" }) as HTMLButtonElement;
  expect(submit.disabled).toBe(true);

  fireEvent.click(within(card).getByRole("radio", { name: /jest/ }));
  fireEvent.click(within(card).getByRole("radio", { name: /vitest/ }));
  fireEvent.click(submit);
  await settle();

  expect(bridge.answered).toEqual([
    { session: "0", request: "toolu_02", reply: { type: "Answers", answers: [["vitest"]] } },
  ]);
  expect(screen.queryByRole("group", { name: "Claude Code is asking" })).toBeNull();
  expect(screen.getByText("Answered: vitest")).toBeTruthy();
});

test("a request of several questions shows one at a time, then sends every answer", async () => {
  const bridge = await asked({ type: "QuestionAsked", request: "toolu_02", questions: [runner, kinds] });

  const card = questionCard();
  expect(within(card).queryByText("Which kinds of test?")).toBeNull();
  const next = within(card).getByRole("button", { name: "Next question" }) as HTMLButtonElement;
  expect(next.disabled).toBe(true);
  fireEvent.click(within(card).getByRole("radio", { name: /vitest/ }));
  fireEvent.click(next);

  expect(within(card).queryByText("Which test runner?")).toBeNull();
  fireEvent.click(within(card).getByRole("checkbox", { name: /unit/ }));
  fireEvent.click(within(card).getByRole("checkbox", { name: /e2e/ }));
  fireEvent.click(within(card).getByRole("checkbox", { name: /integration/ }));
  fireEvent.click(within(card).getByRole("checkbox", { name: /e2e/ }));
  fireEvent.click(within(card).getByRole("button", { name: "Submit answer" }));
  await settle();

  expect(bridge.answered[0].reply).toEqual({ type: "Answers", answers: [["vitest"], ["unit", "integration"]] });
  expect(screen.getByText("Answered: Runner: vitest; Kinds: unit, integration")).toBeTruthy();
});

test("a free-text answer sends the text in place of any option", async () => {
  const bridge = await asked({ type: "QuestionAsked", request: "toolu_02", questions: [runner] });

  const card = questionCard();
  fireEvent.click(within(card).getByRole("radio", { name: /jest/ }));
  fireEvent.change(within(card).getByRole("textbox", { name: "Your own answer to Runner" }), {
    target: { value: "node:test" },
  });
  expect((within(card).getByRole("radio", { name: /jest/ }) as HTMLInputElement).checked).toBe(false);
  fireEvent.click(within(card).getByRole("button", { name: "Submit answer" }));
  await settle();

  expect(bridge.answered[0].reply).toEqual({ type: "Answers", answers: [["node:test"]] });
  expect(screen.getByText("Answered: node:test")).toBeTruthy();
});

test("a request whose turn ended shows as expired and sends nothing", async () => {
  const bridge = await asked(bashCall, approval, { type: "QuestionAsked", request: "toolu_02", questions: [runner] });

  act(() => bridge.messages("0", { type: "TurnEnded", outcome: "Interrupted" }));
  nextFrame();

  expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  expect(screen.queryByRole("group", { name: "Claude Code is asking" })).toBeNull();
  expect(screen.getByText("Expired")).toBeTruthy();
  expect(screen.getByText("Expired: Which test runner?")).toBeTruthy();
  expect(bridge.answered).toEqual([]);
});

test("a question the core no longer has pending shows as expired when answered", async () => {
  const bridge = await asked({ type: "QuestionAsked", request: "toolu_02", questions: [runner] });
  // The turn ended in the core, and the panel has not drawn that yet.
  act(() => bridge.messages("0", { type: "TurnEnded", outcome: "Interrupted" }));

  fireEvent.click(screen.getByRole("radio", { name: /vitest/ }));
  fireEvent.click(screen.getByRole("button", { name: "Submit answer" }));
  await act(async () => {});

  expect(bridge.answered).toEqual([]);
  expect(screen.queryByRole("button", { name: "Submit answer" })).toBeNull();
  expect(screen.getByText("Expired: Which test runner?")).toBeTruthy();
});

test("a new approval scrolls into view, and its controls take keyboard focus in order", async () => {
  const scrolled: Element[] = [];
  Element.prototype.scrollIntoView = function (this: Element) {
    scrolled.push(this);
  };
  await asked(bashCall, approval);

  expect(scrolled).toContain(approvalLine());
  const buttons = within(approvalLine()).getAllByRole("button");
  expect(buttons.map((b) => b.getAttribute("aria-label") ?? b.textContent)).toEqual([
    "Decline",
    "Approve",
    "More approval options",
  ]);
  buttons[0].focus();
  expect(document.activeElement).toBe(buttons[0]);
  expect(buttons.every((b) => b.tabIndex === 0)).toBe(true);
});

test("a request answered before the panel opened shows as its record", async () => {
  const bridge = await started();
  bridge.messages(
    "0",
    { type: "TurnStarted" },
    bashCall,
    approval,
    { type: "Answered", request: "toolu_01", reply: { type: "AllowOnce" } },
    { type: "TurnEnded", outcome: "Completed" },
  );
  await shownPanel(bridge);

  expect(screen.getByText("Approved")).toBeTruthy();
  expect(screen.queryByText(/Expired/)).toBeNull();
});

test("a focus request puts focus in the composer once the panel is shown, and only once", async () => {
  const bridge = await started();
  const view = await shownPanel(bridge, { shown: false });

  view.rerender(<Panel bridge={bridge} shown={false} focusRequest={1} />);
  expect(document.activeElement).toBe(document.body);
  view.rerender(<Panel bridge={bridge} shown focusRequest={1} />);
  expect(document.activeElement).toBe(composer());

  // Shown again later with no new request, it leaves focus where the user put it.
  act(() => (document.activeElement as HTMLElement).blur());
  view.rerender(<Panel bridge={bridge} shown={false} focusRequest={1} />);
  view.rerender(<Panel bridge={bridge} shown focusRequest={1} />);
  expect(document.activeElement).toBe(document.body);

  view.rerender(<Panel bridge={bridge} shown focusRequest={2} />);
  expect(document.activeElement).toBe(composer());
});

function modeButton(name: "Chat" | "Terminal") {
  const mode = within(strip()).getByRole("group", { name: "Mode" });
  return within(mode).getByRole("button", { name }) as HTMLButtonElement;
}

async function switchTo(name: "Chat" | "Terminal") {
  await act(async () => fireEvent.click(modeButton(name)));
}

function terminal() {
  return screen.queryByRole("region", { name: "Terminal of session 0" });
}

test.each<[Agent, string]>([
  ["Claude", "Claude Code"],
  ["Codex", "Codex"],
])("the Terminal button resumes a %s session in its own TUI, in place of the chat", async (agent, tui) => {
  const bridge = await started(agent);
  await shownPanel(bridge, { agent });

  await switchTo("Terminal");

  expect(bridge.modesSet).toEqual([{ session: "0", mode: "Terminal" }]);
  expect(terminal()).toBeTruthy();
  expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
  expect(modeButton("Terminal").getAttribute("aria-pressed")).toBe("true");
  expect(modeButton("Chat").getAttribute("aria-pressed")).toBe("false");
  expect(
    screen.getByText(`The same conversation, resumed in ${tui}'s own TUI. State now comes from hooks.`),
  ).toBeTruthy();
});

test("back in chat mode the panel shows the whole conversation, then says it resumed, then what comes next", async () => {
  const bridge = await started();
  bridge.messages("0", { type: "User", text: "add a test" }, { type: "TextDelta", text: "Added." });
  await shownPanel(bridge);

  await switchTo("Terminal");
  await switchTo("Chat");
  act(() => bridge.messages("0", { type: "User", text: "now the docs" }));
  nextFrame();

  expect(bridge.modesSet.map(({ mode }) => mode)).toEqual(["Terminal", "Chat"]);
  expect(terminal()).toBeNull();
  expect([...document.querySelector(".thread")!.children].map((item) => item.textContent)).toEqual([
    "add a test",
    "Added.",
    "Back in chat mode. The same conversation resumed from Claude Code's TUI, and state comes from the protocol again.",
    "now the docs",
  ]);
});

test.each<[string, AgentStateChanged, string]>([
  ["Working", working("Working"), "Claude Code is working. Switch modes once the turn ends."],
  [
    "NeedsInput",
    { session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" },
    "Claude Code is waiting on you. Answer it, then switch modes.",
  ],
])("the switch is disabled while %s, with the reason as its tooltip", async (_, state, reason) => {
  const bridge = await started();
  await shownPanel(bridge, { state });

  fireEvent.click(modeButton("Terminal"));

  for (const name of ["Chat", "Terminal"] as const) {
    expect(modeButton(name).disabled).toBe(true);
    expect(modeButton(name).title).toBe(reason);
  }
  expect(bridge.modesSet).toEqual([]);
});

// The core refuses chat calls while it switches, and the old process is on its way out.
test("while a switch runs nothing sends, and the switch says where it is going", async () => {
  const bridge = await started();
  let finish = () => {};
  const slow: FakeBridge = {
    ...bridge,
    setMode: (session, mode) =>
      new Promise<void>((resolve) => (finish = () => void bridge.setMode(session, mode).then(resolve))),
  };
  await shownPanel(slow);
  fireEvent.change(composer(), { target: { value: "add a test" } });

  fireEvent.click(modeButton("Terminal"));

  expect((screen.getByRole("button", { name: "Send" }) as HTMLButtonElement).disabled).toBe(true);
  expect(modeButton("Chat").disabled).toBe(true);
  expect(modeButton("Terminal").title).toBe("Switching to terminal mode");
  await act(async () => finish());
  expect(terminal()).toBeTruthy();
});

test("a refused switch says why and stays in the same mode", async () => {
  const bridge = await started();
  // The panel has not heard of the turn yet, but the core has.
  bridge.agentStateChanged(working("Working"));
  await shownPanel(bridge);

  await switchTo("Terminal");

  expect(screen.getByRole("alert").textContent).toBe("agent session 0 cannot switch mode while Working");
  expect(terminal()).toBeNull();
  expect(modeButton("Chat").getAttribute("aria-pressed")).toBe("true");
});

// Such as a chat session that failed to log in, switched to terminal mode to run /login.
test("an ended session offers the switch, which starts it again in the other mode", async () => {
  const bridge = await started();
  await bridge.kill("0");
  await shownPanel(bridge, { state: { session: "0", state: "Failed", source: "Protocol" } });

  expect(modeButton("Terminal").disabled).toBe(false);
  await switchTo("Terminal");

  expect(terminal()).toBeTruthy();
  await expect(bridge.write("0", new Uint8Array([13]))).resolves.toBeUndefined();
});

test("a switch keeps the draft", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  fireEvent.change(composer(), { target: { value: "half a thought" } });

  await switchTo("Terminal");
  await switchTo("Chat");

  expect((composer() as HTMLTextAreaElement).value).toBe("half a thought");
});

// As after a restart or a window reload: the panel learns its session's mode from the core.
test("a panel whose session runs in terminal mode opens in the agent's TUI and never subscribes", async () => {
  const bridge = await started();
  await bridge.setMode("0", "Terminal");

  await shownPanel(bridge);

  expect(modeButton("Terminal").getAttribute("aria-pressed")).toBe("true");
  expect(terminal()).toBeTruthy();
  expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
  expect(bridge.subscribed).toEqual([]);
});

// The switch stays off while the agent waits, so only the TUI can answer it.
test("a terminal-mode session waiting on the user opens in its TUI, where it can be answered", async () => {
  const bridge = await started();
  await bridge.setMode("0", "Terminal");
  const waiting: AgentStateChanged = { session: "0", state: "NeedsInput", kind: "Approval", source: "Hook" };
  bridge.agentStateChanged(waiting);

  await shownPanel(bridge, { state: waiting });

  expect(terminal()!.hidden).toBe(false);
  expect(modeButton("Terminal").getAttribute("aria-pressed")).toBe("true");
  expect(modeButton("Chat").disabled).toBe(true);
});

test("a panel that cannot learn its session's mode says why and shows neither view", async () => {
  const bridge = await started();
  vi.spyOn(bridge, "sessions").mockRejectedValueOnce("grove is quitting");

  await shownPanel(bridge);

  expect(screen.getByRole("alert").textContent).toBe("grove is quitting");
  expect(terminal()).toBeNull();
  expect(screen.queryByRole("textbox", { name: "Message" })).toBeNull();
  expect(modeButton("Chat").disabled).toBe(true);
});

// For the Details card (#243).
test("the panel reports its session's mode and model choice as they change", async () => {
  const bridge = await started();
  const onMode = vi.fn();
  const onModelChoice = vi.fn();
  render(
    <ChatPanel
      bridge={bridge}
      session="0"
      agent="Claude"
      worktree={worktree}
      shown
      state={undefined}
      focusRequest={0}
      onOpenDiff={() => {}}
      onMode={onMode}
      onModelChoice={onModelChoice}
    />,
  );
  await act(async () => {});

  expect(onMode).toHaveBeenLastCalledWith("Chat");
  expect(onModelChoice).toHaveBeenLastCalledWith({ model: expect.objectContaining({ id: "default" }), effort: "high" });

  fireEvent.click(option(openPicker(), /^Haiku 5.5/));
  expect(onModelChoice).toHaveBeenLastCalledWith({ model: expect.objectContaining({ id: "haiku" }), effort: null });

  await switchTo("Terminal");
  expect(onMode).toHaveBeenLastCalledWith("Terminal");
});

test("a panel that takes another session reports that session's mode", async () => {
  const bridge = await started();
  await bridge.spawnAgent(worktree, "Claude", null);
  const props = {
    bridge,
    agent: "Claude" as const,
    worktree,
    shown: true,
    state: undefined,
    focusRequest: 0,
    onOpenDiff: () => {},
  };
  const view = render(<ChatPanel {...props} session="0" onMode={vi.fn()} />);
  await act(async () => {});
  const onMode = vi.fn();

  view.rerender(<ChatPanel {...props} session="1" onMode={onMode} />);
  await act(async () => {});

  expect(onMode).toHaveBeenLastCalledWith("Chat");
});

const MARKDOWN_REPLY = [
  "## Fix\n\nThe parser **drops** the last token. Steps:\n\n",
  "- add a failing test\n- fix `next_token`\n\n",
  "```rust\nfn next_token(&mut self) -> Option<Token> {\n    self.tokens.pop()\n}\n```\n\n",
  "| Gate | Result |\n| --- | --- |\n| clippy | pass |\n\nSee [the docs](https://docs.rs/logos).",
].join("");

async function threadAfterStreaming(deltas: string[]): Promise<string> {
  const bridge = await started();
  const { container } = await shownPanel(bridge);
  act(() => bridge.messages("0", { type: "TurnStarted" }));
  for (const text of deltas) {
    act(() => bridge.messages("0", { type: "TextDelta", text }));
    nextFrame();
  }
  const html = container.querySelector(".thread")!.innerHTML;
  cleanup();
  return html;
}

test("a markdown reply streamed in ten deltas ends with the same DOM as the reply in one delta", async () => {
  const size = Math.ceil(MARKDOWN_REPLY.length / 10);
  const tenDeltas = Array.from({ length: 10 }, (_, i) => MARKDOWN_REPLY.slice(i * size, (i + 1) * size));
  expect(tenDeltas.join("")).toBe(MARKDOWN_REPLY);

  const streamed = await threadAfterStreaming(tenDeltas);
  const whole = await threadAfterStreaming([MARKDOWN_REPLY]);

  expect(streamed).toBe(whole);
  expect(whole).toContain("<table>");
  expect(whole).toContain("<pre>");
});

test("clicking a link in agent text opens it in the default browser through the bridge", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  act(() => bridge.messages("0", { type: "TurnStarted" }, { type: "TextDelta", text: "See [the docs](https://docs.rs/logos)." }));
  nextFrame();

  const navigated = fireEvent.click(screen.getByRole("link", { name: "the docs" }));
  await act(async () => {});

  expect(navigated).toBe(false);
  expect(bridge.openedLinks).toEqual(["https://docs.rs/logos"]);
});

test("the user's own message stays plain text, markdown and all", async () => {
  const bridge = await started();
  const { container } = await shownPanel(bridge);
  act(() => bridge.messages("0", { type: "User", text: "make **this** a `list`:\n- a" }));
  nextFrame();

  const message = container.querySelector(".msg-user")!;
  expect(message.textContent).toBe("make **this** a `list`:\n- a");
  expect(message.children).toHaveLength(0);
});

test("a refused link says why in the error line, and a link that opens clears it", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "TextDelta", text: "See [hosts](file:///etc/hosts) or [the docs](https://docs.rs/logos)." },
    ),
  );
  nextFrame();

  await act(async () => fireEvent.click(screen.getByRole("link", { name: "hosts" })));

  expect(screen.getByRole("alert").textContent).toBe(
    '"file:///etc/hosts" is not an http or https link, so grove does not open it',
  );
  expect(bridge.openedLinks).toEqual([]);

  await act(async () => fireEvent.click(screen.getByRole("link", { name: "the docs" })));

  expect(screen.queryByRole("alert")).toBeNull();
  expect(bridge.openedLinks).toEqual(["https://docs.rs/logos"]);
});

// The agent change notice (#4).

function notice() {
  return screen.queryByRole("group", { name: "Agent change" });
}

function noticeButtons() {
  return within(notice()!)
    .getAllByRole("button")
    .map((button) => button.textContent);
}

async function agentOpensLogs(bridge: FakeBridge, by = "0") {
  const workspace = await bridge.workspace(worktree);
  await act(async () =>
    bridge.agentChangesWorkspace(worktree, by, { ...workspace, panels: [...workspace.panels, "Logs"], selected: "Logs" }),
  );
}

test("an agent's workspace change shows a notice above the composer that names the session and the change", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  composer().focus();

  await agentOpensLogs(bridge);

  expect(notice()!.textContent).toContain("Claude Code changed this workspace:");
  expect(notice()!.textContent).toContain("opened the Logs panel");
  expect(noticeButtons()).toEqual(["Undo", "Keep"]);
  expect(notice()!.compareDocumentPosition(composer()) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(document.activeElement).toBe(composer());
});

test("the notice says each thing the agent changed", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  const workspace = await bridge.workspace(worktree);

  await act(async () =>
    bridge.agentChangesWorkspace(worktree, "0", {
      chat: [...workspace.chat, { agent: "Codex", session: null, shown: true }],
      panels: ["Diff", "Logs"],
      selected: "Diff",
      terminal: false,
    }),
  );

  expect(notice()!.textContent).toContain(
    "opened the Logs panel, closed the Files panel, closed the terminal and added a Codex panel",
  );
});

test("Undo restores the workspace exactly, and the notice goes", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  const before = await bridge.workspace(worktree);
  await agentOpensLogs(bridge);

  await act(async () => fireEvent.click(within(notice()!).getByRole("button", { name: "Undo" })));

  expect(bridge.undone).toEqual([worktree]);
  expect(await bridge.workspace(worktree)).toEqual(before);
  expect(notice()).toBeNull();
});

test("a refused Undo says why and keeps the notice", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  await agentOpensLogs(bridge);
  await act(() => bridge.undoWorkspace(worktree));

  await act(async () => fireEvent.click(within(notice()!).getByRole("button", { name: "Undo" })));

  expect(within(notice()!).getByRole("alert").textContent).toContain("left to undo");
});

test("Keep leaves the workspace as the agent made it, and the notice goes", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  await agentOpensLogs(bridge);

  fireEvent.click(within(notice()!).getByRole("button", { name: "Keep" }));

  expect(notice()).toBeNull();
  expect(bridge.undone).toEqual([]);
  expect((await bridge.workspace(worktree)).panels).toContain("Logs");
});

test("Save to profile shows when the repo uses a profile, and writes the workspace into it", async () => {
  const bridge = await started();
  await bridge.setRepoProfile(worktree, "Rust crate");
  await shownPanel(bridge);
  await agentOpensLogs(bridge);

  expect(noticeButtons()).toEqual(["Undo", "Keep", "Save to profile"]);
  await act(async () => fireEvent.click(within(notice()!).getByRole("button", { name: "Save to profile" })));

  expect(bridge.savedToProfile).toEqual([worktree]);
  expect(notice()).toBeNull();
});

test("a change the user made, or another agent session made, shows no notice", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  const workspace = await bridge.workspace(worktree);

  await act(() => bridge.setWorkspace(worktree, { ...workspace, terminal: false }));
  expect(notice()).toBeNull();

  await agentOpensLogs(bridge, "7");
  expect(notice()).toBeNull();
});

test("an agent's profile change shows a notice with Keep only", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  await act(async () => bridge.agentChangesProfile("0"));

  expect(notice()!.textContent).toContain("Claude Code changed a profile");
  expect(noticeButtons()).toEqual(["Keep"]);
  fireEvent.click(within(notice()!).getByRole("button", { name: "Keep" }));
  expect(notice()).toBeNull();
});

test("the notice shows in terminal mode too", async () => {
  const bridge = await started();
  await bridge.setMode("0", "Terminal");
  await shownPanel(bridge);

  await agentOpensLogs(bridge);

  expect(notice()!.textContent).toContain("opened the Logs panel");
});

test("an unanswered notice stays through a switch to terminal mode and back", async () => {
  const bridge = await started();
  await shownPanel(bridge);
  await agentOpensLogs(bridge);

  await switchTo("Terminal");
  expect(notice()!.textContent).toContain("opened the Logs panel");
  await switchTo("Chat");
  expect(notice()!.textContent).toContain("opened the Logs panel");

  await act(async () => fireEvent.click(within(notice()!).getByRole("button", { name: "Undo" })));
  expect(bridge.undone).toEqual([worktree]);
});
