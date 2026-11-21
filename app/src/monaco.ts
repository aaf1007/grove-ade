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

/**
 * `code` cut into colored runs, one list per line, by the language a fenced block's tag names, such
 * as ts or rust. Undefined when Monaco has no such language.
 */
export async function colorRuns(code: string, tag: string): Promise<CodeRun[][] | undefined> {
  const name = tag.toLowerCase();
  const language = monaco.languages
    .getLanguages()
    .find((l) => l.id === name || l.aliases?.some((alias) => alias.toLowerCase() === name) || l.extensions?.includes(`.${name}`));
  if (!language) return undefined;
  // A language's tokenizer loads on first use. Colorize waits for it, and tokenize does not.
  await monaco.editor.colorize("", language.id, {});
  const lines = code.split(/\r\n|\r|\n/);
  return monaco.editor.tokenize(code, language.id).map((tokens, index) => {
    const line = lines[index];
    const runs: CodeRun[] = [];
    tokens.forEach((token, at) => {
      const text = line.slice(token.offset, tokens[at + 1]?.offset ?? line.length);
      const syntax = syntaxTokenOf(token.type);
      const last = runs.at(-1);
      if (last && last.syntax === syntax) last.text += text;
      else runs.push(syntax ? { text, syntax } : { text });
    });
    return runs;
  });
}
