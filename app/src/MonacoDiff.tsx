import { type ReactNode, type RefObject, useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { editorOptions, holdCodeTheme, languageOf, monaco } from "./monaco";

/** A box shown between lines of the modified side, such as a review comment. */
export interface DiffZone {
  /** Stays the same while the box is shown, so what it holds keeps its state. */
  key: string;
  /** The 1-based line of the modified side it sits under. */
  afterLine: number;
  content: ReactNode;
}

export interface MonacoDiffProps {
  original: string;
  modified: string;
  /** The file's path, for its language. */
  path: string;
  /**
   * Called with the first and last 1-based line of the modified side when the user clicks its line
   * numbers, drags over them, or shift-clicks one to extend the selection.
   */
  onCommentLines(start: number, end: number): void;
  zones: DiffZone[];
}

// Narrower than this, in pixels, the diff shows inline. Wider, each side gets about 50 columns of code.
const SIDE_BY_SIDE_MIN_WIDTH = 900;

// A zone's height before its box is measured.
const UNMEASURED_ZONE_HEIGHT = 32;

/** A read-only Monaco diff of two versions of a file, with boxes between its lines. */
export function MonacoDiff({ original, modified, path, onCommentLines, zones }: MonacoDiffProps) {
  const host = useRef<HTMLDivElement>(null);
  const editor = useRef<monaco.editor.IStandaloneDiffEditor | null>(null);
  const shownPath = useRef<string | null>(null);
  const onCommentLinesRef = useRef(onCommentLines);
  const zonesRef = useRef(zones);
  // Each box's DOM node by key, which its ZoneBox puts there, and its last measured height.
  const boxes = useRef(new Map<string, HTMLElement>());
  const heights = useRef(new Map<string, number>());
  // Zones are added again only when one comes, goes or moves, not when a box's content changes.
  const placed = zones.map((zone) => `${zone.key}@${zone.afterLine}`).join("\n");

  useEffect(() => {
    onCommentLinesRef.current = onCommentLines;
    zonesRef.current = zones;
  });

  useEffect(() => {
    const releaseTheme = holdCodeTheme();
    const diff = monaco.editor.createDiffEditor(host.current!, {
      ...editorOptions(),
      readOnly: true,
      originalEditable: false,
      // Whitespace-only changes must show.
      ignoreTrimWhitespace: false,
      useInlineViewWhenSpaceIsLimited: true,
      renderSideBySideInlineBreakpoint: SIDE_BY_SIDE_MIN_WIDTH,
      // Its only action, Revert Block, needs an editable side. Left on, its menu throws after the diff is disposed.
      renderGutterMenu: false,
      automaticLayout: true,
    });
    const modifiedEditor = diff.getModifiedEditor();
    // A press on the line numbers selects lines, and the selection is final once the button is up,
    // wherever it is by then.
    let onLineNumbers = false;
    const down = modifiedEditor.onMouseDown((e) => {
      onLineNumbers = e.event.leftButton && e.target.type === monaco.editor.MouseTargetType.GUTTER_LINE_NUMBERS;
    });
    function up() {
      if (!onLineNumbers) return;
      onLineNumbers = false;
      const selection = modifiedEditor.getSelection();
      if (!selection) return;
      const { startLineNumber: start, endLineNumber, endColumn } = selection;
      // A whole line's selection ends at the start of the next one.
      const end = endLineNumber > start && endColumn === 1 ? endLineNumber - 1 : endLineNumber;
      onCommentLinesRef.current(start, end);
    }
    window.addEventListener("mouseup", up);
    editor.current = diff;
    return () => {
      window.removeEventListener("mouseup", up);
      down.dispose();
      const models = diff.getModel();
      diff.dispose();
      models?.original.dispose();
      models?.modified.dispose();
      editor.current = null;
      shownPath.current = null;
      releaseTheme();
    };
  }, []);

  useEffect(() => {
    const diff = editor.current;
    if (!diff) return;
    const models = diff.getModel();
    // The same file takes its new text in place, so scroll and cursor stay where they were.
    if (models && shownPath.current === path) {
      const view = diff.saveViewState();
      if (models.original.getValue() !== original) models.original.setValue(original);
      if (models.modified.getValue() !== modified) models.modified.setValue(modified);
      diff.restoreViewState(view);
      return;
    }
    const language = languageOf(path);
    diff.setModel({
      original: monaco.editor.createModel(original, language),
      modified: monaco.editor.createModel(modified, language),
    });
    models?.original.dispose();
    models?.modified.dispose();
    shownPath.current = path;
    const computed = diff.onDidUpdateDiff(() => {
      computed.dispose();
      diff.revealFirstDiff();
    });
  }, [original, modified, path]);

  // A new model or new text drops or shifts the view zones, so they are all added again after it.
  useEffect(() => {
    const modifiedEditor = editor.current?.getModifiedEditor();
    if (!modifiedEditor) return;
    const added: { id: string; zone: monaco.editor.IViewZone; box: HTMLElement; key: string }[] = [];
    modifiedEditor.changeViewZones((zones) => {
      for (const { key, afterLine } of zonesRef.current) {
        const box = boxes.current.get(key);
        if (!box) continue;
        // Monaco sizes and places the zone's node, and the box inside keeps its own height.
        const domNode = document.createElement("div");
        domNode.className = "diff-zone";
        domNode.append(box);
        const zone = { afterLineNumber: afterLine, heightInPx: heights.current.get(key) ?? UNMEASURED_ZONE_HEIGHT, domNode };
        added.push({ id: zones.addZone(zone), zone, box, key });
      }
    });
    const sizes = new ResizeObserver(() => {
      modifiedEditor.changeViewZones((zones) => {
        for (const { id, zone, box, key } of added) {
          // A box not on screen has no height, and keeps the one it had.
          if (box.offsetHeight === 0 || box.offsetHeight === zone.heightInPx) continue;
          zone.heightInPx = box.offsetHeight;
          heights.current.set(key, box.offsetHeight);
          zones.layoutZone(id);
        }
      });
    });
    added.forEach(({ box }) => sizes.observe(box));
    return () => {
      sizes.disconnect();
      modifiedEditor.changeViewZones((zones) => added.forEach(({ id }) => zones.removeZone(id)));
    };
  }, [placed, original, modified, path]);

  return (
    <div className="monaco-diff" ref={host}>
      {zones.map((zone) => (
        <ZoneBox key={zone.key} id={zone.key} boxes={boxes}>
          {zone.content}
        </ZoneBox>
      ))}
    </div>
  );
}

/** A zone's content, drawn into a node of its own that MonacoDiff hands to Monaco. */
function ZoneBox({
  id,
  boxes,
  children,
}: {
  id: string;
  boxes: RefObject<Map<string, HTMLElement>>;
  children: ReactNode;
}) {
  const [box] = useState(() => {
    const node = document.createElement("div");
    node.className = "diff-zone-box";
    // Keys typed in the box stay out of Monaco's keybindings, so Cmd-F does not open its find.
    // React still gets them: it listens on each portal's own node.
    node.addEventListener("keydown", (event) => event.stopPropagation());
    return node;
  });

  // Before MonacoDiff's effects, so its zones find the box.
  useLayoutEffect(() => {
    const all = boxes.current;
    all.set(id, box);
    return () => {
      all.delete(id);
    };
  }, [boxes, id, box]);

  return createPortal(children, box);
}
