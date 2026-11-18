import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { MonacoDiff } from "./MonacoDiff";
import { MonacoEditor } from "./MonacoEditor";
import { monaco } from "./monaco";
import { setAppearance, tokensInDocument } from "./tokensInDocument";

let removeTokens = () => {};

beforeEach(() => {
  // Monaco reads the OS's contrast setting, and jsdom has no matchMedia.
  vi.stubGlobal("matchMedia", (media: string) => ({
    matches: false,
    media,
    addEventListener() {},
    removeEventListener() {},
  }));
  // jsdom has no ResizeObserver, which Monaco's automatic layout uses.
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  // jsdom has no canvas. Monaco paints its overview ruler on one.
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue(
    new Proxy({}, { get: () => () => {}, set: () => true }) as unknown as CanvasRenderingContext2D,
  );
  removeTokens = tokensInDocument({ theme: "dark", palette: "default" });
});

afterEach(() => {
  cleanup();
  removeTokens();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function editorProps() {
  return { worktree: "/code/grove", path: "src/main.ts", text: "const a = 1;\n", open: ["src/main.ts"], onChange() {}, lineChanges: [] };
}

function token(name: string) {
  return getComputedStyle(document.documentElement).getPropertyValue(`--${name}`).trim();
}

// The ground Monaco paints. Its theme sets each color as a CSS variable on the editor.
function ground(container: HTMLElement) {
  return getComputedStyle(container.querySelector(".monaco-editor")!).getPropertyValue("--vscode-editor-background");
}

test("an open editor takes the panel's ground and recolors when the theme and the palette change", async () => {
  const { container } = render(<MonacoEditor {...editorProps()} />);
  expect(ground(container)).toBe(token("panel"));

  setAppearance("light", "default");
  await vi.waitFor(() => expect(ground(container)).toBe(token("panel")));
  const lightDefault = ground(container);

  setAppearance("light", "modern");
  await vi.waitFor(() => expect(ground(container)).toBe(token("panel")));
  expect(ground(container)).not.toBe(lightDefault);
});

test("the editor and both sides of the diff use the code font, no minimap, thin scrollbars and no ligatures", () => {
  render(<MonacoEditor {...editorProps()} />);
  render(<MonacoDiff original="a" modified="b" path="a.ts" onCommentLines={() => {}} zones={[]} />);
  const [diff] = monaco.editor.getDiffEditors();
  const editors = [
    monaco.editor.getEditors().find((editor) => editor.getModel()?.uri.path === "/code/grove/src/main.ts")!,
    diff.getOriginalEditor(),
    diff.getModifiedEditor(),
  ];
  const { EditorOption } = monaco.editor;

  for (const editor of editors) {
    expect(editor.getOption(EditorOption.fontFamily)).toBe(token("font-mono"));
    expect(editor.getOption(EditorOption.fontSize)).toBe(13);
    expect(editor.getOption(EditorOption.lineHeight)).toBe(18);
    expect(editor.getOption(EditorOption.minimap).enabled).toBe(false);
    expect(editor.getOption(EditorOption.scrollbar)).toMatchObject({ verticalScrollbarSize: 6, horizontalScrollbarSize: 6 });
    expect(editor.getOption(EditorOption.padding).top).toBe(8);
    expect(editor.getOption(EditorOption.fontLigatures)).toBe('"liga" off, "calt" off');
  }
});

test("the diff shows inline in a narrow panel and side by side in a wide one", () => {
  render(<MonacoDiff original={"a\nb\n"} modified={"a\nc\n"} path="a.ts" onCommentLines={() => {}} zones={[]} />);
  const [diff] = monaco.editor.getDiffEditors();
  const widths = () => [diff.getOriginalEditor(), diff.getModifiedEditor()].map((e) => e.getLayoutInfo().width);

  diff.layout({ width: 901, height: 300 });
  const [originalWide, modifiedWide] = widths();
  expect(Math.abs(originalWide - modifiedWide)).toBeLessThanOrEqual(1);

  diff.layout({ width: 899, height: 300 });
  const [originalNarrow, modifiedNarrow] = widths();
  // Inline: the original side keeps only its line numbers.
  expect(originalNarrow).toBeLessThan(50);
  expect(modifiedNarrow).toBeGreaterThan(800);
});

test("an editor still opens with stock colors when the tokens cannot be read, and says why", () => {
  const errors = vi.spyOn(console, "error").mockImplementation(() => {});
  removeTokens();

  render(<MonacoEditor {...editorProps()} />);

  expect(monaco.editor.getEditors().some((editor) => editor.getModel()?.uri.path === "/code/grove/src/main.ts")).toBe(true);
  expect(String(errors.mock.calls[0])).toContain("--");
});

test("an open editor takes the stock colors when the tokens cannot be read after a change", async () => {
  vi.spyOn(console, "error").mockImplementation(() => {});
  const { container } = render(<MonacoEditor {...editorProps()} />);
  expect(ground(container)).toBe(token("panel"));
  removeTokens();

  setAppearance("light", "default");

  // The stock light theme's ground.
  await vi.waitFor(() => expect(ground(container)).toBe("#fffffe"));
});
