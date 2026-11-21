import { expect, test } from "vitest";

import tokens from "./tokens.css?raw";

// Every app file but the token file and the tests, which may hold colors as data.
const sources = import.meta.glob<string>(
  ["./**/*.{ts,tsx,css}", "!./**/*.test.*", "!./tokens.css"],
  { query: "?raw", import: "default", eager: true },
);

test("no app file outside the token file has a raw color", () => {
  expect(Object.keys(sources).length).toBeGreaterThan(5);
  const raw = Object.entries(sources).flatMap(([path, text]) =>
    (withoutComments(text).match(/#[0-9a-f]{3,8}\b|\b(rgba?|hsla?)\(/gi) ?? []).map(
      (color) => `${path}: ${color}`,
    ),
  );
  expect(raw).toEqual([]);
});

// A number with a unit or percent outside var() is a raw size, and so is a bare number in a style object.
// Keywords such as 0, none and inherit, and unitless line heights, are fine.
test("no app file outside the token file has a raw radius, font size or shadow", () => {
  const raw = Object.entries(sources).flatMap(([path, text]) =>
    rawSizes(text).map((declaration) => `${path}: ${declaration}`),
  );
  expect(raw).toEqual([]);
});

test.each([
  ".a { border-radius: 7px; }",
  ".a { border-top-left-radius: 50%; }",
  ".a { font-size: 12.5px; }",
  ".a { font: 13px/1.45 var(--font-ui); }",
  ".a { box-shadow: 0 0 0 1px var(--line); }",
  'style={{ fontSize: "13px" }}',
  "style={{ fontSize: 13 }}",
  'style={{ borderRadius: "8px" }}',
  "style={{ borderTopLeftRadius: 4 }}",
  'style={{ boxShadow: "0 1px 2px black" }}',
  ".a { font-size: 2ch; }",
  ".a { border-radius: 1vmin; }",
  'style={{ boxShadow: "0 0 currentColor, 0 1px 2px currentColor" }}',
])("the raw size check rejects %s", (source) => {
  expect(rawSizes(source)).toHaveLength(1);
});

test.each([
  ".a { border-radius: var(--radius-lg); font: inherit; box-shadow: none; }",
  ".a { font: var(--text-code-size)/1.45 var(--font-ui); border-radius: 0; }",
  "style={{ fontSize: \"var(--text-meta-size)\", borderRadius: 0, boxShadow: \"var(--ring)\" }}",
])("the raw size check allows %s", (source) => {
  expect(rawSizes(source)).toEqual([]);
});

// Every app stylesheet but the token file.
const stylesheets = Object.fromEntries(Object.entries(sources).filter(([path]) => path.endsWith(".css")));

test("no app stylesheet outside the token file has a raw padding, margin or gap", () => {
  expect(Object.keys(stylesheets).length).toBeGreaterThan(3);
  const raw = Object.entries(stylesheets).flatMap(([path, text]) =>
    rawSpacing(text).map((declaration) => `${path}: ${declaration}`),
  );
  expect(raw).toEqual([]);
});

test.each([
  ".a { padding: 7px 14px; }",
  ".a { padding-left: 22px; }",
  ".a { margin: 0 auto 6px; }",
  ".a { margin-block: 1em; }",
  ".a { gap: 1px; }",
  ".a { row-gap: 4px; }",
  ".a { column-gap: 2%; }",
  ".a { padding-inline: var(--space-8) 3px; }",
])("the raw spacing check rejects %s", (source) => {
  expect(rawSpacing(source)).toHaveLength(1);
});

test.each([
  ".a { padding: 0 var(--space-12); margin: 0 auto; gap: var(--space-8); }",
  ".a { margin-left: calc(var(--space-8) * -1); padding: inherit; }",
  ".a { grid-template-columns: 1fr 5px; top: 30px; scroll-padding: 0; }",
])("the raw spacing check allows %s", (source) => {
  expect(rawSpacing(source)).toEqual([]);
});

// Each padding, margin or gap declaration that sets a raw length.
function rawSpacing(css: string): string[] {
  return [...withoutComments(css).matchAll(/(?<![\w-])(padding|margin|gap|row-gap|column-gap)(-[a-z]+)*\s*:([^;}]+)/g)]
    .filter(([, , , value]) => /\d(%|[a-z])/i.test(value.replace(/var\([^)]*\)/g, "")))
    .map(([declaration]) => declaration.trim());
}

// Each radius, font size or shadow declaration, in CSS or in a style object, that sets a raw size.
function rawSizes(text: string): string[] {
  return [...withoutComments(text).matchAll(sizedDeclaration)]
    .filter(([, , cssValue, , styleValue]) => {
      const value = (cssValue ?? styleValue).replace(/var\([^)]*\)/g, "");
      const bareNumber = /^\s*\d*\.?\d+\s*$/.test(value) && parseFloat(value) !== 0;
      return /\d(%|[a-z])/i.test(value) || (styleValue !== undefined && bareNumber);
    })
    .map(([declaration]) => declaration.trim());
}

// A CSS declaration, up to its semicolon, or a style object property: a quoted string, or up to its comma.
const sizedDeclaration =
  /(?<![\w-])(border(?:-[a-z]+)*-radius|font-size|font|box-shadow)\s*:([^;}]+)|(?<![\w-])(border(?:[A-Z][a-z]+)*Radius|fontSize|boxShadow)\s*:\s*("[^"]*"|'[^']*'|[^,;}\n]+)/g;

// Each solid text color and the grounds its usage note in the design system names.
const states = ["working", "needs", "question", "done", "idle"];
const syntax = ["kw", "fn", "type", "str", "num", "const", "prop", "tag", "com", "invalid"];
const ansi = ["red", "green", "yellow", "blue", "magenta", "cyan", "white"];
const textOnGrounds: [string, string[]][] = [
  ["fg", ["bg", "sidebar", "panel", "message", "code-bg", "editor-selection", "editor-find"]],
  ["muted", ["bg", "sidebar", "panel"]],
  ["faint", ["bg", "sidebar"]],
  ["accent-fg", ["accent"]],
  ["on-failed", ["failed"]],
  ["accent-text", ["bg", "panel"]],
  ...states.map((state): [string, string[]] => [state, ["bg", "sidebar"]]),
  ...syntax.map((kind): [string, string[]] => [`syn-${kind}`, ["panel", "code-bg"]]),
  ["term-fg", ["term-bg"]],
  ["term-dim", ["term-bg"]],
  ...ansi.map((name): [string, string[]] => [`ansi-${name}`, ["term-bg"]]),
  ...ansi.slice(0, 6).map((name): [string, string[]] => [`ansi-bright-${name}`, ["term-bg"]]),
];

const defaultDark = declarations(tokens, ":root");
const defaultThemes = {
  dark: defaultDark,
  light: { ...defaultDark, ...declarations(tokens, ':root[data-theme="light"]') },
};

test.each(
  Object.entries(defaultThemes).flatMap(([theme, values]) =>
    textOnGrounds.flatMap(([text, grounds]) =>
      grounds.map((ground) => ({ theme, text, ground, values })),
    ),
  ),
)("$text on $ground has 4.5:1 contrast in $theme", ({ text, ground, values }) => {
  expect(contrast(resolve(values, text), resolve(values, ground))).toBeGreaterThanOrEqual(4.5);
});

// The four surface steps: sidebar, chat area, raised (the panel area, dialogs and menus) and code.
// Each must read as its own region, so sorted by lightness each step sits a set distance from the next.
const surfaces = ["sidebar", "bg", "panel", "code-bg"];
const minimumLightnessStep = 3;

const modernDark = { ...defaultDark, ...declarations(tokens, ':root[data-palette="modern"]') };
const everyTheme = {
  "default dark": defaultThemes.dark,
  "default light": defaultThemes.light,
  "modern dark": modernDark,
  "modern light": {
    ...defaultThemes.light,
    ...modernDark,
    ...declarations(tokens, ':root[data-palette="modern"][data-theme="light"]'),
  },
};

test.each(Object.entries(everyTheme))("the surface steps of %s are far enough apart", (_, values) => {
  expect(surfacesTooClose(values)).toEqual([]);
});

test("the surface step check names the pair that is too close", () => {
  const values = { "--sidebar": "#000000", "--bg": "#202020", "--panel": "#404040", "--code-bg": "#424242" };
  expect(surfacesTooClose(values)).toEqual(["panel and code-bg"]);
});

// Each pair of neighbouring surfaces, sorted by lightness, that sits closer than the minimum step.
function surfacesTooClose(values: Record<string, string>): string[] {
  const steps = surfaces
    .map((surface) => ({ surface, lightness: lightness(resolve(values, surface)) }))
    .sort((first, second) => first.lightness - second.lightness);
  return steps
    .slice(1)
    .map((upper, index) => ({ lower: steps[index], upper }))
    .filter(({ lower, upper }) => upper.lightness - lower.lightness < minimumLightnessStep)
    .map(({ lower, upper }) => `${lower.surface} and ${upper.surface}`);
}

// The Modern pairs that met AA before the surfaces moved apart. The rest of Modern's text contrast is not checked yet.
const modernTextOnGrounds: [keyof typeof everyTheme, string, string][] = [
  ["modern light", "muted", "sidebar"],
  ["modern light", "idle", "sidebar"],
  ["modern light", "faint", "sidebar"],
  ["modern light", "syn-kw", "code-bg"],
  ["modern light", "syn-com", "code-bg"],
  ["modern dark", "faint", "code-bg"],
];

test.each(modernTextOnGrounds)("in %s, %s on %s has 4.5:1 contrast", (theme, text, ground) => {
  const values = everyTheme[theme];
  expect(contrast(resolve(values, text), resolve(values, ground))).toBeGreaterThanOrEqual(4.5);
});

test("the chat area is the darkest surface in the default dark theme", () => {
  const chat = lightness(resolve(defaultDark, "bg"));
  for (const surface of surfaces.filter((name) => name !== "bg")) {
    expect(lightness(resolve(defaultDark, surface)), surface).toBeGreaterThan(chat);
  }
});

// CIE L*, from 0 for black to 100 for white, in steps that look even to the eye.
function lightness(hex: string): number {
  const relative = luminance(hex);
  return relative > 216 / 24389 ? 116 * Math.cbrt(relative) - 16 : (relative * 24389) / 27;
}

// A token's value with var() references followed, as a solid #rrggbb color.
function resolve(values: Record<string, string>, token: string): string {
  const value = values[`--${token}`];
  if (value === undefined) throw new Error(`no --${token}`);
  const reference = /^var\(--([\w-]+)\)$/.exec(value);
  if (reference) return resolve(values, reference[1]);
  if (!/^#[0-9a-f]{6}$/i.test(value)) throw new Error(`--${token} is not a solid color: ${value}`);
  return value;
}

// WCAG 2 contrast ratio between two #rrggbb colors.
function contrast(first: string, second: string): number {
  const [lighter, darker] = [luminance(first), luminance(second)].sort((a, b) => b - a);
  return (lighter + 0.05) / (darker + 0.05);
}

function luminance(hex: string): number {
  const [red, green, blue] = [1, 3, 5].map((start) => {
    const channel = parseInt(hex.slice(start, start + 2), 16) / 255;
    return channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

// The properties set by the top-level block with exactly this selector.
function declarations(css: string, selector: string): Record<string, string> {
  const escaped = selector.replace(/[[\]"=]/g, "\\$&");
  const block = new RegExp(`^${escaped} \\{([^}]*)\\}`, "m").exec(withoutComments(css));
  if (!block) throw new Error(`no block for ${selector}`);
  return Object.fromEntries(
    block[1]
      .split(";")
      .map((line) => line.trim())
      .filter(Boolean)
      .map((line) => {
        const colon = line.indexOf(":");
        return [line.slice(0, colon).trim(), line.slice(colon + 1).replace(/\s+/g, " ").trim()];
      }),
  );
}

function withoutComments(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|[^:])\/\/.*$/gm, "$1");
}
