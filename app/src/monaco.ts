import "monaco-editor/basic-languages/monaco.contribution";
import * as monaco from "monaco-editor/editor";
import "monaco-editor/features/register.all";
import EditorWorker from "monaco-editor/editor/editor.worker?worker";

import { buildOrReport, codeFont, colorScheme, monacoTheme, onAppearanceChange, syntaxTokenOf } from "./codeTheme";

// Everything comes from the app bundle, never a CDN, because WKWebView may be offline.
// `features` adds the editor's own parts, such as icons, find and folding. The editor worker
// computes diffs. The languages only color syntax, so they need no workers of their own.
self.MonacoEnvironment = { getWorker: () => new EditorWorker() };

export { monaco };

// Monaco has one theme for every editor on the page. It follows the root's theme and palette while
// any editor is open, and is built again from the tokens when the next one opens.
let openEditors = 0;
let stopFollowing = () => {};

function applyTheme() {
  const theme = buildOrReport(monacoTheme);
  if (!theme) {
    monaco.editor.setTheme(colorScheme() === "dark" ? "vs-dark" : "vs");
    return;
  }
  const name = `grove-${colorScheme()}`;
  monaco.editor.defineTheme(name, theme);
  monaco.editor.setTheme(name);
}

/**
 * Themes Monaco from the tokens, now and on each change of theme or palette, until every editor
 * that called this has called the function it returns.
 */
export function holdCodeTheme(): () => void {
  if (openEditors++ === 0) {
    applyTheme();
    stopFollowing = onAppearanceChange(applyTheme);
  }
  return () => {
    if (--openEditors === 0) stopFollowing();
  };
}

// The scrollbars' width and the space above the first line, in pixels.
const SCROLLBAR_SIZE = 6;
const TOP_PADDING = 8;

/** The options the editor and both sides of the diff share: the code font and a quiet frame. */
export function editorOptions(): monaco.editor.IEditorOptions {
  return {
    ...buildOrReport(codeFont),
    fontLigatures: false,
    cursorWidth: 2,
    minimap: { enabled: false },
    overviewRulerBorder: false,
    padding: { top: TOP_PADDING },
    scrollbar: { verticalScrollbarSize: SCROLLBAR_SIZE, horizontalScrollbarSize: SCROLLBAR_SIZE, useShadows: false },
  };
}

/** The language Monaco colors `path` as, by its name or extension. */
export function languageOf(path: string): string | undefined {
  const name = path.split("/").pop() ?? path;
  const dot = name.lastIndexOf(".");
  const extension = dot > 0 ? name.slice(dot).toLowerCase() : null;
  return monaco.languages
    .getLanguages()
    .find((l) => l.filenames?.includes(name) || (extension && l.extensions?.includes(extension)))
    ?.id;
}

/** A run of code and the syntax token that colors it, if any. */
export type CodeRun = { text: string; syntax?: string };

// Tokenizing runs on the main thread, at about 5,000 characters a millisecond and slower for code
// full of slashes, so a block this long takes about 6 ms at most. A longer block stays plain.
const MOST_CHARACTERS_TO_COLOR = 20_000;

// The blocks colored so far, by tag and code, so a block shown again is not tokenized again. The
// oldest goes first once there are this many.
const MOST_BLOCKS_KEPT = 200;
const colorings = new Map<string, { runs?: CodeRun[][]; done: Promise<void> }>();

// Blocks color one at a time, each in a task of its own, so the window paints between them.
let lastColoring = Promise.resolve();
const nextTask = () => new Promise<void>((resolve) => setTimeout(resolve));

/** The colored runs of `code` in the language `tag` names, if `colorRuns` has finished them. */
export function coloredRuns(code: string, tag: string): CodeRun[][] | undefined {
  return colorings.get(`${tag}\n${code}`)?.runs;
}

/**
 * Colors `code` in the language a fenced block's tag names, such as ts or rust, for `coloredRuns`
 * to give. It stays plain when Monaco has no such language or the block is too long.
 */
export function colorRuns(code: string, tag: string): Promise<void> {
  const key = `${tag}\n${code}`;
  const known = colorings.get(key);
  if (known) return known.done;
  const coloring: { runs?: CodeRun[][]; done: Promise<void> } = {
    done: lastColoring.then(nextTask).then(async () => {
      coloring.runs = await runsOf(code, tag);
    }),
  };
  lastColoring = coloring.done.catch(() => {});
  colorings.set(key, coloring);
  if (colorings.size > MOST_BLOCKS_KEPT) colorings.delete(colorings.keys().next().value!);
  return coloring.done;
}

async function runsOf(code: string, tag: string): Promise<CodeRun[][] | undefined> {
  if (code.length > MOST_CHARACTERS_TO_COLOR) return undefined;
  const name = tag.toLowerCase();
  const language = monaco.languages
    .getLanguages()
    .find((l) => l.id === name || l.aliases?.some((alias) => alias.toLowerCase() === name) || l.extensions?.includes(`.${name}`));
  if (!language) return undefined;
  await loadLanguages([language.id]);
  let lines = monaco.editor.tokenize(code, language.id);
  // Code in a language embedded in this one, such as a script in HTML, comes as one plain token
  // until that language has loaded too.
  const embedded = new Set(lines.flat().map((token) => token.language));
  if ([...embedded].some((id) => !loadedLanguages.has(id))) {
    await loadLanguages([...embedded]);
    lines = monaco.editor.tokenize(code, language.id);
  }
  const text = code.split(/\r\n|\r|\n/);
  return lines.map((tokens, index) => {
    const line = text[index];
    const runs: CodeRun[] = [];
    tokens.forEach((token, at) => {
      const part = line.slice(token.offset, tokens[at + 1]?.offset ?? line.length);
      const syntax = syntaxTokenOf(token.type);
      const last = runs.at(-1);
      if (last && last.syntax === syntax) last.text += part;
      else runs.push(syntax ? { text: part, syntax } : { text: part });
    });
    return runs;
  });
}

// A language's tokenizer loads on first use. Colorize waits for it, and tokenize does not.
const loadedLanguages = new Set<string>();

async function loadLanguages(ids: string[]) {
  await Promise.all(ids.filter((id) => !loadedLanguages.has(id)).map((id) => monaco.editor.colorize("", id, {})));
  ids.forEach((id) => loadedLanguages.add(id));
}
