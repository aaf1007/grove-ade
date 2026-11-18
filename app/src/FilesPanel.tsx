import { useEffect, useId, useState, useSyncExternalStore } from "react";

import type { Bridge, FileStatus, LineChange } from "./bridge";
import { MonacoEditor } from "./MonacoEditor";
import { isEdited, OpenFiles } from "./openFiles";
import { Button } from "./ui/Button";

type Mark = "A" | "M";

const MARKS: Partial<Record<FileStatus, Mark>> = {
  Added: "A",
  Untracked: "A",
  Modified: "M",
  Renamed: "M",
};

const MARK_NAMES: Record<Mark, string> = { A: "Added", M: "Modified" };

/**
 * One worktree's Files panel: editor tabs on the left, the file tree on the right. Files open in
 * tabs, edits save with Cmd-S, and lines changed since HEAD get gutter marks.
 */
export function FilesPanel({ bridge, worktree }: { bridge: Bridge; worktree: string }) {
  const [model] = useState(() => new OpenFiles(bridge, worktree));
  const files = useSyncExternalStore(model.subscribe, model.list);
  const [paths, setPaths] = useState<{ list: string[]; error?: string }>({ list: [] });
  const [marks, setMarks] = useState(new Map<string, Mark>());
  // Folders the user opened or closed. Others are open when they hold a changed file.
  const [folders, setFolders] = useState(new Map<string, boolean>());
  // Bumped on each DiffChanged for this worktree, to load the tree and the gutter again.
  const [changes, setChanges] = useState(0);
  const [active, setActive] = useState<string | null>(null);
  // The edited file whose close waits on the user's answer.
  const [closing, setClosing] = useState<string | null>(null);
  const [gutter, setGutter] = useState<{ path: string; lines: LineChange[] } | null>(null);

  useEffect(
    () =>
      bridge.onDiffChanged((changed) => {
        if (changed !== worktree) return;
        setChanges((n) => n + 1);
        void model.reload();
      }),
    [bridge, worktree, model],
  );

  useEffect(() => {
    // A later load replaces this one, so this one drops its answer.
    let current = true;
    bridge.listFiles(worktree).then(
      (list) => current && setPaths({ list }),
      (e: unknown) => current && setPaths({ list: [], error: String(e) }),
    );
    // Without the diff the tree only lacks its marks.
    bridge.diff(worktree, "Head").then(
      (diff) => {
        if (!current) return;
        const found = new Map<string, Mark>();
        for (const file of diff) {
          const mark = MARKS[file.status];
          if (mark) found.set(file.newPath, mark);
        }
        setMarks(found);
      },
      () => {},
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, changes]);

  useEffect(() => {
    if (active === null) return;
    let current = true;
    // Without them the gutter stays empty.
    bridge.lineChanges(worktree, active).then(
      (lines) => current && setGutter({ path: active, lines }),
      () => current && setGutter({ path: active, lines: [] }),
    );
    return () => {
      current = false;
    };
  }, [bridge, worktree, active, changes]);

  const ids = useId();
  const tabId = (path: string) => `${ids}-tab-${files.findIndex((file) => file.path === path)}`;
  const activeFile = files.find((file) => file.path === active);
  // A file that could not be read shows only why. One with edits stays, so a failed save
  // keeps them on screen.
  const editable =
    activeFile && !activeFile.binary && (!activeFile.error || isEdited(activeFile));

  function open(path: string) {
    setActive(path);
    void model.open(path);
  }

  // Reads the tabs as they are now, since a close can wait on a save.
  function close(path: string) {
    const open = model.list();
    const at = open.findIndex((file) => file.path === path);
    const next = (open[at + 1] ?? open[at - 1])?.path ?? null;
    setActive((shown) => (shown === path ? next : shown));
    model.close(path);
  }

  function askToClose(path: string) {
    const file = model.get(path);
    if (file && isEdited(file)) setClosing(path);
    else close(path);
  }

  function answerClose(answer: "Save" | "Don't save" | "Cancel") {
    const path = closing!;
    setClosing(null);
    if (answer === "Don't save") close(path);
    // A failed save shows on the file, which stays open. So does one edited again meanwhile.
    if (answer !== "Save") return;
    model.save(path).then(
      () => {
        const file = model.get(path);
        if (file && !isEdited(file)) close(path);
      },
      () => {},
    );
  }

  return (
    <div
      className="files"
      onKeyDown={(e) => {
        if (!e.metaKey || e.key.toLowerCase() !== "s") return;
        e.preventDefault();
        if (active !== null) model.save(active).catch(() => {});
      }}
    >
      <div className="editor">
        <div className="ed-tabs" role="tablist" aria-label="Open files">
          {files.map((file) => (
            <div key={file.path} className="ed-tab" role="presentation">
              <button
                role="tab"
                id={tabId(file.path)}
                aria-selected={file.path === active}
                aria-controls={file.path === active ? `${ids}-panel` : undefined}
                title={file.path}
                onClick={() => setActive(file.path)}
              >
                {baseName(file.path)}
                {isEdited(file) && (
                  <>
                    {" "}
                    <span className="dirty" title="Unsaved">
                      <span className="visually-hidden">Unsaved</span>
                    </span>
                  </>
                )}
              </button>
              <button
                className="close"
                aria-label={`Close ${baseName(file.path)}`}
                title={`Close ${file.path}`}
                onClick={() => askToClose(file.path)}
              >
                ×
              </button>
            </div>
          ))}
        </div>
        <div
          className="ed-panel"
          id={`${ids}-panel`}
          role={activeFile ? "tabpanel" : undefined}
          aria-labelledby={activeFile ? tabId(activeFile.path) : undefined}
        >
          {closing !== null && (
            <div className="ed-notice" role="alertdialog" aria-label="Unsaved changes">
              <span>{baseName(closing)} has unsaved changes.</span>
              {(["Save", "Don't save", "Cancel"] as const).map((answer) => (
                <Button key={answer} size="xs" onClick={() => answerClose(answer)}>
                  {answer}
                </Button>
              ))}
            </div>
          )}
          {activeFile && (
            <div className="ed-path">
              <span>{activeFile.path}</span>
              {isEdited(activeFile) && <span>Edited · ⌘S saves</span>}
            </div>
          )}
          {activeFile?.changedOnDisk != null && (
            <div className="ed-notice" role="alert">
              <span>{activeFile.path} changed on disk.</span>
              <Button size="xs" onClick={() => model.takeDisk(activeFile.path)}>
                Load the disk version
              </Button>
              <Button size="xs" onClick={() => model.keepEdits(activeFile.path)}>
                Keep my edits
              </Button>
            </div>
          )}
          {activeFile?.error && (
            <p role="alert" className="ed-error">
              {activeFile.error}
            </p>
          )}
          {activeFile?.binary && <p className="panel-empty">Binary file, not shown.</p>}
          {files.length === 0 && <p className="panel-empty">Open a file from the tree.</p>}
          {/* Mounted while the panel is, so each file keeps its undo history between tabs. */}
          <div className="ed-body" hidden={!editable}>
            <MonacoEditor
              worktree={worktree}
              path={editable ? activeFile.path : null}
              text={editable ? activeFile.text : ""}
              open={files.map((file) => file.path)}
              onChange={(text) => active !== null && model.edit(active, text)}
              lineChanges={gutter && gutter.path === active ? gutter.lines : NO_LINES}
            />
          </div>
        </div>
      </div>
      <div className="tree" role="tree" aria-label="Files">
        {paths.error && <p role="alert">{paths.error}</p>}
        {rows(paths.list, marks, folders).map((row) =>
          row.kind === "folder" ? (
            <button
              key={`${row.path}/`}
              className="tree-row folder"
              role="treeitem"
              aria-level={row.depth + 1}
              aria-expanded={row.open}
              title={row.path}
              style={{ paddingLeft: 6 + row.depth * 12 }}
              onClick={() => setFolders((open) => new Map(open).set(row.path, !row.open))}
            >
              {row.name}
            </button>
          ) : (
            <button
              key={row.path}
              className="tree-row"
              role="treeitem"
              aria-level={row.depth + 1}
              aria-current={row.path === active}
              title={row.path}
              style={{ paddingLeft: 18 + row.depth * 12 }}
              onClick={() => open(row.path)}
            >
              {row.name}
              {row.mark && (
                <>
                  {" "}
                  <span className={`mark ${row.mark}`} title={MARK_NAMES[row.mark]}>
                    {row.mark}
                  </span>
                </>
              )}
            </button>
          ),
        )}
      </div>
    </div>
  );
}

const NO_LINES: LineChange[] = [];

const baseName = (path: string) => path.slice(path.lastIndexOf("/") + 1);

type Row =
  | { kind: "folder"; path: string; name: string; depth: number; open: boolean }
  | { kind: "file"; path: string; name: string; depth: number; mark?: Mark };

interface Folder {
  folders: Map<string, Folder>;
  files: string[];
  /** It holds a changed file, at any depth. */
  changed: boolean;
}

/** The tree's visible rows: in each folder its folders, then its files, each sorted by name. */
function rows(paths: string[], marks: Map<string, Mark>, opened: Map<string, boolean>): Row[] {
  const root: Folder = { folders: new Map(), files: [], changed: false };
  for (const path of paths) {
    const parts = path.split("/");
    let folder = root;
    for (const part of parts.slice(0, -1)) {
      let next = folder.folders.get(part);
      if (!next) {
        next = { folders: new Map(), files: [], changed: false };
        folder.folders.set(part, next);
      }
      next.changed ||= marks.has(path);
      folder = next;
    }
    folder.files.push(path);
  }

  const out: Row[] = [];
  const byName = (a: string, b: string) => a.localeCompare(b);
  function walk(folder: Folder, prefix: string, depth: number) {
    for (const name of [...folder.folders.keys()].sort(byName)) {
      const child = folder.folders.get(name)!;
      const path = prefix + name;
      const open = opened.get(path) ?? child.changed;
      out.push({ kind: "folder", path, name, depth, open });
      if (open) walk(child, `${path}/`, depth + 1);
    }
    const files = folder.files.map((path) => ({ path, name: baseName(path) }));
    for (const { path, name } of files.sort((a, b) => byName(a.name, b.name))) {
      out.push({ kind: "file", path, name, depth, mark: marks.get(path) });
    }
  }
  walk(root, "", 0);
  return out;
}
