import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { Profiler } from "react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import type { Agent, AgentMessage, AgentStateChanged, Question, Reply } from "./bridge";
import { ChatPanel } from "./ChatPanel";
import { fakeBridge, type FakeBridge } from "./fakeBridge";

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
  await shownPanel(bridge);

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "toolu_01", tool: "Bash", input: { command: "npm test" } },
      { type: "ToolResult", id: "toolu_01", output: "3 passed", isError: false },
      { type: "ToolCall", id: "toolu_02", tool: "Read", input: { file_path: `${worktree}/src/a.ts` } },
      { type: "ToolCall", id: "toolu_03", tool: "git/status", input: { short: true } },
      { type: "ToolResult", id: "toolu_03", output: "not a repo", isError: true },
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
  fireEvent.click(used);

  expect(ran.getAttribute("aria-expanded")).toBe("true");
  expect(screen.getByText("3 passed")).toBeTruthy();
  expect(screen.getByText("Running")).toBeTruthy();
  expect(screen.getByText("not a repo")).toBeTruthy();
  expect(document.querySelector(".messages")!.textContent).not.toMatch(/[{}"]/);
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

test("while the agent works the thread says for how long, with no dots", async () => {
  vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
  try {
    const bridge = await started();
    const view = await shownPanel(bridge, { state: working("Working") });
    expect(screen.getByText("Working for 0s")).toBeTruthy();
    expect(document.querySelector(".dots")).toBeNull();

    act(() => vi.advanceTimersByTime(63_000));
    expect(screen.getByText("Working for 1m 3s")).toBeTruthy();

    // Waiting on the user is part of the turn: the count goes on once it works again.
    view.rerender(<Panel bridge={bridge} state={{ session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" }} />);
    expect(screen.queryByText(/^Working for/)).toBeNull();
    act(() => vi.advanceTimersByTime(3_600_000));
    view.rerender(<Panel bridge={bridge} state={working("Working")} />);
    expect(screen.getByText("Working for 1h 1m")).toBeTruthy();

    view.rerender(<Panel bridge={bridge} state={working("Done")} />);
    view.rerender(<Panel bridge={bridge} state={working("Working")} />);
    expect(screen.getByText("Working for 0s")).toBeTruthy();
  } finally {
    vi.useRealTimers();
  }
});

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

test("a turn's edits show as a files-changed card that opens the diff", async () => {
  const bridge = await started();
  const onOpenDiff = vi.fn();
  await shownPanel(bridge, { onOpenDiff });

  act(() =>
    bridge.messages(
      "0",
      { type: "TurnStarted" },
      { type: "ToolCall", id: "1", tool: "Edit", input: { file_path: `${worktree}/src/a.ts` } },
      { type: "ToolCall", id: "2", tool: "Write", input: { file_path: `${worktree}/src/b.ts` } },
      { type: "ToolCall", id: "3", tool: "MultiEdit", input: { file_path: `${worktree}/src/a.ts` } },
      { type: "ToolCall", id: "4", tool: "NotebookEdit", input: { notebook_path: "/elsewhere/n.ipynb" } },
      { type: "ToolCall", id: "5", tool: "Read", input: { file_path: `${worktree}/src/c.ts` } },
      { type: "TurnEnded", outcome: "Completed" },
    ),
  );
  nextFrame();

  const card = screen.getByRole("group", { name: "3 files changed" });
  expect(within(card).getAllByRole("listitem").map((item) => item.textContent)).toEqual([
    "src/a.ts",
    "src/b.ts",
    "/elsewhere/n.ipynb",
  ]);
  fireEvent.click(within(card).getByRole("button", { name: "Review diff" }));
  expect(onOpenDiff).toHaveBeenCalledOnce();
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

test("the header shows the agent, its state with the source, and Chat mode", async () => {
  const bridge = await started();
  await shownPanel(bridge, { state: working("Working") });

  expect(document.querySelector(".pane-head b")!.textContent).toBe("Claude Code");
  expect(document.querySelector(".pane-head .ui-agent-state")!.textContent).toBe("Working · Protocol");
  const mode = screen.getByRole("group", { name: "Mode" });
  expect(within(mode).getByRole("button", { name: "Chat" }).getAttribute("aria-pressed")).toBe("true");
  expect((within(mode).getByRole("button", { name: "Terminal" }) as HTMLButtonElement).disabled).toBe(true);
});

// The account line moved to the sidebar's footer (#29).
test("the composer has no account line", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  expect(document.querySelector(".composer-foot")).toBeNull();
  expect(pickerButton().textContent).toContain("Default (recommended) · high effort");
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
  expect(pickerButton().textContent).toContain("GPT-5.5 · medium effort");

  // A model that takes the effort keeps it.
  fireEvent.click(option(openPicker(), /^GPT-5.5 mini/));
  expect(pickerButton().textContent).toContain("GPT-5.5 mini · medium effort");
  const efforts = within(openPicker()).getByRole("group", { name: "Effort" });
  expect(within(efforts).getAllByRole("button").map((b) => b.textContent)).toEqual(["low", "medium"]);
  fireEvent.click(within(efforts).getByRole("button", { name: "low" }));

  expect(pickerButton().textContent).toContain("GPT-5.5 mini · low effort");
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

  expect(pickerButton().textContent).toContain("GPT-5.5 mini · low effort");
  expect(bridge.modelsSet).toHaveLength(sent);
});

test("a model that does not take the current effort gets its own default, or none", async () => {
  const bridge = await started();
  await shownPanel(bridge);

  fireEvent.click(option(openPicker(), /^Haiku 5.5/));

  expect(pickerButton().textContent).toContain("Haiku 5.5");
  expect(pickerButton().textContent).not.toContain("effort");
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

// A panel showing a turn that waits on `request`.
async function asked(request: AgentMessage) {
  const bridge = await started();
  await shownPanel(bridge, { state: { session: "0", state: "NeedsInput", kind: "Approval", source: "Protocol" } });
  act(() => bridge.messages("0", { type: "TurnStarted" }, request));
  nextFrame();
  return bridge;
}

const approval: AgentMessage = {
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

test.each<[string, Reply, string]>([
  ["Allow", { type: "AllowOnce" }, "Allowed Bash once"],
  ["Always allow this session", { type: "AllowForSession" }, "Allowed Bash for this session"],
  ["Deny", { type: "Deny", message: null }, "Denied Bash"],
])("an approval's %s button sends its reply and the card becomes a record", async (button, reply, record) => {
  const bridge = await asked(approval);

  const card = screen.getByRole("group", { name: "Claude Code wants to use Bash" });
  expect(within(card).getByText("npm test")).toBeTruthy();
  expect(within(card).getByText("Waiting for approval")).toBeTruthy();
  fireEvent.click(within(card).getByRole("button", { name: button }));
  await settle();

  expect(bridge.answered).toEqual([{ session: "0", request: "toolu_01", reply }]);
  expect(screen.queryByRole("group", { name: "Claude Code wants to use Bash" })).toBeNull();
  expect(screen.queryByRole("button", { name: button })).toBeNull();
  expect(screen.getByText(record)).toBeTruthy();
});

test("a card's buttons send once, even when pressed again before the answer is back", async () => {
  const bridge = await asked(approval);

  const allow = screen.getByRole("button", { name: "Allow" });
  fireEvent.click(allow);
  fireEvent.click(allow);
  fireEvent.click(screen.getByRole("button", { name: "Deny" }));
  await settle();

  expect(bridge.answered).toHaveLength(1);
});

test("a single-choice question sends the chosen option", async () => {
  const bridge = await asked({ type: "QuestionAsked", request: "toolu_02", questions: [runner] });

  const card = screen.getByRole("group", { name: "Claude Code is asking" });
  expect(within(card).getByText("Which test runner?")).toBeTruthy();
  expect(within(card).getByText("Fast, Vite native")).toBeTruthy();
  const send = within(card).getByRole("button", { name: "Answer" }) as HTMLButtonElement;
  expect(send.disabled).toBe(true);

  fireEvent.click(within(card).getByRole("radio", { name: /jest/ }));
  fireEvent.click(within(card).getByRole("radio", { name: /vitest/ }));
  fireEvent.click(send);
  await settle();

  expect(bridge.answered).toEqual([
    { session: "0", request: "toolu_02", reply: { type: "Answers", answers: [["vitest"]] } },
  ]);
  expect(screen.queryByRole("group", { name: "Claude Code is asking" })).toBeNull();
  expect(screen.getByText("Answered: vitest")).toBeTruthy();
});

test("a multiple-choice question sends every selected option, one answer per question", async () => {
  const bridge = await asked({ type: "QuestionAsked", request: "toolu_02", questions: [runner, kinds] });

  const card = screen.getByRole("group", { name: "Claude Code is asking" });
  fireEvent.click(within(card).getByRole("radio", { name: /vitest/ }));
  fireEvent.click(within(card).getByRole("checkbox", { name: /unit/ }));
  fireEvent.click(within(card).getByRole("checkbox", { name: /e2e/ }));
  fireEvent.click(within(card).getByRole("checkbox", { name: /integration/ }));
  fireEvent.click(within(card).getByRole("checkbox", { name: /e2e/ }));
  fireEvent.click(within(card).getByRole("button", { name: "Answer" }));
  await settle();

  expect(bridge.answered[0].reply).toEqual({ type: "Answers", answers: [["vitest"], ["unit", "integration"]] });
  expect(screen.getByText("Answered: Runner: vitest; Kinds: unit, integration")).toBeTruthy();
});

test("a free-text answer sends the text in place of any option", async () => {
  const bridge = await asked({ type: "QuestionAsked", request: "toolu_02", questions: [runner] });

  const card = screen.getByRole("group", { name: "Claude Code is asking" });
  fireEvent.click(within(card).getByRole("radio", { name: /jest/ }));
  fireEvent.change(within(card).getByRole("textbox", { name: "Your own answer to Runner" }), {
    target: { value: "node:test" },
  });
  expect((within(card).getByRole("radio", { name: /jest/ }) as HTMLInputElement).checked).toBe(false);
  fireEvent.click(within(card).getByRole("button", { name: "Answer" }));
  await settle();

  expect(bridge.answered[0].reply).toEqual({ type: "Answers", answers: [["node:test"]] });
  expect(screen.getByText("Answered: node:test")).toBeTruthy();
});

test("a card whose turn ended shows as expired and sends nothing", async () => {
  const bridge = await asked(approval);

  act(() => bridge.messages("0", { type: "TurnEnded", outcome: "Interrupted" }));
  nextFrame();

  expect(screen.queryByRole("button", { name: "Allow" })).toBeNull();
  expect(screen.getByText("Expired: Claude Code asked to use Bash")).toBeTruthy();
  expect(bridge.answered).toEqual([]);
});

test("a card the core no longer has pending shows as expired when answered", async () => {
  const bridge = await asked({ type: "QuestionAsked", request: "toolu_02", questions: [runner] });
  // The turn ended in the core, and the panel has not drawn that yet.
  act(() => bridge.messages("0", { type: "TurnEnded", outcome: "Interrupted" }));

  fireEvent.click(screen.getByRole("radio", { name: /vitest/ }));
  fireEvent.click(screen.getByRole("button", { name: "Answer" }));
  await act(async () => {});

  expect(bridge.answered).toEqual([]);
  expect(screen.queryByRole("button", { name: "Answer" })).toBeNull();
  expect(screen.getByText("Expired: Which test runner?")).toBeTruthy();
});

test("a new card scrolls into view, and its controls take keyboard focus in order", async () => {
  const scrolled: Element[] = [];
  Element.prototype.scrollIntoView = function (this: Element) {
    scrolled.push(this);
  };
  await asked(approval);

  const card = screen.getByRole("group", { name: "Claude Code wants to use Bash" });
  expect(scrolled).toContain(card);
  const buttons = within(card).getAllByRole("button");
  expect(buttons.map((b) => b.textContent)).toEqual(["Allow", "Deny", "Always allow this session"]);
  buttons[0].focus();
  expect(document.activeElement).toBe(buttons[0]);
  expect(buttons.every((b) => b.tabIndex === 0)).toBe(true);
});

test("a request answered before the panel opened shows as its record", async () => {
  const bridge = await started();
  bridge.messages(
    "0",
    { type: "TurnStarted" },
    approval,
    { type: "Answered", request: "toolu_01", reply: { type: "AllowOnce" } },
    { type: "TurnEnded", outcome: "Completed" },
  );
  await shownPanel(bridge);

  expect(screen.getByText("Allowed Bash once")).toBeTruthy();
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
  const mode = screen.getByRole("group", { name: "Mode" });
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
