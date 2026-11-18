import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import type { AgentState, AgentStateChanged, FileDiff, SessionId } from "./bridge";
import { DiffView } from "./DiffView";
import { type FakeBridge, fakeBridge, type FakeChange, type FakeDiffs } from "./fakeBridge";
import type { MonacoDiffProps } from "./MonacoDiff";

// Monaco needs a real browser. The stub shows exactly what the diff view hands it: both texts,
// each box under its line, and how often the editor was created. `commentLines` clicks the
// modified side's line numbers.
const editors = vi.hoisted(() => ({
  created: 0,
  commentLines: undefined as ((start: number, end: number) => void) | undefined,
}));
vi.mock("./MonacoDiff", async () => {
  const { useEffect } = await import("react");
  return {
    MonacoDiff: ({ original, modified, onCommentLines, zones }: MonacoDiffProps) => {
      useEffect(() => {
        editors.created++;
      }, []);
      editors.commentLines = onCommentLines;
      return (
        <div>
          <pre aria-label="Original">{original}</pre>
          <pre aria-label="Modified">{modified}</pre>
          {zones.map((zone) => (
            <div key={zone.key} role="region" aria-label={`Under line ${zone.afterLine}`}>
              {zone.content}
            </div>
          ))}
        </div>
      );
    },
  };
});

afterEach(() => {
  cleanup();
  editors.created = 0;
  editors.commentLines = undefined;
});

const worktree = "/code/grove-wt/feat-40";

const repos = { [worktree]: [{ path: worktree, branch: "feat-40", isMain: true }] };

/** Renders the diff view of `worktree`, which grove tracks from here on. */
async function renderDiff(
  bridge: FakeBridge,
  agentStates: Record<SessionId, AgentStateChanged> = {},
  onSent: (session: SessionId) => void = () => {},
) {
  if (!(await bridge.listWorktrees()).some((w) => w.path === worktree)) await bridge.addRepo(worktree);
  return render(<DiffView bridge={bridge} worktree={worktree} agentStates={agentStates} onSent={onSent} />);
}

function change(
  file: Partial<FileDiff> & { newPath: string },
  old: string | null,
  now: string | null,
): FakeChange {
  const binary = file.binary ?? false;
  return {
    file: { status: "Modified", oldPath: file.newPath, added: 0, removed: 0, binary, ...file },
    versions: { old: binary ? null : old, new: binary ? null : now, binary },
  };
}

function listed() {
  return within(screen.getByRole("list", { name: "Changed files" }))
    .getAllByRole("listitem")
    .map((item) => item.textContent);
}

test("lists each changed file with its added and removed line counts", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [
        change({ newPath: "src/lib.rs", added: 3, removed: 1 }, "a\n", "b\n"),
        change({ newPath: "README.md", status: "Added", added: 12 }, null, "# grove\n"),
      ],
    },
  };

  await renderDiff(fakeBridge(repos, diffs));

  expect(await screen.findAllByRole("listitem")).toHaveLength(2);
  expect(listed()).toEqual(["src/lib.rs +3 -1", "README.md +12 -0"]);
});

function editorShows() {
  return {
    original: screen.getByLabelText("Original").textContent,
    modified: screen.getByLabelText("Modified").textContent,
  };
}

test("the first file opens in the editor, and clicking another opens that one", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [
        change({ newPath: "a.rs", added: 1, removed: 1 }, "old a\n", "new a\n"),
        change({ newPath: "b.rs", status: "Added", added: 1 }, null, "new b\n"),
      ],
    },
  };

  await renderDiff(fakeBridge(repos, diffs));

  await screen.findByText("new a");
  expect(editorShows()).toEqual({ original: "old a\n", modified: "new a\n" });

  fireEvent.click(screen.getByRole("button", { name: /b\.rs/ }));

  await screen.findByText("new b");
  expect(editorShows()).toEqual({ original: "", modified: "new b\n" });
});

test("switching the diff base changes the file list and the editor", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [change({ newPath: "a.rs", added: 1, removed: 1 }, "staged a\n", "saved a\n")],
      MergeBase: [
        change({ newPath: "a.rs", added: 2, removed: 1 }, "base a\n", "saved a\n"),
        change({ newPath: "committed.rs", status: "Added", added: 4 }, null, "fn main() {}\n"),
      ],
    },
  };

  await renderDiff(fakeBridge(repos, diffs));
  await screen.findByText("staged a");

  const bases = screen.getByRole("group", { name: "Diff base" });
  expect(within(bases).getByRole("button", { name: "HEAD" }).getAttribute("aria-pressed")).toBe("true");
  fireEvent.click(within(bases).getByRole("button", { name: "Merge base" }));

  await screen.findByText("base a");
  expect(within(bases).getByRole("button", { name: "Merge base" }).getAttribute("aria-pressed")).toBe("true");
  expect(listed()).toEqual(["a.rs +2 -1", "committed.rs +4 -0"]);
  expect(editorShows()).toEqual({ original: "base a\n", modified: "saved a\n" });

  fireEvent.click(within(bases).getByRole("button", { name: "HEAD" }));

  await screen.findByText("staged a");
  expect(listed()).toEqual(["a.rs +1 -1"]);
});

test("a base the core rejects shows its error and no files", async () => {
  const diffs: FakeDiffs = { [worktree]: { Head: [change({ newPath: "a.rs" }, "a\n", "b\n")] } };

  await renderDiff(fakeBridge(repos, diffs));
  await screen.findByText("b");

  fireEvent.click(screen.getByRole("button", { name: "Merge base" }));

  expect((await screen.findByRole("alert")).textContent).toBe(
    `the repo at ${worktree} has no origin/HEAD, main or master to use as the base branch`,
  );
  expect(screen.queryAllByRole("listitem")).toEqual([]);
  expect(screen.queryByLabelText("Modified")).toBeNull();
});

test("a binary file shows a binary notice in the list and in place of the editor", async () => {
  const diffs: FakeDiffs = {
    [worktree]: { Head: [change({ newPath: "logo.png", binary: true }, null, null)] },
  };

  await renderDiff(fakeBridge(repos, diffs));

  expect(await screen.findByText("Binary file, not shown.")).toBeTruthy();
  expect(listed()).toEqual(["logo.png binary"]);
  expect(screen.queryByLabelText("Modified")).toBeNull();
});

test("a file git calls binary is never read, so a large one cannot fill memory", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [
        change({ newPath: "a.rs", added: 1 }, "", "a\n"),
        change({ newPath: "video.mp4", binary: true }, null, null),
      ],
    },
  };
  const bridge = fakeBridge(repos, diffs);
  const read = vi.spyOn(bridge, "fileVersions");
  await renderDiff(bridge);
  await screen.findByText("a");

  fireEvent.click(screen.getByRole("button", { name: /video\.mp4/ }));

  expect(await screen.findByText("Binary file, not shown.")).toBeTruthy();
  expect(read.mock.calls.map(([, , file]) => file.newPath)).toEqual(["a.rs"]);
});

test("a file git diffs as text but that is not UTF-8 shows the binary notice", async () => {
  const latin1: FakeChange = {
    file: {
      status: "Modified",
      oldPath: "notes.txt",
      newPath: "notes.txt",
      binary: false,
      added: 1,
      removed: 1,
    },
    versions: { old: null, new: null, binary: true },
  };

  await renderDiff(fakeBridge(repos, { [worktree]: { Head: [latin1] } }));

  expect(await screen.findByText("Binary file, not shown.")).toBeTruthy();
  expect(screen.queryByLabelText("Modified")).toBeNull();
});

test("DiffChanged for this worktree refreshes the file list and the editor", async () => {
  const diffs: FakeDiffs = {
    [worktree]: { Head: [change({ newPath: "a.rs", added: 1 }, "", "one\n")] },
  };
  const bridge = fakeBridge(repos, diffs);
  await renderDiff(bridge);
  await screen.findByText("one");

  diffs[worktree].Head = [
    change({ newPath: "a.rs", added: 2 }, "", "one\ntwo\n"),
    change({ newPath: "b.rs", status: "Untracked", added: 1 }, null, "new\n"),
  ];
  act(() => bridge.diffChanged("/code/grove-wt/other"));
  expect(listed()).toEqual(["a.rs +1 -0"]);

  act(() => bridge.diffChanged(worktree));

  await screen.findByText(/two/);
  expect(listed()).toEqual(["a.rs +2 -0", "b.rs +1 -0"]);
  expect(editorShows().modified).toBe("one\ntwo\n");
  // The same editor takes the new versions, so its scroll and cursor survive the refresh.
  expect(editors.created).toBe(1);
});

test("carriage returns reach the editor as a visible ␍, so a CRLF change shows", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [change({ newPath: "a.txt", added: 2, removed: 2 }, "one\ntwo\n", "one\r\ntwo\r\n")],
    },
  };

  await renderDiff(fakeBridge(repos, diffs));

  await screen.findByLabelText("Modified");
  expect(editorShows()).toEqual({ original: "one\ntwo\n", modified: "one␍\ntwo␍\n" });
});

// Review comments (#54).

const lib = "fn a() {}\nfn b() {}\nfn c() {}\n";

/** A worktree whose diff and disk both have `src/lib.rs` as `text`, with each agent session in `agents`. */
async function commentable(text = lib) {
  const diffs: FakeDiffs = { [worktree]: { Head: [change({ newPath: "src/lib.rs", added: 3 }, "", text)] } };
  const bridge = fakeBridge(repos, diffs, { [worktree]: { "src/lib.rs": text } });
  await bridge.addRepo(worktree);
  // As App keeps them: each agent session's latest state.
  const states: Record<SessionId, AgentStateChanged> = {};
  bridge.onAgentState((change) => (states[change.session] = change));
  return { bridge, diffs, states };
}

/** Changes `src/lib.rs` on disk and in the diff, as an agent's edit would, and says so as the watcher does. */
function edit(bridge: FakeBridge, diffs: FakeDiffs, text: string) {
  bridge.setFile(worktree, "src/lib.rs", text);
  diffs[worktree].Head = [change({ newPath: "src/lib.rs", added: 3 }, "", text)];
  act(() => bridge.diffChanged(worktree));
}

function comment(name: string) {
  return screen.getByRole("group", { name });
}

function under(line: number) {
  return screen.getByRole("region", { name: `Under line ${line}` });
}

test("clicking line numbers opens a comment box under them, and adding sends the path and lines to the core", async () => {
  const { bridge } = await commentable();
  await renderDiff(bridge);
  await screen.findByText(/fn b/);

  act(() => editors.commentLines!(2, 3));

  const box = within(under(3)).getByRole("form", { name: "New comment on lines 2 to 3" });
  fireEvent.change(within(box).getByRole("textbox"), { target: { value: "merge these" } });
  fireEvent.click(within(box).getByRole("button", { name: "Add comment" }));

  await screen.findByRole("group", { name: "Comment on lines 2 to 3" });
  expect(await bridge.comments(worktree)).toMatchObject([
    { path: "src/lib.rs", startLine: 2, endLine: 3, text: "fn b() {}\nfn c() {}", body: "merge these", state: "Open" },
  ]);
  expect(within(under(3)).getByText("merge these")).toBeTruthy();
  expect(within(comment("Comment on lines 2 to 3")).getByText("Open")).toBeTruthy();
  expect(screen.queryByRole("form")).toBeNull();
});

test("a comment box adds nothing when empty, and Cancel or Escape closes it", async () => {
  const { bridge } = await commentable();
  const addComment = vi.spyOn(bridge, "addComment");
  await renderDiff(bridge);
  await screen.findByText(/fn b/);

  act(() => editors.commentLines!(1, 1));
  const box = screen.getByRole("form", { name: "New comment on line 1" });
  expect(within(box).getByRole("button", { name: "Add comment" })).toHaveProperty("disabled", true);
  fireEvent.click(within(box).getByRole("button", { name: "Cancel" }));
  expect(screen.queryByRole("form")).toBeNull();

  act(() => editors.commentLines!(2, 2));
  fireEvent.keyDown(screen.getByRole("textbox"), { key: "Escape" });
  expect(screen.queryByRole("form")).toBeNull();
  expect(addComment).not.toHaveBeenCalled();
});

test("a comment the core refuses keeps its box, with the core's error", async () => {
  const { bridge } = await commentable();
  await renderDiff(bridge);
  await screen.findByText(/fn b/);

  act(() => editors.commentLines!(3, 9));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "past the end" } });
  fireEvent.click(screen.getByRole("button", { name: "Add comment" }));

  expect((await screen.findByRole("alert")).textContent).toBe(`${worktree}/src/lib.rs has no lines 3 to 9`);
  expect((screen.getByRole("textbox") as HTMLTextAreaElement).value).toBe("past the end");
});

test("a comment being typed follows its lines when the agent edits the file, and keeps what was typed", async () => {
  const { bridge, diffs } = await commentable();
  await renderDiff(bridge);
  await screen.findByText(/fn b/);
  act(() => editors.commentLines!(2, 3));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "merge these" } });

  edit(bridge, diffs, `// one\n// two\n${lib}`);

  await waitFor(() => under(5));
  const box = within(under(5)).getByRole("form", { name: "New comment on lines 4 to 5" });
  expect((within(box).getByRole("textbox") as HTMLTextAreaElement).value).toBe("merge these");
  fireEvent.click(within(box).getByRole("button", { name: "Add comment" }));

  await screen.findByRole("group", { name: "Comment on lines 4 to 5" });
  expect(await bridge.comments(worktree)).toMatchObject([
    { startLine: 4, endLine: 5, text: "fn b() {}\nfn c() {}", body: "merge these" },
  ]);
});

test("a comment being typed whose lines the agent changed asks to select them again", async () => {
  const { bridge, diffs } = await commentable();
  const addComment = vi.spyOn(bridge, "addComment");
  await renderDiff(bridge);
  await screen.findByText(/fn b/);
  act(() => editors.commentLines!(2, 2));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "rename b" } });

  edit(bridge, diffs, "fn a() {}\nfn renamed() {}\nfn c() {}\n");

  const box = screen.getByRole("form", { name: "New comment on line 2" });
  expect((await within(box).findByRole("alert")).textContent).toBe("Those lines changed. Select them again.");
  expect(within(box).getByRole("button", { name: "Add comment" })).toHaveProperty("disabled", true);
  fireEvent.keyDown(within(box).getByRole("textbox"), { key: "Enter", metaKey: true });
  expect(addComment).not.toHaveBeenCalled();
});

test("a comment follows its text when the agent edits the file", async () => {
  const { bridge, diffs } = await commentable();
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 2, endLine: 2, body: "rename b" });
  await renderDiff(bridge);
  await screen.findByRole("group", { name: "Comment on line 2" });
  expect(within(under(2)).getByText("rename b")).toBeTruthy();

  edit(bridge, diffs, `// one\n// two\n${lib}`);

  await screen.findByRole("group", { name: "Comment on line 4" });
  expect(within(under(4)).getByText("rename b")).toBeTruthy();
  expect(screen.queryByText("Possibly addressed")).toBeNull();
});

test("a comment whose text is gone stays where it was, flagged as possibly addressed", async () => {
  const { bridge, diffs } = await commentable();
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 2, endLine: 2, body: "rename b" });
  await renderDiff(bridge);
  await screen.findByRole("group", { name: "Comment on line 2" });

  edit(bridge, diffs, "fn a() {}\nfn renamed() {}\nfn c() {}\n");

  expect(await within(comment("Comment on line 2")).findByText("Possibly addressed")).toBeTruthy();
});

test("resolving a comment hides it", async () => {
  const { bridge } = await commentable();
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 1, endLine: 1, body: "first" });
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 3, endLine: 3, body: "third" });
  await renderDiff(bridge);
  await screen.findByRole("group", { name: "Comment on line 3" });

  fireEvent.click(within(comment("Comment on line 1")).getByRole("button", { name: "Resolve" }));

  await waitFor(() => expect(screen.queryByRole("group", { name: "Comment on line 1" })).toBeNull());
  expect((await bridge.comments(worktree)).map((c) => c.body)).toEqual(["third"]);
});

test("comments on files the diff does not show are listed apart, and can be resolved", async () => {
  const { bridge } = await commentable();
  bridge.setFile(worktree, "src/old.rs", "fn old() {}\n");
  await bridge.addComment(worktree, { path: "src/old.rs", startLine: 1, endLine: 1, body: "delete this" });
  await renderDiff(bridge);

  const elsewhere = await screen.findByRole("list", { name: "Comments the diff cannot show" });
  const item = within(elsewhere).getByRole("group", { name: "Comment on src/old.rs line 1" });
  expect(within(item).getByText("delete this")).toBeTruthy();

  fireEvent.click(within(item).getByRole("button", { name: "Resolve" }));

  await waitFor(() => expect(screen.queryByRole("list", { name: "Comments the diff cannot show" })).toBeNull());
});

test.each([
  ["git calls binary", change({ newPath: "src/lib.rs", binary: true }, null, null)],
  [
    "is not UTF-8",
    {
      file: { status: "Modified", oldPath: "src/lib.rs", newPath: "src/lib.rs", binary: false, added: 1, removed: 1 },
      versions: { old: null, new: null, binary: true },
    } satisfies FakeChange,
  ],
])("comments on a file that %s are listed apart, and can be resolved", async (_, binary) => {
  const { bridge, diffs } = await commentable();
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 1, endLine: 1, body: "first" });
  diffs[worktree].Head = [binary];
  await renderDiff(bridge);

  const elsewhere = await screen.findByRole("list", { name: "Comments the diff cannot show" });
  const item = within(elsewhere).getByRole("group", { name: "Comment on src/lib.rs line 1" });

  fireEvent.click(within(item).getByRole("button", { name: "Resolve" }));

  await waitFor(() => expect(screen.queryByRole("list", { name: "Comments the diff cannot show" })).toBeNull());
});

function stateChange(session: SessionId, state: AgentState): AgentStateChanged {
  return { session, source: "Protocol", ...state };
}

/**
 * Two open comments, and five agent sessions in the worktree: 0 Claude in chat mode, Idle and
 * focused last; 1 Codex, Working; 2 Claude in terminal mode with bracketed paste off; 3 Claude
 * waiting on an approval; 4 Claude in terminal mode, Done, with bracketed paste on.
 */
async function reviewable() {
  const setup = await commentable();
  const { bridge } = setup;
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 1, endLine: 1, body: "first" });
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 2, endLine: 3, body: "second" });
  for (const agent of ["Claude", "Codex", "Claude", "Claude", "Claude"] as const) {
    await bridge.spawnAgent(worktree, agent, null);
  }
  await bridge.setMode("2", "Terminal");
  await bridge.setMode("4", "Terminal");
  bridge.setBracketedPaste("4", true);
  bridge.agentStateChanged(stateChange("0", { state: "Idle" }));
  bridge.agentStateChanged(stateChange("1", { state: "Working" }));
  bridge.agentStateChanged(stateChange("2", { state: "Idle" }));
  bridge.agentStateChanged(stateChange("3", { state: "NeedsInput", kind: "Approval" }));
  bridge.agentStateChanged(stateChange("4", { state: "Done" }));
  await bridge.focus("0");
  return setup;
}

async function openPicker() {
  fireEvent.click(await screen.findByRole("button", { name: /^Send review batch/ }));
  return screen.findByRole("dialog", { name: "Send review batch" });
}

function options(picker: HTMLElement) {
  return within(picker)
    .getAllByRole("button", { name: /mode/ })
    .map((option) => [option.getAttribute("aria-label"), option.getAttribute("aria-pressed"), (option as HTMLButtonElement).disabled]);
}

test("Send review batch opens a picker that defaults to the session focused last and says why others cannot take it", async () => {
  const { bridge, states } = await reviewable();
  await renderDiff(bridge, states);

  const picker = await openPicker();

  expect(within(picker).getByText("Send 2 comments to")).toBeTruthy();
  await waitFor(() =>
    expect(options(picker)).toEqual([
      ["Claude Code · chat mode. Focused last", "true", false],
      ["Codex · chat mode. Working: wait for the turn to end", "false", true],
      ["Claude Code · terminal mode. Bracketed paste is off in its TUI", "false", true],
      ["Claude Code · chat mode. NeedsInput: answer its approval first", "false", true],
      ["Claude Code · terminal mode", "false", false],
    ]),
  );
  expect(within(picker).getByRole("button", { name: "Send" })).toHaveProperty("disabled", false);
});

test("sending marks the comments Sent, and a chat-mode session gets them as one review batch", async () => {
  const { bridge, states } = await reviewable();
  const onSent = vi.fn();
  await renderDiff(bridge, states, onSent);
  const picker = await openPicker();
  await waitFor(() => expect(options(picker)[0][1]).toBe("true"));

  fireEvent.click(within(picker).getByRole("button", { name: "Send" }));

  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(bridge.reviewBatches).toEqual([{ worktree, to: "0" }]);
  expect(onSent).toHaveBeenCalledWith("0");
  const history = await bridge.subscribe("0", () => {});
  expect(history).toEqual([
    {
      type: "ReviewBatch",
      comments: [
        { path: "src/lib.rs", startLine: 1, endLine: 1, text: "fn a() {}", body: "first" },
        { path: "src/lib.rs", startLine: 2, endLine: 3, text: "fn b() {}\nfn c() {}", body: "second" },
      ],
    },
  ]);
  await waitFor(() => expect(within(comment("Comment on line 1")).getByText("Sent")).toBeTruthy());
  expect(within(comment("Comment on lines 2 to 3")).getByText("Sent")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Send review batch/ })).toBeNull();
});

test("picking another session sends the batch to it", async () => {
  const { bridge, states } = await reviewable();
  await renderDiff(bridge, states);
  const picker = await openPicker();
  await waitFor(() => expect(options(picker)[4][2]).toBe(false));

  fireEvent.click(within(picker).getByRole("button", { name: "Claude Code · terminal mode" }));
  expect(options(picker).map(([, pressed]) => pressed)).toEqual(["false", "false", "false", "false", "true"]);
  fireEvent.click(within(picker).getByRole("button", { name: "Send" }));

  await waitFor(() => expect(bridge.reviewBatches).toEqual([{ worktree, to: "4" }]));
});

test("the picker takes focus when it opens and leaves it on the session the user picks", async () => {
  const { bridge, states } = await reviewable();
  await renderDiff(bridge, states);
  const picker = await openPicker();
  expect(document.activeElement).toBe(picker);
  await waitFor(() => expect(options(picker)[4][2]).toBe(false));
  const option = within(picker).getByRole("button", { name: "Claude Code · terminal mode" });

  option.focus();
  fireEvent.click(option);

  expect(option.getAttribute("aria-pressed")).toBe("true");
  expect(document.activeElement).toBe(option);
});

test("a send the core refuses says why in the picker and keeps the comments open", async () => {
  const { bridge, states } = await reviewable();
  await renderDiff(bridge, states);
  const picker = await openPicker();
  await waitFor(() => expect(options(picker)[0][1]).toBe("true"));
  // The turn starts after the picker drew its blockers.
  bridge.agentStateChanged(stateChange("0", { state: "Working" }));

  fireEvent.click(within(picker).getByRole("button", { name: "Send" }));

  expect((await within(picker).findByRole("alert")).textContent).toBe(
    "agent session 0 cannot take a review batch while Working",
  );
  expect(bridge.reviewBatches).toEqual([]);
  expect(screen.getByRole("button", { name: "Send review batch (2)" })).toBeTruthy();
});

test("with no agent session in the worktree the picker says so and cannot send", async () => {
  const { bridge } = await commentable();
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 1, endLine: 1, body: "first" });
  await renderDiff(bridge);

  const picker = await openPicker();

  expect(await within(picker).findByText("No agent session in this worktree yet.")).toBeTruthy();
  expect(within(picker).getByRole("button", { name: "Send" })).toHaveProperty("disabled", true);
  fireEvent.keyDown(picker, { key: "Escape" });
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("the review batch button counts the open comments and leaves with the last one, picker and all", async () => {
  const { bridge } = await commentable();
  await renderDiff(bridge);
  await screen.findByLabelText("Modified");
  await act(() => bridge.addComment(worktree, { path: "src/lib.rs", startLine: 1, endLine: 1, body: "first" }));
  await act(() => bridge.addComment(worktree, { path: "src/lib.rs", startLine: 2, endLine: 3, body: "second" }));

  const picker = await openPicker();
  expect(screen.getByRole("button", { name: "Send review batch (2)" })).toBeTruthy();
  fireEvent.click(within(comment("Comment on line 1")).getByRole("button", { name: "Resolve" }));
  fireEvent.click(within(comment("Comment on lines 2 to 3")).getByRole("button", { name: "Resolve" }));

  await waitFor(() => expect(screen.queryByRole("button", { name: /^Send review batch/ })).toBeNull());
  expect(picker.isConnected).toBe(false);
  await act(() => bridge.addComment(worktree, { path: "src/lib.rs", startLine: 1, endLine: 1, body: "third" }));
  expect(await screen.findByRole("button", { name: "Send review batch (1)" })).toBeTruthy();
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("with no comments open there is no review batch button", async () => {
  const { bridge } = await commentable();
  await renderDiff(bridge);
  await screen.findByRole("group", { name: "Diff base" });
  await screen.findByLabelText("Modified");

  expect(screen.queryByRole("button", { name: /^Send review batch/ })).toBeNull();
});
