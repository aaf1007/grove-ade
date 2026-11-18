import type { Terminal } from "@xterm/xterm";

// xterm.js has no public API for its cell size. @xterm/addon-fit reads this same field.
interface Internals {
  _core: { _renderService?: { dimensions: { css: { cell: { width: number; height: number } } } } };
}

/** One cell's size in CSS pixels as xterm.js draws it, or null before it has measured one. */
export function cellSize(term: Terminal): { width: number; height: number } | null {
  const cell = (term as unknown as Internals)._core._renderService?.dimensions.css.cell;
  return cell && cell.width > 0 && cell.height > 0 ? cell : null;
}
