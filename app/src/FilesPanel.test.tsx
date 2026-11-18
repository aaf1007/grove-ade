import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import type { FileDiff, LineChange } from "./bridge";
import { fakeBridge, type FakeChange, type FakeFiles } from "./fakeBridge";
import { FilesPanel } from "./FilesPanel";
import type { MonacoEditorProps } from "./MonacoEditor";

// Monaco needs a real browser. The stub shows the text the panel hands it, takes typing, and
// lists the gutter marks.
vi.mock("./MonacoEditor", () => ({
  MonacoEditor: ({ path, text, onChange, lineChanges }: MonacoEditorProps) =>
    path === null ? null : (
      <div>
        <textarea
          aria-label={`Editor for ${path}`}
          value={text}
          onChange={(e) => onChange(e.target.value)}
        />
        <ul aria-label="Gutter marks">
          {lineChanges.map((change) => (
            <li key={JSON.stringify(change)}>{JSON.stringify(change)}</li>
          ))}
        </ul>
      </div>
    ),
}));

afterEach(cleanup);

const worktree = "/code/grove-wt/feat-124";

function change(newPath: string, status: FileDiff["status"], lines: LineChange[] = []): FakeChange {
  return {
    file: { status, oldPath: newPath, newPath, binary: false, added: 0, removed: 0 },
    versions: { old: null, new: null, binary: false },
    lines,
  };
}

function setup(files: FakeFiles[string], head: FakeChange[] = []) {
  const bridge = fakeBridge({}, { [worktree]: { Head: head } }, { [worktree]: { ...files } });
  render(<FilesPanel bridge={bridge} worktree={worktree} />);
  return bridge;
}

const tree = () => screen.getByRole("tree", { name: "Files" });
const item = (name: string) => within(tree()).findByRole("treeitem", { name });
const tabs = () => screen.getByRole("tablist", { name: "Open files" });
const editor = (path: string) => screen.findByRole<HTMLTextAreaElement>("textbox", { name: `Editor for ${path}` });

// Opens folders on the way to the file first.
async function open(path: string) {
  const parts = path.split("/");
  for (const folder of parts.slice(0, -1)) {
    const row = await item(folder);
    if (row.getAttribute("aria-expanded") === "false") fireEvent.click(row);
  }
  // A changed file's name ends in its mark.
  const name = parts[parts.length - 1].replaceAll(".", "\\.");
  fireEvent.click(await within(tree()).findByRole("treeitem", { name: new RegExp(`^${name}( [AM])?$`) }));
}

function type(path: string, text: string) {
  fireEvent.change(screen.getByRole("textbox", { name: `Editor for ${path}` }), {
    target: { value: text },
  });
}

function pressCmdS(path: string) {
  fireEvent.keyDown(screen.getByRole("textbox", { name: `Editor for ${path}` }), {
    key: "s",
    metaKey: true,
  });
}

// Lets the bridge's promises settle.
const settle = () => act(async () => {});

test("the tree shows the files from list_files, with A and M marks from the Head diff", async () => {
  setup(
    {
      "README.md": "# grove\n",
      "src/lib.rs": "",
      "src/new.rs": "",
      "docs/vision.md": "",
    },
    [change("src/lib.rs", "Modified"), change("src/new.rs", "Untracked"), change("README.md", "Added")],
  );

  // Folders come before files, and a folder with a changed file in it starts open.
  const rows = await within(tree()).findAllByRole("treeitem");
  expect(rows.map((row) => row.textContent)).toEqual([
    "docs",
    "src",
    "lib.rs M",
    "new.rs A",
    "README.md A",
  ]);
  expect((await item("src")).getAttribute("aria-expanded")).toBe("true");
  expect((await item("docs")).getAttribute("aria-expanded")).toBe("false");

  fireEvent.click(await item("docs"));

  expect(await item("vision.md")).toBeTruthy();
});

test("clicking a file opens it in a tab, once", async () => {
  const bridge = setup({ "src/lib.rs": "fn main() {}\n", "README.md": "# grove\n" });

  await open("src/lib.rs");
  await open("README.md");
  await open("src/lib.rs");

  expect((await editor("src/lib.rs")).value).toBe("fn main() {}\n");
  expect(bridge.readFiles).toEqual([
    { worktree, file: "src/lib.rs" },
    { worktree, file: "README.md" },
  ]);
  const open_ = within(tabs()).getAllByRole("tab");
  expect(open_.map((tab) => [tab.textContent, tab.getAttribute("aria-selected")])).toEqual([
    ["lib.rs", "true"],
    ["README.md", "false"],
  ]);
});

test("an edit marks the tab, and Cmd-S saves it through write_file", async () => {
  const bridge = setup({ "src/lib.rs": "fn main() {}\n" });
  await open("src/lib.rs");
  await editor("src/lib.rs");

  type("src/lib.rs", "fn main() { run() }\n");

  expect(within(tabs()).getByRole("tab", { name: "lib.rs Unsaved" })).toBeTruthy();

  pressCmdS("src/lib.rs");
  await settle();

  expect(bridge.writtenFiles).toEqual([{ worktree, file: "src/lib.rs", text: "fn main() { run() }\n" }]);
  expect(within(tabs()).getByRole("tab", { name: "lib.rs" })).toBeTruthy();
});

test("a save that fails says why and keeps the edit", async () => {
  const bridge = setup({ "src/lib.rs": "a\n" });
  bridge.writeFile = () => Promise.reject("could not replace src/lib.rs: Permission denied");
  await open("src/lib.rs");
  await editor("src/lib.rs");

  type("src/lib.rs", "b\n");
  pressCmdS("src/lib.rs");

  expect((await screen.findByRole("alert")).textContent).toContain("Permission denied");
  expect(within(tabs()).getByRole("tab", { name: "lib.rs Unsaved" })).toBeTruthy();
  // The edit stays on screen, to copy or save again.
  expect((await editor("src/lib.rs")).value).toBe("b\n");
});

test("Cmd-S on a file that could not be read writes nothing", async () => {
  const bridge = setup({ "a.txt": "a" });
  await item("a.txt");
  // An agent deletes it after the tree listed it.
  bridge.readFile = () => Promise.reject("could not read a.txt: No such file or directory");
  await open("a.txt");
  expect((await screen.findByRole("alert")).textContent).toContain("No such file");

  fireEvent.keyDown(within(tabs()).getByRole("tab", { name: "a.txt" }), { key: "s", metaKey: true });
  await settle();

  expect(bridge.writtenFiles).toEqual([]);
});

test("a UTF-8 byte order mark survives a save", async () => {
  const bridge = setup({ "a.txt": new Uint8Array([0xef, 0xbb, 0xbf, 0x61]) });
  await open("a.txt");
  await editor("a.txt");

  type("a.txt", "﻿ab");
  pressCmdS("a.txt");
  await settle();

  expect(bridge.writtenFiles.map((w) => w.text)).toEqual(["﻿ab"]);
});

test("closing an unsaved tab asks first", async () => {
  const bridge = setup({ "a.txt": "a", "b.txt": "b" });
  await open("a.txt");
  await open("b.txt");
  await editor("b.txt");
  type("b.txt", "b2");
  const close = () => fireEvent.click(within(tabs()).getByRole("button", { name: "Close b.txt" }));

  close();
  const ask = screen.getByRole("alertdialog", { name: "Unsaved changes" });
  expect(ask.textContent).toContain("b.txt has unsaved changes");
  fireEvent.click(within(ask).getByRole("button", { name: "Cancel" }));

  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(within(tabs()).getByRole("tab", { name: "b.txt Unsaved" })).toBeTruthy();

  close();
  fireEvent.click(screen.getByRole("button", { name: "Don't save" }));

  expect(within(tabs()).queryByRole("tab", { name: /b\.txt/ })).toBeNull();
  expect(bridge.writtenFiles).toEqual([]);
  // The tab beside it takes over.
  expect(within(tabs()).getByRole("tab", { name: "a.txt", selected: true })).toBeTruthy();
});

test("Save in the close question saves, then closes", async () => {
  const bridge = setup({ "a.txt": "a" });
  await open("a.txt");
  await editor("a.txt");
  type("a.txt", "a2");

  fireEvent.click(within(tabs()).getByRole("button", { name: "Close a.txt" }));
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await settle();

  expect(bridge.writtenFiles).toEqual([{ worktree, file: "a.txt", text: "a2" }]);
  expect(within(tabs()).queryAllByRole("tab")).toEqual([]);
});

test("edits made while the close question's Save writes keep the tab open", async () => {
  const bridge = setup({ "a.txt": "a" });
  let release = () => {};
  const write = bridge.writeFile;
  bridge.writeFile = async (wt, file, text) => {
    await new Promise<void>((resolve) => (release = resolve));
    await write(wt, file, text);
  };
  await open("a.txt");
  await editor("a.txt");
  type("a.txt", "a2");

  fireEvent.click(within(tabs()).getByRole("button", { name: "Close a.txt" }));
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await settle();
  type("a.txt", "a3");
  act(() => release());
  await settle();

  expect(bridge.writtenFiles.map((w) => w.text)).toEqual(["a2"]);
  expect(within(tabs()).getByRole("tab", { name: "a.txt Unsaved" })).toBeTruthy();
  expect((await editor("a.txt")).value).toBe("a3");
});

test("a tab picked while the close question's Save writes stays picked", async () => {
  const bridge = setup({ "a.txt": "a", "b.txt": "b", "c.txt": "c" });
  let release = () => {};
  const write = bridge.writeFile;
  bridge.writeFile = async (wt, file, text) => {
    await new Promise<void>((resolve) => (release = resolve));
    await write(wt, file, text);
  };
  await open("a.txt");
  await open("b.txt");
  await open("c.txt");
  fireEvent.click(within(tabs()).getByRole("tab", { name: "a.txt" }));
  type("a.txt", "a2");

  fireEvent.click(within(tabs()).getByRole("button", { name: "Close a.txt" }));
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await settle();
  fireEvent.click(within(tabs()).getByRole("tab", { name: "c.txt" }));
  act(() => release());
  await settle();

  expect(within(tabs()).getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["b.txt", "c.txt"]);
  expect(within(tabs()).getByRole("tab", { selected: true }).textContent).toBe("c.txt");
});

test("a tab picked while its close question's Save writes gives way when it closes", async () => {
  const bridge = setup({ "a.txt": "a", "b.txt": "b" });
  let release = () => {};
  const write = bridge.writeFile;
  bridge.writeFile = async (wt, file, text) => {
    await new Promise<void>((resolve) => (release = resolve));
    await write(wt, file, text);
  };
  await open("a.txt");
  type("a.txt", "a2");
  await open("b.txt");

  fireEvent.click(within(tabs()).getByRole("button", { name: "Close a.txt" }));
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await settle();
  fireEvent.click(within(tabs()).getByRole("tab", { name: "a.txt Unsaved" }));
  act(() => release());
  await settle();

  expect(within(tabs()).getByRole("tab", { selected: true }).textContent).toBe("b.txt");
  expect(await editor("b.txt")).toBeTruthy();
});

test("a clean tab closes without asking", async () => {
  setup({ "a.txt": "a" });
  await open("a.txt");

  fireEvent.click(within(tabs()).getByRole("button", { name: "Close a.txt" }));

  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(within(tabs()).queryAllByRole("tab")).toEqual([]);
});

test("a file changed on disk while open and unedited reloads", async () => {
  const bridge = setup({ "a.txt": "one\n" });
  await open("a.txt");
  await editor("a.txt");

  bridge.setFile(worktree, "a.txt", "two\n");
  act(() => bridge.diffChanged(worktree));
  await settle();

  expect((await editor("a.txt")).value).toBe("two\n");
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a file changed on disk while edited offers a choice", async () => {
  const bridge = setup({ "a.txt": "one\n" });
  await open("a.txt");
  await editor("a.txt");
  type("a.txt", "mine\n");

  bridge.setFile(worktree, "a.txt", "theirs\n");
  act(() => bridge.diffChanged(worktree));
  const conflict = await screen.findByRole("alert");

  expect(conflict.textContent).toContain("a.txt changed on disk");
  expect((await editor("a.txt")).value).toBe("mine\n");

  fireEvent.click(within(conflict).getByRole("button", { name: "Load the disk version" }));

  expect((await editor("a.txt")).value).toBe("theirs\n");
  expect(within(tabs()).getByRole("tab", { name: "a.txt" })).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("keeping my edits over a disk change saves them over it", async () => {
  const bridge = setup({ "a.txt": "one\n" });
  await open("a.txt");
  await editor("a.txt");
  type("a.txt", "mine\n");
  bridge.setFile(worktree, "a.txt", "theirs\n");
  act(() => bridge.diffChanged(worktree));

  fireEvent.click(await screen.findByRole("button", { name: "Keep my edits" }));

  expect(screen.queryByRole("alert")).toBeNull();
  expect(within(tabs()).getByRole("tab", { name: "a.txt Unsaved" })).toBeTruthy();
  // The same disk change does not ask again.
  act(() => bridge.diffChanged(worktree));
  await settle();
  expect(screen.queryByRole("alert")).toBeNull();

  pressCmdS("a.txt");
  await settle();
  expect(bridge.writtenFiles.map((w) => w.text)).toEqual(["mine\n"]);
});

test("a disk change that goes back to the saved text drops the choice", async () => {
  const bridge = setup({ "a.txt": "one\n" });
  await open("a.txt");
  await editor("a.txt");
  type("a.txt", "mine\n");
  bridge.setFile(worktree, "a.txt", "theirs\n");
  act(() => bridge.diffChanged(worktree));
  await screen.findByRole("alert");

  // Such as a git stash, then a stash pop of nothing.
  bridge.setFile(worktree, "a.txt", "one\n");
  act(() => bridge.diffChanged(worktree));
  await settle();

  expect(screen.queryByRole("alert")).toBeNull();
  expect((await editor("a.txt")).value).toBe("mine\n");
  expect(within(tabs()).getByRole("tab", { name: "a.txt Unsaved" })).toBeTruthy();
});

test("a file that turns binary on disk while edited keeps the edits until the user picks", async () => {
  const bridge = setup({ "a.txt": "one\n" });
  await open("a.txt");
  await editor("a.txt");
  type("a.txt", "mine\n");

  bridge.setFile(worktree, "a.txt", new Uint8Array([0x89, 0x50, 0, 1]));
  act(() => bridge.diffChanged(worktree));
  const conflict = await screen.findByRole("alert");

  expect(conflict.textContent).toContain("a.txt changed on disk");
  expect((await editor("a.txt")).value).toBe("mine\n");
  expect(within(tabs()).getByRole("tab", { name: "a.txt Unsaved" })).toBeTruthy();

  fireEvent.click(within(conflict).getByRole("button", { name: "Load the disk version" }));

  expect(await screen.findByText("Binary file, not shown.")).toBeTruthy();
  expect(within(tabs()).getByRole("tab", { name: "a.txt" })).toBeTruthy();
});

test("keeping my edits over a file that turned binary saves them over it", async () => {
  const bridge = setup({ "a.txt": "one\n" });
  await open("a.txt");
  await editor("a.txt");
  type("a.txt", "mine\n");
  bridge.setFile(worktree, "a.txt", new Uint8Array([0, 1]));
  act(() => bridge.diffChanged(worktree));

  fireEvent.click(await screen.findByRole("button", { name: "Keep my edits" }));
  act(() => bridge.diffChanged(worktree));
  await settle();

  expect(screen.queryByRole("alert")).toBeNull();
  pressCmdS("a.txt");
  await settle();
  expect(bridge.writtenFiles.map((w) => w.text)).toEqual(["mine\n"]);
});

test("saves of one file write in the order they were asked for", async () => {
  const bridge = setup({ "a.txt": "a" });
  const steps: string[] = [];
  let finishFirst = () => {};
  const write = bridge.writeFile;
  bridge.writeFile = async (wt, file, text) => {
    steps.push(`start ${text}`);
    if (steps.length === 1) await new Promise<void>((resolve) => (finishFirst = resolve));
    await write(wt, file, text);
    steps.push(`end ${text}`);
  };
  await open("a.txt");
  await editor("a.txt");

  type("a.txt", "one");
  pressCmdS("a.txt");
  type("a.txt", "two");
  pressCmdS("a.txt");
  await settle();
  act(() => finishFirst());
  await settle();

  expect(steps).toEqual(["start one", "end one", "start two", "end two"]);
  expect(within(tabs()).getByRole("tab", { name: "a.txt" })).toBeTruthy();
});

test("a reload that read the disk before my save does not undo it", async () => {
  const bridge = setup({ "a.txt": "old\n" });
  await open("a.txt");
  await editor("a.txt");
  // The next read returns what was on disk when it started, but only when released.
  let release = () => {};
  const read = bridge.readFile;
  bridge.readFile = async (wt, file) => {
    const before = await read(wt, file);
    await new Promise<void>((resolve) => (release = resolve));
    return before;
  };
  act(() => bridge.diffChanged(worktree));
  bridge.readFile = read;

  type("a.txt", "new\n");
  pressCmdS("a.txt");
  await settle();
  act(() => release());
  await settle();

  expect((await editor("a.txt")).value).toBe("new\n");
  expect(within(tabs()).getByRole("tab", { name: "a.txt" })).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("my own save does not count as a change on disk", async () => {
  const bridge = setup({ "a.txt": "one\n" });
  await open("a.txt");
  await editor("a.txt");
  type("a.txt", "two\n");
  pressCmdS("a.txt");
  await settle();

  act(() => bridge.diffChanged(worktree));
  await settle();

  expect(screen.queryByRole("alert")).toBeNull();
  expect(within(tabs()).getByRole("tab", { name: "a.txt" })).toBeTruthy();
});

test("the gutter marks are the Head diff's line changes for the open file, kept current", async () => {
  const head = [change("src/lib.rs", "Modified", [{ kind: "Modified", start: 2, end: 3 }])];
  const bridge = setup({ "src/lib.rs": "a\nb\nc\n", "README.md": "" }, head);
  const marks = async () =>
    within(await screen.findByRole("list", { name: "Gutter marks" }))
      .queryAllByRole("listitem")
      .map((mark) => JSON.parse(mark.textContent!) as LineChange);

  await open("src/lib.rs");
  await editor("src/lib.rs");
  await settle();
  expect(await marks()).toEqual([{ kind: "Modified", start: 2, end: 3 }]);

  head[0].lines = [{ kind: "Added", start: 4, end: 4 }, { kind: "Deleted", after: 0 }];
  act(() => bridge.diffChanged(worktree));
  await settle();
  expect(await marks()).toEqual([
    { kind: "Added", start: 4, end: 4 },
    { kind: "Deleted", after: 0 },
  ]);

  await open("README.md");
  await editor("README.md");
  await settle();
  expect(await marks()).toEqual([]);
});

test("a file that is not text shows as binary, outside the editor", async () => {
  setup({ "logo.png": new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0, 1]), "latin1.txt": new Uint8Array([0xe9]) });

  await open("logo.png");

  expect(await screen.findByText("Binary file, not shown.")).toBeTruthy();
  expect(screen.queryByRole("textbox")).toBeNull();

  await open("latin1.txt");

  expect(await screen.findByText("Binary file, not shown.")).toBeTruthy();
});

test("a new file on disk shows in the tree after the diff changes", async () => {
  const bridge = setup({ "a.txt": "" });
  await item("a.txt");

  bridge.setFile(worktree, "b.txt", "");
  act(() => bridge.diffChanged(worktree));

  expect(await item("b.txt")).toBeTruthy();
});
