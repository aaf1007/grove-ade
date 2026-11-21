import { afterEach, expect, test } from "vitest";

import { codeFont, colorToken, monacoTheme, onAppearanceChange, syntaxTokenOf, terminalTheme } from "./codeTheme";
import tokens from "./tokens.css?raw";
import { setAppearance, tokensInDocument } from "./tokensInDocument";

let removeTokens = () => {};

afterEach(() => removeTokens());

// Every color token in the token file, with its default dark value.
const colorTokens = [...tokens.matchAll(/^\s*--([\w-]+):\s*(#[0-9a-f]{6}|rgba\([^)]*\)|var\(--[\w-]+\));/gim)]
  .map(([, name, value]) => ({ name, value }))
  .filter(({ name }, index, all) => all.findIndex((other) => other.name === name) === index);

// The token file with each color token set to a color no other token has, translucent where the
// real one is. A reference to another token stays one.
function uniqueColors() {
  const expected = new Set<string>();
  const declarations = colorTokens.map(({ name, value }, index) => {
    const blue = index + 1;
    const hex = `#0000${blue.toString(16).padStart(2, "0")}`;
    if (value.startsWith("var(")) return `--${name}: ${value};`;
    if (value.startsWith("rgba(")) {
      expected.add(`${hex}80`);
      return `--${name}: rgba(0, 0, ${blue}, 0.5);`;
    }
    expected.add(hex);
    return `--${name}: ${hex};`;
  });
  const scales = tokens.slice(tokens.indexOf("/* Scales"));
  return { css: `:root {\n${declarations.join("\n")}\n}\n${scales}`, expected };
}

function colorsIn(theme: ReturnType<typeof monacoTheme>, terminal: ReturnType<typeof terminalTheme>) {
  const ruleColors = theme.rules.flatMap((rule) => [rule.foreground, rule.background]);
  return [
    ...Object.values(theme.colors),
    ...ruleColors.filter((color) => color !== undefined).map((color) => `#${color}`),
    ...Object.values(terminal),
  ];
}

test.each(["dark", "light"])("the %s Monaco and terminal themes hold only colors read from tokens", (theme) => {
  const { css, expected } = uniqueColors();
  expect(colorTokens.length).toBeGreaterThan(60);
  removeTokens = tokensInDocument({ theme, css });

  const colors = colorsIn(monacoTheme(), terminalTheme());

  expect(colors.length).toBeGreaterThan(40);
  expect(colors.filter((color) => !expected.has(color))).toEqual([]);
});

const appearances = [
  { theme: "dark", palette: "default" },
  { theme: "light", palette: "default" },
  { theme: "dark", palette: "modern" },
  { theme: "light", palette: "modern" },
];

// A token's value as the window computes it, as #rrggbb.
function solid(name: string) {
  const value = getComputedStyle(document.documentElement).getPropertyValue(`--${name}`).trim();
  expect(value).toMatch(/^#[0-9a-f]{6}$/i);
  return value.toLowerCase();
}

test.each(appearances)("the Monaco theme for $theme $palette maps the editor's parts to their tokens", (appearance) => {
  removeTokens = tokensInDocument(appearance);

  const theme = monacoTheme();

  expect(theme.base).toBe(appearance.theme === "dark" ? "vs-dark" : "vs");
  expect(theme.colors).toMatchObject({
    "editor.background": solid("panel"),
    "editor.foreground": solid("fg"),
    "editorLineNumber.foreground": solid("faint"),
    "editorLineNumber.activeForeground": solid("fg"),
    "editor.lineHighlightBackground": solid("editor-line"),
    "editor.selectionBackground": solid("editor-selection"),
    "editor.findMatchBackground": solid("editor-find"),
    "editor.findMatchHighlightBackground": solid("editor-find"),
    "editor.wordHighlightBackground": solid("editor-find"),
    "editorCursor.foreground": solid("accent"),
    // Brackets stay the text color.
    "editorBracketHighlight.foreground1": solid("fg"),
    "editorBracketHighlight.foreground6": solid("fg"),
    "editorBracketHighlight.unexpectedBracket.foreground": solid("failed"),
    // The diff's overview ruler.
    "editorOverviewRuler.background": solid("panel"),
    "diffEditorOverview.insertedForeground": solid("add"),
    "diffEditorOverview.removedForeground": solid("del"),
  });
  const ruleFor = (token: string) => `#${theme.rules.find((rule) => rule.token === token)?.foreground}`;
  expect(ruleFor("")).toBe(solid("fg"));
  expect(ruleFor("keyword")).toBe(solid("syn-kw"));
  expect(ruleFor("string")).toBe(solid("syn-str"));
  expect(ruleFor("type")).toBe(solid("syn-type"));
  expect(ruleFor("comment")).toBe(solid("syn-com"));
  expect(ruleFor("number")).toBe(solid("syn-num"));
  expect(ruleFor("tag")).toBe(solid("syn-tag"));
  const syntax = ["kw", "fn", "type", "str", "num", "const", "prop", "tag", "com"].map((kind) => solid(`syn-${kind}`));
  const ruleColors = theme.rules.map((rule) => `#${rule.foreground}`);
  expect(syntax.filter((color) => !ruleColors.includes(color))).toEqual([]);
});

test("translucent diff tokens become hex with alpha", () => {
  removeTokens = tokensInDocument({ theme: "dark", palette: "default" });

  expect(monacoTheme().colors).toMatchObject({
    // rgba(0, 188, 125, 0.12) and rgba(251, 44, 54, 0.12)
    "diffEditor.insertedLineBackground": "#00bc7d1f",
    "diffEditor.removedLineBackground": "#fb2c361f",
    // rgba(0, 188, 125, 0.28) and rgba(251, 44, 54, 0.28)
    "diffEditor.insertedTextBackground": "#00bc7d47",
    "diffEditor.removedTextBackground": "#fb2c3647",
  });
});

test("syn-invalid follows its reference to failed", () => {
  removeTokens = tokensInDocument({ theme: "dark", palette: "default" });

  const invalid = monacoTheme().rules.find((rule) => rule.token === "invalid");

  expect(`#${invalid?.foreground}`).toBe(solid("failed"));
});

test.each(appearances)("the terminal theme for $theme $palette maps the terminal's parts to their tokens", (appearance) => {
  removeTokens = tokensInDocument(appearance);

  expect(terminalTheme()).toEqual({
    background: solid("term-bg"),
    foreground: solid("term-fg"),
    cursor: solid("accent"),
    cursorAccent: solid("term-bg"),
    selectionBackground: solid("editor-selection"),
    black: solid("ansi-black"),
    red: solid("ansi-red"),
    green: solid("ansi-green"),
    yellow: solid("ansi-yellow"),
    blue: solid("ansi-blue"),
    magenta: solid("ansi-magenta"),
    cyan: solid("ansi-cyan"),
    white: solid("ansi-white"),
    brightBlack: solid("ansi-bright-black"),
    brightRed: solid("ansi-bright-red"),
    brightGreen: solid("ansi-bright-green"),
    brightYellow: solid("ansi-bright-yellow"),
    brightBlue: solid("ansi-bright-blue"),
    brightMagenta: solid("ansi-bright-magenta"),
    brightCyan: solid("ansi-bright-cyan"),
    brightWhite: solid("ansi-bright-white"),
  });
});

// The core answers a program's color queries with fixed dark colors, so the terminal stays dark in
// light mode until the core hears the UI's colors. Then the light tokens can differ.
test.each(["default", "modern"])("in light mode the %s palette's terminal keeps its dark theme", (palette) => {
  removeTokens = tokensInDocument({ theme: "dark", palette });
  const dark = terminalTheme();
  setAppearance("light", palette);

  const light = terminalTheme();

  expect({ ...light, cursor: undefined, selectionBackground: undefined }).toEqual({
    ...dark,
    cursor: undefined,
    selectionBackground: undefined,
  });
});

test("code is the mono token at the code size on an 18px line", () => {
  removeTokens = tokensInDocument();

  expect(codeFont()).toEqual({
    fontFamily: getComputedStyle(document.documentElement).getPropertyValue("--font-mono").trim(),
    fontSize: 13,
    lineHeight: 18,
  });
});

// The production build minifies colors: #000000 becomes #000, #008000 becomes green, and a
// translucent rgba() becomes 8-digit hex.
test.each([
  ["#000", "#000000"],
  ["#111111", "#111111"],
  ["#0009", "#00000099"],
  ["#ffffff14", "#ffffff14"],
  ["green", "#008000"],
  ["rgba(0, 188, 125, 0.12)", "#00bc7d1f"],
  ["rgba(0,188,125,.28)", "#00bc7d47"],
])("a color token written as %s reads as %s", (written, hex) => {
  removeTokens = tokensInDocument({ css: `:root { --panel: ${written}; }` });

  expect(colorToken("panel")).toBe(hex);
});

test("the code font reads from the minified shorthand", () => {
  removeTokens = tokensInDocument({
    css: ':root{--font-mono:ui-monospace,"SF Mono",monospace;--text-code-size:13px;--text-code:400 var(--text-code-size)/1.375 var(--font-mono)}',
  });

  expect(codeFont()).toMatchObject({ fontSize: 13, lineHeight: 18 });
});

test.each([
  ["keyword.ts", "syn-kw"],
  ["string.ts", "syn-str"],
  ["string.key.json", "syn-prop"],
  ["string.escape.rust", "syn-const"],
  ["identifier.ts", undefined],
  ["keywords.ts", undefined],
])("a %s token in a chat code block takes %s, as in the editor", (type, token) => {
  expect(syntaxTokenOf(type)).toBe(token);
});

test("a missing token is an error, not a stock color", () => {
  expect(() => terminalTheme()).toThrow("--term-bg");
});

test("a change of theme or palette on the root reaches each listener until it stops listening", async () => {
  removeTokens = tokensInDocument();
  const heard: string[] = [];
  const stop = onAppearanceChange(() => heard.push(document.documentElement.dataset.theme!));

  setAppearance("light", "default");
  await Promise.resolve();
  setAppearance("light", "modern");
  await Promise.resolve();
  stop();
  setAppearance("dark", "modern");
  await Promise.resolve();

  expect(heard).toEqual(["light", "light"]);
});
