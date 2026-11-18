// How the core anchors a review comment to its text, for the UI and the fake bridge.

/** A file's lines as the core reads them: split at \n, each without a trailing \r, and no last empty line after a final newline. */
export function fileLines(contents: string): string[] {
  const lines = contents.split("\n").map((line) => line.replace(/\r$/, ""));
  if (contents.endsWith("\n")) lines.pop();
  return lines;
}

/**
 * The 1-based line where `text` starts in `lines`. Of several copies, the one closest to line
 * `near`, and of two as close, the earlier. null when it is not there.
 */
export function findLines(lines: string[], text: string, near: number): number | null {
  const want = text.split("\n");
  let best: number | null = null;
  for (let at = 0; at + want.length <= lines.length; at++) {
    if (!want.every((line, i) => lines[at + i] === line)) continue;
    const line = at + 1;
    if (best === null || Math.abs(line - near) < Math.abs(best - near)) best = line;
  }
  return best;
}
