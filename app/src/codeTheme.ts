import type { ITheme } from "@xterm/xterm";
import type * as monaco from "monaco-editor/editor";

// The editor, the diff and the terminal are drawn outside CSS, so they read the tokens from the
// root element as the window computes them for the current theme and palette.

/** A token's value on the root element, with references to other tokens followed. */
export function readToken(name: string): string {
  const value = getComputedStyle(document.documentElement).getPropertyValue(`--${name}`).trim();
  if (!value) throw new Error(`no --${name} token on the root element`);
  const reference = /^var\(--([\w-]+)\)$/.exec(value);
  return reference ? readToken(reference[1]) : value;
}

/** A color token as #rrggbb, or #rrggbbaa when it is translucent. Monaco takes hex only. */
export function colorToken(name: string): string {
  const value = readToken(name);
  // The production build minifies colors, to short hex, 8-digit hex or a name such as green. So the
  // window reads each one, and gives it back in its rgb or rgba form.
  const probe = document.createElement("span");
  probe.style.color = value;
  if (!probe.style.color) throw new Error(`--${name} is not a color: ${value}`);
  document.documentElement.append(probe);
  const computed = getComputedStyle(probe).color;
  probe.remove();
  const channels = /^rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*(?:,\s*([\d.]+)\s*)?\)$/.exec(computed);
  if (!channels) throw new Error(`--${name} computes to a color this cannot read: ${computed}`);
  const [red, green, blue] = [1, 2, 3].map((index) => Number(channels[index]));
  const alpha = channels[4] === undefined ? 1 : Number(channels[4]);
  const bytes = alpha === 1 ? [red, green, blue] : [red, green, blue, Math.round(alpha * 255)];
  return `#${bytes.map((byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}

/**
 * The theme `build` makes, or undefined when a token cannot be read. The error goes to the console,
 * and the editor or terminal keeps its stock colors: a wrong color must not take down the window.
 */
export function buildOrReport<T>(build: () => T): T | undefined {
  try {
    return build();
  } catch (error) {
    console.error("grove could not build the code theme from the tokens:", error);
    return undefined;
  }
}

/** The code text style: the mono font at the code size, with the line height in pixels. */
export function codeFont() {
  const fontSize = parseFloat(readToken("text-code-size"));
  // The shorthand's line height, after the slash: a multiple of the size, or pixels.
  const lineHeight = /\/\s*([\d.]+)(px)?/.exec(readToken("text-code"));
  if (!lineHeight) throw new Error("--text-code has no line height");
  const pixels = lineHeight[2] ? Number(lineHeight[1]) : Math.round(fontSize * Number(lineHeight[1]));
  return { fontFamily: readToken("font-mono"), fontSize, lineHeight: pixels };
}

// Monaco's token names, as its languages emit them, and the syntax token that colors each. A more
// specific name wins over its prefix. A name with no rule takes the editor's text color.
const SYNTAX: [string, string][] = [
  ["keyword", "syn-kw"],
  ["predefined", "syn-fn"],
  ["annotation", "syn-fn"],
  ["type", "syn-type"],
  ["namespace", "syn-type"],
  ["string", "syn-str"],
  ["attribute.value", "syn-str"],
  ["number", "syn-num"],
  ["constant", "syn-const"],
  ["variable.predefined", "syn-const"],
  ["string.escape", "syn-const"],
  ["attribute.name", "syn-prop"],
  ["key", "syn-prop"],
  ["string.key", "syn-prop"],
  ["tag", "syn-tag"],
  ["metatag", "syn-tag"],
  ["regexp", "syn-tag"],
  ["comment", "syn-com"],
  ["invalid", "syn-invalid"],
  ["string.invalid", "syn-invalid"],
];

/** The Monaco theme for the root's theme and palette, built from the tokens alone. */
export function monacoTheme(): monaco.editor.IStandaloneThemeData {
  const fg = colorToken("fg");
  const panel = colorToken("panel");
  const line = colorToken("line");
  const find = colorToken("editor-find");
  const selection = colorToken("editor-selection");
  const hover = colorToken("hover");
  return {
    base: colorScheme() === "dark" ? "vs-dark" : "vs",
    // Nothing comes from the stock theme's syntax colors.
    inherit: false,
    rules: [
      { token: "", foreground: fg.slice(1), background: panel.slice(1) },
      ...SYNTAX.map(([token, name]) => ({ token, foreground: colorToken(name).slice(1) })),
      { token: "emphasis", fontStyle: "italic" },
      { token: "strong", fontStyle: "bold" },
    ],
    colors: {
      "editor.background": panel,
      "editor.foreground": fg,
      "editorGutter.background": panel,
      "editorLineNumber.foreground": colorToken("faint"),
      "editorLineNumber.activeForeground": fg,
      "editor.lineHighlightBackground": colorToken("editor-line"),
      "editor.lineHighlightBorder": colorToken("editor-line"),
      "editor.selectionBackground": selection,
      "editor.inactiveSelectionBackground": selection,
      "editor.findMatchBackground": find,
      "editor.findMatchHighlightBackground": find,
      "editor.wordHighlightBackground": find,
      "editor.wordHighlightStrongBackground": find,
      "editorCursor.foreground": colorToken("accent"),
      // Monaco colors bracket pairs by depth. Here they stay the text color.
      ...Object.fromEntries([1, 2, 3, 4, 5, 6].map((depth) => [`editorBracketHighlight.foreground${depth}`, fg])),
      "editorBracketHighlight.unexpectedBracket.foreground": colorToken("syn-invalid"),
      "editorBracketMatch.background": hover,
      "editorBracketMatch.border": line,
      "editorIndentGuide.background1": colorToken("line-soft"),
      "editorIndentGuide.activeBackground1": line,
      "editorWidget.background": panel,
      "editorWidget.foreground": fg,
      "editorWidget.border": line,
      "editorHoverWidget.background": panel,
      "editorHoverWidget.border": line,
      "editorSuggestWidget.background": panel,
      "editorSuggestWidget.border": line,
      "editorSuggestWidget.selectedBackground": colorToken("row-selected"),
      "scrollbarSlider.background": hover,
      "scrollbarSlider.hoverBackground": hover,
      "scrollbarSlider.activeBackground": colorToken("row-selected"),
      "diffEditor.insertedLineBackground": colorToken("add-bg"),
      "diffEditor.removedLineBackground": colorToken("del-bg"),
      "diffEditor.insertedTextBackground": colorToken("add-word"),
      "diffEditor.removedTextBackground": colorToken("del-word"),
      "diffEditorGutter.insertedLineBackground": colorToken("add-bg"),
      "diffEditorGutter.removedLineBackground": colorToken("del-bg"),
      "diffEditor.border": line,
      "editorOverviewRuler.background": panel,
      "editorOverviewRuler.border": line,
      "diffEditorOverview.insertedForeground": colorToken("add"),
      "diffEditorOverview.removedForeground": colorToken("del"),
    },
  };
}

/** The xterm.js theme for the root's theme and palette, built from the tokens alone. */
export function terminalTheme(): ITheme {
  const ansi = (name: string) => colorToken(`ansi-${name}`);
  return {
    background: colorToken("term-bg"),
    foreground: colorToken("term-fg"),
    cursor: colorToken("accent"),
    cursorAccent: colorToken("term-bg"),
    selectionBackground: colorToken("editor-selection"),
    black: ansi("black"),
    red: ansi("red"),
    green: ansi("green"),
    yellow: ansi("yellow"),
    blue: ansi("blue"),
    magenta: ansi("magenta"),
    cyan: ansi("cyan"),
    white: ansi("white"),
    brightBlack: ansi("bright-black"),
    brightRed: ansi("bright-red"),
    brightGreen: ansi("bright-green"),
    brightYellow: ansi("bright-yellow"),
    brightBlue: ansi("bright-blue"),
    brightMagenta: ansi("bright-magenta"),
    brightCyan: ansi("bright-cyan"),
    brightWhite: ansi("bright-white"),
  };
}

/** The root's color scheme, which `useAppearance` sets as `data-theme`. */
export function colorScheme(): "light" | "dark" {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}

/**
 * Calls `changed` after the root's theme or palette changes, once the window's tokens have the
 * new values. Returns a function that stops it.
 */
export function onAppearanceChange(changed: () => void): () => void {
  const observer = new MutationObserver(changed);
  observer.observe(document.documentElement, { attributeFilter: ["data-theme", "data-palette"] });
  return () => observer.disconnect();
}
