import { useEffect, useRef } from "react";

import type { LineChange } from "./bridge";
import { editorOptions, holdCodeTheme, languageOf, monaco } from "./monaco";

export interface MonacoEditorProps {
  worktree: string;
  /** The open file to show, relative to the worktree, or null for none. */
  path: string | null;
  /** The file's text. New text from outside, such as a reload, replaces the editor's as one edit the user can undo. */
  text: string;
  /** Every open file. Each keeps its own model, with its undo history, until it closes. */
  open: readonly string[];
  /** Called with the whole text after each edit the user makes. */
  onChange(text: string): void;
  /** The shown file's lines changed since HEAD, marked in the gutter. */
  lineChanges: readonly LineChange[];
  /** A line from 1 that holds a problem, marked and scrolled into view. null or left out marks none. */
  problemLine?: number | null;
}

/** A Monaco editor showing one open file at a time. */
export function MonacoEditor({ worktree, path, text, open, onChange, lineChanges, problemLine }: MonacoEditorProps) {
  const host = useRef<HTMLDivElement>(null);
  const editor = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const gutter = useRef<monaco.editor.IEditorDecorationsCollection | null>(null);
  const problem = useRef<monaco.editor.IEditorDecorationsCollection | null>(null);
  // Each open file's model, and the text it last took from the panel or gave it, so only text
  // from outside is applied. A model can hold other text, since it keeps one line ending.
  const models = useRef(new Map<string, { model: monaco.editor.ITextModel; known: string }>());
  const views = useRef(new Map<string, monaco.editor.ICodeEditorViewState | null>());
  const shown = useRef<string | null>(null);
  // Set while text from outside goes in, so it does not come back as an edit.
  const applying = useRef(false);
  const onChangeRef = useRef(onChange);

  useEffect(() => {
    onChangeRef.current = onChange;
  });

  useEffect(() => {
    const releaseTheme = holdCodeTheme();
    const ed = monaco.editor.create(host.current!, { ...editorOptions(), automaticLayout: true, model: null });
    const typed = ed.onDidChangeModelContent(() => {
      const entry = shown.current === null ? undefined : models.current.get(shown.current);
      if (!entry || applying.current) return;
      entry.known = entry.model.getValue();
      onChangeRef.current(entry.known);
    });
    gutter.current = ed.createDecorationsCollection();
    problem.current = ed.createDecorationsCollection();
    editor.current = ed;
    const [all, viewStates] = [models.current, views.current];
    return () => {
      typed.dispose();
      ed.dispose();
      all.forEach(({ model }) => model.dispose());
      all.clear();
      viewStates.clear();
      editor.current = null;
      gutter.current = null;
      problem.current = null;
      shown.current = null;
      releaseTheme();
    };
  }, []);

  useEffect(() => {
    const ed = editor.current!;
    if (path === null) {
      if (shown.current !== null) views.current.set(shown.current, ed.saveViewState());
      ed.setModel(null);
      shown.current = null;
      return;
    }
    let entry = models.current.get(path);
    if (!entry) {
      const uri = monaco.Uri.file(`${worktree}/${path}`);
      entry = { model: monaco.editor.createModel(text, languageOf(path), uri), known: text };
      models.current.set(path, entry);
    } else if (entry.known !== text) {
      applying.current = true;
      const { model } = entry;
      model.pushEditOperations([], [{ range: model.getFullModelRange(), text }], () => null);
      applying.current = false;
      entry.known = text;
    }
    if (shown.current !== path) {
      if (shown.current !== null) views.current.set(shown.current, ed.saveViewState());
      ed.setModel(entry.model);
      ed.restoreViewState(views.current.get(path) ?? null);
      shown.current = path;
    }
  }, [worktree, path, text]);

  // A new model clears the gutter, so the marks go on again when the shown file changes.
  useEffect(() => {
    gutter.current?.set(lineChanges.map(decoration));
  }, [lineChanges, path]);

  useEffect(() => {
    if (problemLine == null) {
      problem.current?.clear();
      return;
    }
    problem.current?.set([
      {
        range: new monaco.Range(problemLine, 1, problemLine, 1),
        options: { isWholeLine: true, className: "problem-line", linesDecorationsClassName: "gutter-problem" },
      },
    ]);
    editor.current?.revealLineInCenterIfOutsideViewport(problemLine);
  }, [problemLine, path]);

  useEffect(() => {
    for (const [file, { model }] of models.current) {
      if (open.includes(file)) continue;
      model.dispose();
      models.current.delete(file);
      views.current.delete(file);
    }
  }, [open]);

  return <div className="monaco-editor-host" ref={host} />;
}

function decoration(change: LineChange): monaco.editor.IModelDeltaDecoration {
  if (change.kind === "Deleted") {
    const line = Math.max(change.after, 1);
    const above = change.after === 0 ? " above" : "";
    return {
      range: new monaco.Range(line, 1, line, 1),
      options: { linesDecorationsClassName: `gutter-deleted${above}` },
    };
  }
  return {
    range: new monaco.Range(change.start, 1, change.end, 1),
    options: { isWholeLine: true, linesDecorationsClassName: `gutter-${change.kind.toLowerCase()}` },
  };
}
