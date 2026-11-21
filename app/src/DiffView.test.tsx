import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, onTestFinished, test, vi } from "vitest";

import type { AgentState, AgentStateChanged, FileDiff, SessionId } from "./bridge";
import { DiffView } from "./DiffView";
import { type FakeBridge, fakeBridge, type FakeChange, type FakeDiffs } from "./fakeBridge";
import type { MonacoDiffProps } from "./MonacoDiff";

// Monaco needs a real browser. The stub shows exactly what the diff view hands it: both texts (no
// original for a file with no old side), each box under its line, and which file each editor was
// created for. `commentLines` clicks the modified side's line numbers.
const editors = vi.hoisted(() => ({
  created: [] as string[],
  commentLines: undefined as ((start: number, end: number) => void) | undefined,
}));
vi.mock("./MonacoDiff", async () => {
  const { useEffect, useState } = await import("react");
  return {
    MonacoDiff: ({ original, modified, path, onCommentLines, zones }: MonacoDiffProps) => {
      // The file the editor was created for. A later path is a new file in the same editor.
      const [first] = useState(path);
      useEffect(() => {
        editors.created.push(first);
      }, [first]);
      editors.commentLines = onCommentLines;
      return (
        <div>
          {original !== null && <pre aria-label="Original">{original}</pre>}
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

// jsdom lays nothing out, so it has no IntersectionObserver or ResizeObserver. The fakes say every
// file's section is near the view unless its path is in `farFromView`, and `scrollNear` and `grow`
// report what scrolling and a section taking its height would.
const layout = {
  farFromView: new Set<string>(),
  near: [] as { target: Element; callback: IntersectionObserverCallback }[],
  resized: [] as ResizeObserverCallback[],
};

class FakeIntersectionObserver {
  private readonly callback: IntersectionObserverCallback;
  constructor(callback: IntersectionObserverCallback) {
    this.callback = callback;
  }
  observe(target: Element) {
    layout.near.push({ target, callback: this.callback });
    const isIntersecting = !layout.farFromView.has(target.getAttribute("aria-label") ?? "");
    this.callback([{ target, isIntersecting } as IntersectionObserverEntry], this as unknown as IntersectionObserver);
  }
  disconnect() {
    layout.near = layout.near.filter(({ callback }) => callback !== this.callback);
  }
}

class FakeResizeObserver {
  private readonly callback: ResizeObserverCallback;
  constructor(callback: ResizeObserverCallback) {
    this.callback = callback;
  }
  observe() {
    layout.resized.push(this.callback);
  }
  disconnect() {
    layout.resized = layout.resized.filter((callback) => callback !== this.callback);
  }
}

/** Scrolls the section of `path` near the view. */
function scrollNear(path: string) {
  layout.farFromView.delete(path);
  act(() =>
    layout.near
      .filter(({ target }) => target.getAttribute("aria-label") === path)
      .forEach(({ target, callback }) =>
        callback([{ target, isIntersecting: true } as IntersectionObserverEntry], {} as IntersectionObserver),
      ),
  );
}

/** Says the file list changed height, as it does when an editor in it takes its height. */
function grow() {
  act(() => layout.resized.forEach((callback) => callback([], {} as ResizeObserver)));
}

beforeEach(() => {
  vi.stubGlobal("IntersectionObserver", FakeIntersectionObserver);
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  editors.created = [];
  editors.commentLines = undefined;
  layout.farFromView.clear();
  layout.near = [];
  layout.resized = [];
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

/** Each file section's header: its path and its added and removed counts. */
function listed() {
  return within(screen.getByRole("list", { name: "Changed files" }))
    .queryAllByRole("heading")
    .map((heading) => heading.textContent);
}

/** The section of the file at `path`. */
function section(path: string) {
  return screen.getByRole("listitem", { name: path });
}

test("each changed file gets a section with its path and its added and removed line counts", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [
        change({ newPath: "src/lib.rs", added: 3, removed: 1 }, "a\n", "b\n"),
        change({ newPath: "README.md", status: "Added", added: 12 }, null, "# grove\n"),
        change({ newPath: "src/main.rs", added: 2, removed: 5 }, "c\n", "d\n"),
      ],
    },
  };

  await renderDiff(fakeBridge(repos, diffs));

  await waitFor(() => expect(listed()).toEqual(["src/lib.rs +3 -1", "README.md +12 -0", "src/main.rs +2 -5"]));
  expect((await within(section("src/lib.rs")).findByLabelText("Modified")).textContent).toBe("b\n");
  expect(within(section("src/main.rs")).getByLabelText("Modified").textContent).toBe("d\n");
});

/** What the editor in the section of `path` shows. */
function editorShows(path: string) {
  return {
    original: within(section(path)).getByLabelText("Original").textContent,
    modified: within(section(path)).getByLabelText("Modified").textContent,
  };
}

test("switching the diff base switches the whole list, each section with its counts and its diff", async () => {
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
  expect(editorShows("a.rs")).toEqual({ original: "base a\n", modified: "saved a\n" });
  // Each file shows as its own read lands, so this one may come after a.rs.
  expect((await within(section("committed.rs")).findByLabelText("Modified")).textContent).toBe("fn main() {}\n");

  fireEvent.click(within(bases).getByRole("button", { name: "HEAD" }));

  await screen.findByText("staged a");
  expect(listed()).toEqual(["a.rs +1 -1"]);
  expect(editorShows("a.rs")).toEqual({ original: "staged a\n", modified: "saved a\n" });
});

test("a file asked for from outside opens on the HEAD base, expanded and scrolled to, and the view says it did", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [
        change({ newPath: "a.rs", added: 1, removed: 1 }, "old a\n", "new a\n"),
        change({ newPath: "b.rs", status: "Added", added: 1 }, null, "new b\n"),
      ],
      MergeBase: [change({ newPath: "c.rs", status: "Added", added: 1 }, null, "new c\n")],
    },
  };
  const bridge = fakeBridge(repos, diffs);
  await bridge.addRepo(worktree);
  const onRevealed = vi.fn();
  const scrolledTo = watchScrolls();
  const props = { bridge, worktree, agentStates: {}, onSent: () => {}, onRevealed };
  const view = render(<DiffView {...props} reveal={null} />);
  await screen.findByText("new b");
  fireEvent.click(within(section("b.rs")).getByRole("button", { name: /b\.rs/ }));
  const bases = screen.getByRole("group", { name: "Diff base" });
  fireEvent.click(within(bases).getByRole("button", { name: "Merge base" }));
  await screen.findByText("new c");

  view.rerender(<DiffView {...props} reveal="b.rs" />);

  await screen.findByText("new b");
  expect(within(bases).getByRole("button", { name: "HEAD" }).getAttribute("aria-pressed")).toBe("true");
  expect(within(section("b.rs")).getByRole("button", { name: /b\.rs/ }).getAttribute("aria-expanded")).toBe("true");
  expect(scrolledTo).toEqual([section("b.rs")]);
  expect(onRevealed).toHaveBeenCalledOnce();
});

/** Each element scrolled into view from here on. jsdom lays nothing out, so it has no scrollIntoView. */
function watchScrolls(): Element[] {
  const scrolledTo: Element[] = [];
  Element.prototype.scrollIntoView = function (this: Element) {
    scrolledTo.push(this);
  };
  onTestFinished(() => {
    delete (Element.prototype as Partial<Element>).scrollIntoView;
  });
  return scrolledTo;
}

/** The diff view of `worktree` with `a.rs` and `b.rs` changed, and a rerender that asks to reveal a file. */
async function revealable(diffs: FakeDiffs) {
  const bridge = fakeBridge(repos, diffs);
  await bridge.addRepo(worktree);
  const props = { bridge, worktree, agentStates: {}, onSent: () => {} };
  const view = render(<DiffView {...props} reveal={null} />);
  await screen.findByRole("listitem", { name: "a.rs" });
  return { bridge, reveal: (path: string) => view.rerender(<DiffView {...props} reveal={path} />) };
}

const twoFiles = (): FakeDiffs => ({
  [worktree]: {
    Head: [
      change({ newPath: "a.rs", added: 1 }, "", "a\n"),
      change({ newPath: "b.rs", added: 1 }, "", "b\n"),
    ],
  },
});

test("a revealed file stays in view while the sections above it take their height, until the user scrolls", async () => {
  const scrolledTo = watchScrolls();
  const { reveal } = await revealable(twoFiles());

  reveal("b.rs");
  expect(scrolledTo).toEqual([section("b.rs")]);

  grow();
  expect(scrolledTo).toEqual([section("b.rs"), section("b.rs")]);

  fireEvent.wheel(screen.getByRole("list", { name: "Changed files" }));
  grow();
  expect(scrolledTo).toHaveLength(2);
});

test("switching the diff base lets go of a revealed file, so the view does not jump back to it", async () => {
  const scrolledTo = watchScrolls();
  const diffs = twoFiles();
  diffs[worktree].MergeBase = [change({ newPath: "b.rs", added: 2 }, "", "base b\n")];
  const { reveal } = await revealable(diffs);
  reveal("b.rs");
  expect(scrolledTo).toHaveLength(1);

  fireEvent.click(within(screen.getByRole("group", { name: "Diff base" })).getByRole("button", { name: "Merge base" }));
  await waitFor(() => expect(listed()).toEqual(["b.rs +2 -0"]));
  grow();

  expect(scrolledTo).toHaveLength(1);
});

test("a revealed file that is not in the HEAD diff is dropped, so it does not scroll the view when it shows up later", async () => {
  const scrolledTo = watchScrolls();
  const diffs = twoFiles();
  const { bridge, reveal } = await revealable(diffs);

  reveal("committed.rs");
  diffs[worktree] = { Head: [...twoFiles()[worktree].Head!, change({ newPath: "committed.rs", added: 1 }, "", "c\n")] };
  act(() => bridge.diffChanged(worktree));

  await screen.findByRole("listitem", { name: "committed.rs" });
  expect(scrolledTo).toEqual([]);
});

test("the Diff opens on the base it is asked for, and a new ask switches to it again", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [change({ newPath: "a.rs", added: 1, removed: 1 }, "staged a\n", "saved a\n")],
      MergeBase: [change({ newPath: "a.rs", added: 2, removed: 1 }, "base a\n", "saved a\n")],
    },
  };
  const bridge = fakeBridge(repos, diffs);
  await bridge.addRepo(worktree);
  const view = (request: number) => (
    <DiffView
      bridge={bridge}
      worktree={worktree}
      agentStates={{}}
      onSent={() => {}}
      opened={{ base: "MergeBase", request }}
    />
  );
  const { rerender } = render(view(1));

  await screen.findByText("base a");
  const bases = screen.getByRole("group", { name: "Diff base" });
  expect(within(bases).getByRole("button", { name: "Merge base" }).getAttribute("aria-pressed")).toBe("true");

  fireEvent.click(within(bases).getByRole("button", { name: "HEAD" }));
  await screen.findByText("staged a");

  // The same ask again keeps the user's choice. A new one goes back to the merge base.
  rerender(view(1));
  expect(within(bases).getByRole("button", { name: "HEAD" }).getAttribute("aria-pressed")).toBe("true");
  rerender(view(2));
  await screen.findByText("base a");
  expect(within(bases).getByRole("button", { name: "Merge base" }).getAttribute("aria-pressed")).toBe("true");
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

  expect(within(section("video.mp4")).getByText("Binary file, not shown.")).toBeTruthy();
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
  expect(editorShows("a.rs").modified).toBe("one\ntwo\n");
  await within(section("b.rs")).findByText("new");
  // The same editor takes the new versions, so its scroll and cursor survive the refresh.
  expect(editors.created).toEqual(["a.rs", "b.rs"]);
});

test("a file is read only while its section is open and near the view, and a refresh reads only those again", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [
        change({ newPath: "a.rs", added: 1 }, "", "a\n"),
        change({ newPath: "b.rs", added: 1 }, "", "b\n"),
        change({ newPath: "c.rs", added: 1 }, "", "c\n"),
      ],
    },
  };
  const bridge = fakeBridge(repos, diffs);
  const read = vi.spyOn(bridge, "fileVersions");
  const reads = () => read.mock.calls.map(([, , file]) => file.newPath);
  layout.farFromView.add("c.rs");
  await renderDiff(bridge);
  await screen.findByText("b");
  expect(reads()).toEqual(["a.rs", "b.rs"]);
  expect(within(section("c.rs")).queryByLabelText("Modified")).toBeNull();

  fireEvent.click(within(section("b.rs")).getByRole("button", { name: /b\.rs/ }));
  scrollNear("c.rs");
  await within(section("c.rs")).findByText("c");
  expect(reads()).toEqual(["a.rs", "b.rs", "c.rs"]);

  act(() => bridge.diffChanged(worktree));
  await waitFor(() => expect(reads()).toHaveLength(5));
  expect(reads().slice(3).sort()).toEqual(["a.rs", "c.rs"]);

  // Opened again after the refresh, it reads its file again.
  fireEvent.click(within(section("b.rs")).getByRole("button", { name: /b\.rs/ }));
  await waitFor(() => expect(reads()).toHaveLength(6));
  expect(reads()[5]).toBe("b.rs");
});

test("each file shows as soon as its own read lands, whatever the others take", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [
        change({ newPath: "slow.lock", added: 1 }, "", "slow\n"),
        change({ newPath: "fast.rs", added: 1 }, "", "fast\n"),
      ],
    },
  };
  const bridge = fakeBridge(repos, diffs);
  const read = bridge.fileVersions.bind(bridge);
  vi.spyOn(bridge, "fileVersions").mockImplementation((tree, base, file) =>
    file.newPath === "slow.lock" ? new Promise(() => {}) : read(tree, base, file),
  );

  await renderDiff(bridge);

  expect(await within(await screen.findByRole("listitem", { name: "fast.rs" })).findByText("fast")).toBeTruthy();
  expect(within(section("slow.lock")).queryByLabelText("Modified")).toBeNull();
});

test("a file with no old side gets only added lines, with no removed one", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [
        change({ newPath: "new.rs", status: "Untracked", added: 2 }, null, "fn a() {}\nfn b() {}\n"),
        change({ newPath: "empty.rs", added: 1 }, "", "x\n"),
      ],
    },
  };

  await renderDiff(fakeBridge(repos, diffs));

  await waitFor(() => expect(within(section("new.rs")).getByLabelText("Modified").textContent).toBe("fn a() {}\nfn b() {}\n"));
  expect(within(section("new.rs")).queryByLabelText("Original")).toBeNull();
  // An empty old side is still a side, so its diff shows as one.
  expect(editorShows("empty.rs")).toEqual({ original: "", modified: "x\n" });
});

test("carriage returns reach the editor as a visible ␍, so a CRLF change shows", async () => {
  const diffs: FakeDiffs = {
    [worktree]: {
      Head: [change({ newPath: "a.txt", added: 2, removed: 2 }, "one\ntwo\n", "one\r\ntwo\r\n")],
    },
  };

  await renderDiff(fakeBridge(repos, diffs));

  await screen.findByLabelText("Modified");
  expect(editorShows("a.txt")).toEqual({ original: "one\ntwo\n", modified: "one␍\ntwo␍\n" });
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

test("collapsing a file's section hides its diff, and expanding it shows its comments and the one being typed", async () => {
  const { bridge } = await commentable();
  await bridge.addComment(worktree, { path: "src/lib.rs", startLine: 1, endLine: 1, body: "first" });
  await renderDiff(bridge);
  await screen.findByRole("group", { name: "Comment on line 1" });
  act(() => editors.commentLines!(3, 3));
  fireEvent.change(screen.getByRole("textbox"), { target: { value: "half typed" } });
  const header = within(section("src/lib.rs")).getByRole("button", { name: "src/lib.rs +3 -0" });

  fireEvent.click(header);

  expect(header.getAttribute("aria-expanded")).toBe("false");
  expect(screen.queryByRole("group", { name: "Comment on line 1" })).toBeNull();
  expect(screen.queryByRole("textbox")).toBeNull();

  fireEvent.click(header);

  expect(header.getAttribute("aria-expanded")).toBe("true");
  expect(within(under(1)).getByText("first")).toBeTruthy();
  expect((within(under(3)).getByRole("textbox") as HTMLTextAreaElement).value).toBe("half typed");
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
