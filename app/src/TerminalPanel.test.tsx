import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { Terminal } from "@xterm/xterm";
import { afterEach, beforeEach, expect, onTestFinished, test, vi } from "vitest";

import { fakeBridge } from "./fakeBridge";
import { TerminalPanel } from "./TerminalPanel";
import { setAppearance, tokensInDocument } from "./tokensInDocument";

// The real xterm.js, so tests see what it parses, writes and replies. Each one made is kept here.
const terminals = vi.hoisted(() => [] as Terminal[]);
// WebGL loads and full redraws, with the number of the terminal each was for.
const drawLog = vi.hoisted(() => [] as string[]);
vi.mock("@xterm/xterm", async (importOriginal) => {
  const xterm = await importOriginal<typeof import("@xterm/xterm")>();
  class KeptTerminal extends xterm.Terminal {
    constructor(...args: ConstructorParameters<typeof xterm.Terminal>) {
      super(...args);
      // Under vitest xterm.js takes the platform to be Node. grove runs on macOS, where Option and
      // Cmd mean what the key tests check.
      const core = (this as unknown as { _core: { browser: object } })._core;
      core.browser = { ...core.browser, isMac: true };
      terminals.push(this);
    }
    refresh(start: number, end: number) {
      if (start === 0 && end === this.rows - 1) drawLog.push(`redraw ${terminals.indexOf(this)}`);
      super.refresh(start, end);
    }
  }
  return { ...xterm, Terminal: KeptTerminal };
});

// jsdom has no WebGL. The addon draws on its own canvas in the terminal, as the real one does.
vi.mock("@xterm/addon-webgl", () => ({
  WebglAddon: class {
    private readonly canvas = document.createElement("canvas");
    private term: Terminal | undefined;
    activate(term: Terminal) {
      this.term = term;
      this.canvas.className = "webgl";
      term.element!.append(this.canvas);
      drawLog.push(`webgl ${terminals.indexOf(term)}`);
    }
    dispose() {
      this.canvas.remove();
      drawLog.push(`dispose ${terminals.indexOf(this.term!)}`);
    }
  },
}));

// jsdom lays nothing out, so xterm.js measures its cells as 0x0.
vi.mock("./cellSize", () => ({ cellSize: () => ({ width: 8, height: 16 }) }));

// jsdom has no ResizeObserver. Tests size the panel as WebKit reports it: a hidden panel is 0x0.
const observers: ResizeObserverCallback[] = [];
class FakeResizeObserver {
  constructor(callback: ResizeObserverCallback) {
    observers.push(callback);
  }
  observe() {}
  disconnect() {}
}

function resizeTo(width: number, height: number) {
  const entry = { contentRect: { width, height } } as ResizeObserverEntry;
  for (const callback of observers) callback([entry], {} as ResizeObserver);
}

let removeTokens = () => {};

beforeEach(() => {
  removeTokens = tokensInDocument({ theme: "dark", palette: "default" });
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  // xterm.js watches the device pixel ratio, which jsdom cannot report.
  vi.stubGlobal("matchMedia", (media: string) => ({
    matches: false,
    media,
    addListener() {},
    removeListener() {},
  }));
});

afterEach(() => {
  cleanup();
  removeTokens();
  setPageVisibility("visible");
  vi.useRealTimers();
  terminals.length = 0;
  observers.length = 0;
  drawLog.length = 0;
  vi.unstubAllGlobals();
});

const worktree = "/code/grove";

async function shellSession() {
  const bridge = fakeBridge({ [worktree]: [{ path: worktree, branch: "main", isMain: true }] });
  await bridge.addRepo(worktree);
  const session = await bridge.spawnShell(worktree, 80, 24);
  return { bridge, session };
}

// WebKit hides the page when the window is hidden, minimized or covered, or the screen locks.
function setPageVisibility(visibility: DocumentVisibilityState) {
  Object.defineProperty(document, "visibilityState", { value: visibility, configurable: true });
  document.dispatchEvent(new Event("visibilitychange"));
}

const nextFrame = () => new Promise((resolve) => requestAnimationFrame(resolve));

const utf8 = (text: string) => new TextEncoder().encode(text);

function typed(bridge: { writes: { bytes: Uint8Array }[] }) {
  return bridge.writes.map(({ bytes }) => new TextDecoder().decode(bytes)).join("");
}

function line(row: number) {
  return terminals[0].buffer.active.getLine(row)?.translateToString(true);
}

// xterm.js state with no public getter.
interface XtermCore {
  coreService: {
    isCursorHidden: boolean;
    // Set by DECSCUSR. undefined means the terminal's own option.
    decPrivateModes: { cursorStyle?: string; cursorBlink?: boolean };
  };
  // The theme's colors, as OSC 4, 10, 11 and 12 change them.
  _themeService: {
    colors: Record<"background" | "foreground" | "cursor", Color> & { ansi: Color[] };
  };
}
interface Color {
  css: string;
}

function xtermCore() {
  return (terminals[0] as unknown as { _core: XtermCore })._core;
}

const cursorHidden = () => xtermCore().coreService.isCursorHidden;

function colors() {
  const { background, foreground, cursor, ansi } = xtermCore()._themeService.colors;
  return [background, foreground, cursor, ansi[1]].map((color) => color.css);
}

function cursorStyle() {
  const { cursorStyle, cursorBlink } = xtermCore().coreService.decPrivateModes;
  return { cursorStyle, cursorBlink };
}

// A shown panel. `reopen` hides it, lets its session print `whileHidden`, and shows it again with
// `modes` then "prompt" as the session's snapshot.
async function reopenablePanel() {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(true));
  async function reopen(whileHidden: string, modes: string) {
    rerender(panel(false));
    bridge.output(session, utf8(whileHidden));
    bridge.setSnapshot(session, utf8(`${modes}prompt`));
    rerender(panel(true));
    await vi.waitFor(() => expect(line(0)).toBe("prompt"));
  }
  return { bridge, session, reopen };
}

test("a frame is acked only after xterm.js has written it", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  bridge.output(session, utf8("hello"));

  expect(bridge.acks).toEqual([]);
  await vi.waitFor(() => expect(bridge.acks).toEqual([{ session, bytes: 5 }]));
  expect(line(0)).toBe("hello");
});

test("what the user types goes to the session", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  terminals[0].input("ls -la\r");

  expect(typed(bridge)).toBe("ls -la\r");
  expect(bridge.writes.every((write) => write.session === session)).toBe(true);
});

test("the panel reports each time its terminal gets focus", async () => {
  const { bridge, session } = await shellSession();
  const onFocus = vi.fn();
  render(<TerminalPanel bridge={bridge} session={session} shown onFocus={onFocus} />);
  terminals[0].blur();

  terminals[0].focus();

  // Once when it first opened, and once now.
  expect(onFocus).toHaveBeenCalledTimes(2);
});

test("the panel reports when its terminal loses focus", async () => {
  const { bridge, session } = await shellSession();
  const onBlur = vi.fn();
  render(<TerminalPanel bridge={bridge} session={session} shown onBlur={onBlur} />);

  terminals[0].blur();

  expect(onBlur).toHaveBeenCalledTimes(1);
});

test("output reaches xterm.js as bytes, so a character split across frames stays whole", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);
  const [first, second, third] = utf8("日");

  bridge.output(session, new Uint8Array([first, second]));
  bridge.output(session, new Uint8Array([third]));

  await vi.waitFor(() => expect(bridge.acks).toHaveLength(2));
  expect(line(0)).toBe("日");
});

test("xterm.js answers no terminal query, so the core's reply is the only one", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);
  const queries = [
    "\x1b[c", // DA1
    "\x1b[>c", // DA2
    "\x1b[5n", // DSR status
    "\x1b[6n", // DSR cursor position
    "\x1b[?6n", // DECXCPR
    "\x1b[4$p", // DECRQM
    "\x1b[?2004$p", // DECRQM, private mode
    "\x1bP$qm\x1b\\", // DECRQSS
    "\x1b[14t", // text area size in pixels
    "\x1b[18t", // text area size in cells
    "\x1b]4;1;?\x07", // palette color
    "\x1b]10;?\x07", // foreground
    "\x1b]11;?\x07", // background
    "\x1b]12;?\x07", // cursor color
  ];

  for (const query of queries) bridge.output(session, utf8(query));

  await vi.waitFor(() => expect(bridge.acks).toHaveLength(queries.length));
  expect(bridge.writes).toEqual([]);
});

test("a hidden or collapsed panel never sends a zero size", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(false));
  resizeTo(0, 0);
  vi.runAllTimers();

  rerender(panel(true));
  // 800 px less the 14 px scrollbar is 98 cells of 8 px; 400 px is 25 rows of 16 px.
  resizeTo(800, 400);
  vi.runAllTimers();
  resizeTo(800, 0);
  vi.runAllTimers();
  rerender(panel(false));
  resizeTo(0, 0);
  vi.runAllTimers();

  expect(bridge.resizes).toEqual([{ session, cols: 98, rows: 25 }]);
});

test("resizes are debounced, so only the size the panel settles on goes out", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  resizeTo(400, 400);
  vi.advanceTimersByTime(50);
  resizeTo(600, 400);
  vi.advanceTimersByTime(50);
  resizeTo(800, 400);
  expect(bridge.resizes).toEqual([]);
  vi.runAllTimers();

  expect(bridge.resizes).toEqual([{ session, cols: 98, rows: 25 }]);
  expect([terminals[0].cols, terminals[0].rows]).toEqual([98, 25]);
});

test("a panel that resizes while its snapshot is on the way writes it at its size, then takes the panel's", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const { bridge, session } = await shellSession();
  // A TUI's screen, which xterm.js does not reflow on resize.
  bridge.output(session, utf8(`\x1b[?1049h${"x".repeat(90)}`));
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  resizeTo(800, 400);
  vi.runAllTimers();
  await vi.runAllTimersAsync();

  // Written at 80 columns, it wrapped after 80 characters.
  expect(line(0)).toBe("x".repeat(80));
  expect([terminals[0].cols, terminals[0].rows]).toEqual([98, 25]);
  expect(bridge.resizes).toEqual([{ session, cols: 98, rows: 25 }]);
});

test("a hidden panel detaches, and attaches again when shown", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(true));
  expect(bridge.attached(session)).toBe(true);

  rerender(panel(false));
  expect(bridge.attached(session)).toBe(false);

  rerender(panel(true));
  expect(bridge.attached(session)).toBe(true);
});

test("a frame still being written when its panel detaches is not acked", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(true));

  bridge.output(session, utf8("before"));
  rerender(panel(false));
  rerender(panel(true));
  bridge.output(session, utf8("after"));

  await vi.waitFor(() => expect(line(0)).toBe("beforeafter"));
  expect(bridge.acks).toEqual([{ session, bytes: 5 }]);
});

test("a reopened panel draws its session's snapshot, then the live frames", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(true));
  bridge.output(session, utf8("drawn"));
  await vi.waitFor(() => expect(line(0)).toBe("drawn"));

  rerender(panel(false));
  bridge.output(session, utf8(" hidden"));
  rerender(panel(true));
  await vi.waitFor(() => expect(line(0)).toBe("drawn hidden"));
  bridge.output(session, utf8(" live"));

  await vi.waitFor(() => expect(line(0)).toBe("drawn hidden live"));
});

test("a frame that comes before the snapshot waits for it, and is drawn after it", async () => {
  const { bridge, session } = await shellSession();
  bridge.output(session, utf8("before the panel, "));
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  // The snapshot reaches the panel only once this test awaits, so this frame comes first.
  bridge.output(session, utf8("then live"));

  await vi.waitFor(() => expect(line(0)).toBe("before the panel, then live"));
  expect(bridge.acks).toEqual([{ session, bytes: 9 }]);
});

test("a frame the snapshot covers is neither drawn nor acked", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(true));
  bridge.output(session, utf8("one"));
  await vi.waitFor(() => expect(bridge.acks).toHaveLength(1));
  rerender(panel(false));
  rerender(panel(true));

  // The snapshot covers frame 1.
  bridge.sendFrame(session, { seq: 1, bytes: utf8(" covered") });
  bridge.sendFrame(session, { seq: 2, bytes: utf8(" two") });
  await vi.waitFor(() => expect(bridge.acks).toHaveLength(2));
  bridge.sendFrame(session, { seq: 1, bytes: utf8(" late") });
  bridge.sendFrame(session, { seq: 3, bytes: utf8(" three") });

  await vi.waitFor(() => expect(line(0)).toBe("one two three"));
  expect(bridge.acks.map((ack) => ack.bytes)).toEqual([3, 4, 6]);
});

// The core counts only live frames against its watermark, and starts from 0 on each attach.
test("a hidden panel acks nothing, and a reopened one acks each live frame but not the snapshot", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(true));
  bridge.output(session, utf8("a"));
  await vi.waitFor(() => expect(bridge.acks).toHaveLength(1));

  rerender(panel(false));
  expect(bridge.attached(session)).toBe(false);
  bridge.output(session, utf8("x".repeat(200_000)));
  rerender(panel(true));
  bridge.output(session, utf8("live"));

  await vi.waitFor(() => expect(bridge.acks).toHaveLength(2));
  await nextFrame();
  expect(bridge.acks).toEqual([
    { session, bytes: 1 },
    { session, bytes: 4 },
  ]);
});

test("a reopened panel shows the cursor its session hid and then showed while it was hidden", async () => {
  const { bridge, session, reopen } = await reopenablePanel();
  bridge.output(session, utf8("\x1b[?25l"));
  await vi.waitFor(() => expect(cursorHidden()).toBe(true));

  // A terminal starts with its cursor shown, so the snapshot has nothing for it.
  await reopen("\x1b[?25h", "");

  expect(cursorHidden()).toBe(false);
});

test("a reopened panel keeps the cursor hidden while its session hides it", async () => {
  const { reopen } = await reopenablePanel();

  await reopen("\x1b[?25l", "\x1b[?25l");

  expect(cursorHidden()).toBe(true);
});

test("a reopened panel takes the cursor style its session set back to the default while it was hidden", async () => {
  const { bridge, session, reopen } = await reopenablePanel();
  bridge.output(session, utf8("\x1b[6 q"));
  await vi.waitFor(() => expect(cursorStyle()).toEqual({ cursorStyle: "bar", cursorBlink: false }));

  // The default style, so the snapshot has nothing for it.
  await reopen("\x1b[0 q", "");

  expect(cursorStyle()).toEqual({ cursorStyle: undefined, cursorBlink: undefined });
});

test("a reopened panel keeps the cursor style its session set", async () => {
  const { reopen } = await reopenablePanel();

  await reopen("\x1b[3 q", "\x1b[3 q");

  expect(cursorStyle()).toEqual({ cursorStyle: "underline", cursorBlink: true });
});

test("a reopened panel takes the theme's colors back when its session reset them while it was hidden", async () => {
  const { bridge, session, reopen } = await reopenablePanel();
  const theme = colors();
  const set = ["11;#102030", "10;#405060", "12;#708090", "4;1;#a0b0c0"];
  bridge.output(session, utf8(set.map((color) => `\x1b]${color}\x07`).join("")));
  await vi.waitFor(() => expect(colors()).toEqual(["#102030", "#405060", "#708090", "#a0b0c0"]));

  // The snapshot holds no colors.
  await reopen("\x1b]111\x07\x1b]110\x07\x1b]112\x07\x1b]104\x07", "");

  expect(colors()).toEqual(theme);
});

function token(name: string) {
  return getComputedStyle(document.documentElement).getPropertyValue(`--${name}`).trim();
}

test("a terminal takes its colors from the tokens and its font from the code tokens", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  const { options } = terminals[0];
  expect(options.theme).toMatchObject({ background: token("term-bg"), foreground: token("term-fg"), red: token("ansi-red") });
  expect(options.fontFamily).toBe(token("font-mono"));
  expect(options.fontSize).toBe(13);
});

test("a terminal still opens with its stock colors when the tokens cannot be read, and says why", async () => {
  const errors = vi.spyOn(console, "error").mockImplementation(() => {});
  removeTokens();
  const { bridge, session } = await shellSession();
  bridge.output(session, utf8("still here"));

  render(<TerminalPanel bridge={bridge} session={session} shown />);

  await vi.waitFor(() => expect(line(0)).toBe("still here"));
  expect(screen.queryByRole("alert")).toBeNull();
  expect(String(errors.mock.calls[0])).toContain("--");
});

test("every open terminal, shown or hidden, recolors when the palette or the theme changes", async () => {
  const { bridge, session } = await shellSession();
  const other = await bridge.spawnShell(worktree, 80, 24);
  render(
    <>
      <TerminalPanel bridge={bridge} session={session} shown />
      <TerminalPanel bridge={bridge} session={other} shown={false} />
    </>,
  );
  const backgrounds = () => terminals.map((term) => term.options.theme?.background);
  const defaultDark = token("term-bg");
  expect(backgrounds()).toEqual([defaultDark, defaultDark]);

  setAppearance("dark", "modern");
  const modernDark = token("term-bg");
  expect(modernDark).not.toBe(defaultDark);
  await vi.waitFor(() => expect(backgrounds()).toEqual([modernDark, modernDark]));
  expect(colors()[0]).toBe(modernDark);

  setAppearance("light", "modern");
  await vi.waitFor(() => expect(terminals[0].options.theme?.selectionBackground).toBe(token("editor-selection")));
  expect(backgrounds()).toEqual([token("term-bg"), token("term-bg")]);
});

test("a change of palette keeps the colors a program set, until the program resets them", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);
  const programColors = () => {
    const { background, ansi } = xtermCore()._themeService.colors;
    return [background.css, ansi[200].css];
  };
  bridge.output(session, utf8("\x1b]4;200;#123456\x07\x1b]11;#102030\x07"));
  await vi.waitFor(() => expect(programColors()).toEqual(["#102030", "#123456"]));

  setAppearance("dark", "modern");
  await vi.waitFor(() => expect(terminals[0].options.theme?.background).toBe(token("term-bg")));

  await vi.waitFor(() => expect(programColors()).toEqual(["#102030", "#123456"]));

  // The reset is still queued when the palette changes.
  bridge.output(session, utf8("\x1b]111\x07"));
  setAppearance("dark", "default");
  await vi.waitFor(() => expect(terminals[0].options.theme?.background).toBe(token("term-bg")));
  await vi.waitFor(() => expect(programColors()).toEqual([token("term-bg"), "#123456"]));
});

test("a palette slot outside 0 to 255 is not kept across a change of palette, and 0007 is slot 7", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);
  bridge.output(session, utf8("\x1b]4;256;#123456\x07\x1b]4;x;#123456\x07\x1b]4;0007;#123456\x07"));
  await vi.waitFor(() => expect(xtermCore()._themeService.colors.ansi[7].css).toBe("#123456"));

  setAppearance("dark", "modern");
  await vi.waitFor(() => expect(terminals[0].options.theme?.background).toBe(token("term-bg")));

  await vi.waitFor(() => expect(xtermCore()._themeService.colors.ansi[7].css).toBe("#123456"));
  const { ansi } = xtermCore()._themeService.colors;
  expect(ansi).toHaveLength(256);
  expect(ansi.every((color) => color !== undefined)).toBe(true);
});

test("a reset of palette slot 0007 resets slot 7, so the next palette's color shows", async () => {
  const modernWhite = document.createElement("style");
  modernWhite.textContent = ':root[data-palette="modern"] { --ansi-white: #abcdef; }';
  document.head.append(modernWhite);
  onTestFinished(() => modernWhite.remove());
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);
  bridge.output(session, utf8("\x1b]4;0007;#123456\x07\x1b]104;0007\x07"));
  await vi.waitFor(() => expect(xtermCore()._themeService.colors.ansi[7].css).toBe(token("ansi-white")));

  setAppearance("dark", "modern");

  await vi.waitFor(() => expect(terminals[0].options.theme?.white).toBe("#abcdef"));
  await vi.waitFor(() => expect(xtermCore()._themeService.colors.ansi[7].css).toBe("#abcdef"));
});

test("a change of palette between two halves of a sequence leaves the sequence whole", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);
  bridge.output(session, utf8("\x1b]4;200;#123456\x07\x1b[38;2;255;"));
  await vi.waitFor(() => expect(xtermCore()._themeService.colors.ansi[200].css).toBe("#123456"));

  setAppearance("dark", "modern");
  await vi.waitFor(() => expect(terminals[0].options.theme?.background).toBe(token("term-bg")));
  bridge.output(session, utf8("0;0mhello"));

  await vi.waitFor(() => expect(line(0)).toBe("hello"));
  const cell = terminals[0].buffer.active.getLine(0)!.getCell(0)!;
  expect([cell.isFgRGB(), cell.getFgColor()]).toEqual([true, 0xff0000]);
});

test("a shown panel detaches while the page is hidden, and attaches again when it is visible", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  setPageVisibility("hidden");
  expect(bridge.attached(session)).toBe(false);

  setPageVisibility("visible");
  expect(bridge.attached(session)).toBe(true);
});

test("a panel loads WebGL when first shown and keeps it while hidden", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(false));
  await nextFrame();
  expect(drawLog).toEqual([]);

  rerender(panel(true));
  await vi.waitFor(() => expect(drawLog).toContain("webgl 0"));
  rerender(panel(false));
  rerender(panel(true));
  await nextFrame();
  await nextFrame();

  expect(drawLog.filter((entry) => entry.startsWith("webgl"))).toEqual(["webgl 0"]);
});

test("before a panel makes its WebGL context, the shown panels redraw", async () => {
  const { bridge, session } = await shellSession();
  const other = await bridge.spawnShell(worktree, 80, 24);
  const panels = (secondShown: boolean) => (
    <>
      <TerminalPanel bridge={bridge} session={session} shown />
      <TerminalPanel bridge={bridge} session={other} shown={secondShown} />
    </>
  );
  const { rerender } = render(panels(false));
  await vi.waitFor(() => expect(drawLog).toContain("webgl 0"));
  drawLog.length = 0;

  rerender(panels(true));
  expect(drawLog).not.toContain("webgl 1");
  await vi.waitFor(() => expect(drawLog).toContain("webgl 1"));

  expect(drawLog.indexOf("redraw 0")).toBeLessThan(drawLog.indexOf("webgl 1"));
  expect(drawLog.indexOf("redraw 0")).not.toBe(-1);
});

test("a closed panel detaches", async () => {
  const { bridge, session } = await shellSession();
  const { unmount } = render(<TerminalPanel bridge={bridge} session={session} shown />);

  unmount();

  expect(bridge.attached(session)).toBe(false);
});

test("typing into a session that has ended shows the core's error", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);
  await bridge.kill(session);

  terminals[0].input("x");

  expect((await screen.findByRole("alert")).textContent).toBe(`session ${session} is not running`);
});

test("a panel takes the keyboard when it first opens", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  expect(terminals[0].textarea).toBe(document.activeElement);
});

// WebKit drops the least recently drawn context past 16, and the canvas hears it at once.
function loseWebglContext(term: Terminal) {
  term.element!.querySelector("canvas.webgl")!.dispatchEvent(new Event("webglcontextlost"));
}

test("a shown panel that loses its WebGL context draws with the DOM renderer at once, then loads WebGL again", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);
  await vi.waitFor(() => expect(drawLog).toContain("webgl 0"));
  drawLog.length = 0;

  loseWebglContext(terminals[0]);

  // Without the addon xterm.js draws with its DOM renderer, so the panel never shows a dead canvas.
  expect(drawLog[0]).toBe("dispose 0");
  expect(drawLog).not.toContain("webgl 0");
  await vi.waitFor(() => expect(drawLog.at(-1)).toBe("webgl 0"));
});

test("a hidden panel that loses its WebGL context loads it again only once shown", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(true));
  await vi.waitFor(() => expect(drawLog).toContain("webgl 0"));
  rerender(panel(false));
  drawLog.length = 0;

  loseWebglContext(terminals[0]);
  await nextFrame();
  await nextFrame();
  expect(drawLog).toEqual(["dispose 0"]);

  rerender(panel(true));
  await vi.waitFor(() => expect(drawLog.at(-1)).toBe("webgl 0"));
});

test("a panel hidden before its WebGL loads makes no context until shown", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} />
  );
  const { rerender } = render(panel(true));
  rerender(panel(false));
  await nextFrame();
  await nextFrame();
  expect(drawLog.filter((entry) => entry.startsWith("webgl"))).toEqual([]);

  rerender(panel(true));
  await vi.waitFor(() => expect(drawLog).toContain("webgl 0"));
  await nextFrame();

  expect(drawLog.filter((entry) => entry.startsWith("webgl"))).toEqual(["webgl 0"]);
});

// A key as WebKit reports it on macOS: `key` is what the layout makes of it, `code` the key pressed.
function press(init: KeyboardEventInit & { keyCode: number }) {
  return fireEvent.keyDown(terminals[0].textarea!, init);
}

test("Shift+Enter sends ESC CR, the newline Claude Code takes", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  const shiftEnter = { key: "Enter", code: "Enter", keyCode: 13, charCode: 13, shiftKey: true };
  press(shiftEnter);
  fireEvent.keyPress(terminals[0].textarea!, shiftEnter);

  expect(typed(bridge)).toBe("\x1b\r");
});

test("Option acts as Meta", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  press({ key: "∫", code: "KeyB", keyCode: 66, altKey: true });
  press({ key: "ƒ", code: "KeyF", keyCode: 70, altKey: true });

  expect(typed(bridge)).toBe("\x1bb\x1bf");
});

// macOS starts an accent for Option+E on the US layout even with Option as Meta.
test("after an Option dead key the next key goes out plain, not accented", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  press({ key: "Dead", code: "KeyE", keyCode: 69, altKey: true });
  press({ key: "é", code: "KeyE", keyCode: 69, isComposing: true });

  expect(typed(bridge)).toBe("\x1bee");
});

test("Cmd chords belong to grove, and none reaches the session", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  press({ key: "Enter", code: "Enter", keyCode: 13, metaKey: true });
  press({ key: "Backspace", code: "Backspace", keyCode: 8, metaKey: true });
  press({ key: "k", code: "KeyK", keyCode: 75, metaKey: true });
  press({ key: "j", code: "KeyJ", keyCode: 74, metaKey: true });
  // WebKit's own paste event brings the text, so the key itself is left alone.
  const pasteLeftToWebKit = press({ key: "v", code: "KeyV", keyCode: 86, metaKey: true });

  expect(bridge.writes).toEqual([]);
  expect(pasteLeftToWebKit).toBe(true);
});

test("every other key goes to the session", async () => {
  const { bridge, session } = await shellSession();
  render(<TerminalPanel bridge={bridge} session={session} shown />);

  press({ key: "c", code: "KeyC", keyCode: 67, ctrlKey: true });
  press({ key: "Enter", code: "Enter", keyCode: 13 });
  press({ key: "Escape", code: "Escape", keyCode: 27 });
  press({ key: "ArrowUp", code: "ArrowUp", keyCode: 38 });
  press({ key: "Tab", code: "Tab", keyCode: 9, shiftKey: true });

  expect(typed(bridge)).toBe("\x03\r\x1b\x1b[A\x1b[Z");
});

test("a focus request puts the keyboard in the terminal once shown, and only once", async () => {
  const { bridge, session } = await shellSession();
  const panel = (shown: boolean, focusRequest: number) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} focusRequest={focusRequest} />
  );
  const { rerender } = render(panel(true, 0));
  terminals[0].blur();

  rerender(panel(false, 1));
  expect(document.activeElement).toBe(document.body);
  rerender(panel(true, 1));
  expect(document.activeElement).toBe(terminals[0].textarea);

  terminals[0].blur();
  rerender(panel(false, 1));
  rerender(panel(true, 1));
  expect(document.activeElement).toBe(document.body);
});

async function runSession() {
  const bridge = fakeBridge({ [worktree]: [{ path: worktree, branch: "main", isMain: true }] });
  await bridge.addRepo(worktree);
  await bridge.setRepoOverridesText(worktree, `[[run_session]]\nname = "tests"\ncommand = "cargo test"\n`);
  const session = await bridge.spawnRun(worktree, "tests", 80, 24);
  return { bridge, session };
}

test("an ended session's panel stays attached while hidden, so it gets all its output, and resizes no more", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const { bridge, session } = await runSession();
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} ended />
  );
  const { rerender } = render(<TerminalPanel bridge={bridge} session={session} shown />);
  bridge.output(session, utf8("ran"));
  await vi.waitFor(() => expect(line(0)).toBe("ran"));
  const attach = vi.spyOn(bridge, "attach");
  const resize = vi.spyOn(bridge, "resize");

  bridge.exit(session);
  rerender(panel(true));
  rerender(panel(false));
  bridge.output(session, utf8(" done"));
  rerender(panel(true));
  await vi.waitFor(() => expect(line(0)).toBe("ran done"));
  resizeTo(800, 400);
  vi.runAllTimers();

  expect(attach).not.toHaveBeenCalled();
  expect(resize).not.toHaveBeenCalled();
});

test("a run session that exits while its panel is hidden shows all its output once shown", async () => {
  const { bridge, session } = await runSession();
  const panel = (shown: boolean, ended: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} ended={ended} />
  );
  const { rerender } = render(panel(true, false));
  bridge.output(session, utf8("running"));
  await vi.waitFor(() => expect(line(0)).toBe("running"));
  rerender(panel(false, false));
  bridge.output(session, utf8(" 12 passed"));
  bridge.exit(session);
  rerender(panel(false, true));

  rerender(panel(true, true));

  await vi.waitFor(() => expect(line(0)).toBe("running 12 passed"));
  expect(screen.queryByRole("alert")).toBeNull();
});

test("a run session that exits before its panel attaches shows its output", async () => {
  const { bridge, session } = await runSession();
  bridge.output(session, utf8("pnpm: command not found"));
  bridge.exit(session);

  render(<TerminalPanel bridge={bridge} session={session} shown ended />);

  await vi.waitFor(() => expect(line(0)).toBe("pnpm: command not found"));
});

test("a stopped run session that a restart brought back shows an empty terminal, not an error", async () => {
  const { bridge } = await runSession();
  const restored = bridge.restoreStoppedRun(worktree, "dev");
  const attach = vi.spyOn(bridge, "attach");

  render(<TerminalPanel bridge={bridge} session={restored} shown ended />);
  await vi.waitFor(() => expect(attach).toHaveBeenCalled());
  await act(async () => {});

  expect(line(0)).toBe("");
  expect(screen.queryByRole("alert")).toBeNull();
});

test("typing into an ended session sends nothing and shows no error", async () => {
  const { bridge, session } = await runSession();
  bridge.exit(session);
  render(<TerminalPanel bridge={bridge} session={session} shown ended />);
  const write = vi.spyOn(bridge, "write");

  terminals[0].input("x");
  await act(async () => {});

  expect(write).not.toHaveBeenCalled();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("an ended session whose attach fails for a reason other than its end shows the error", async () => {
  const { bridge, session } = await runSession();
  bridge.exit(session);
  vi.spyOn(bridge, "attach").mockRejectedValueOnce("the core stopped");

  render(<TerminalPanel bridge={bridge} session={session} shown ended />);

  expect((await screen.findByRole("alert")).textContent).toBe("the core stopped");
});

test("an attach that failed is tried again the next time the panel is shown", async () => {
  const { bridge, session } = await runSession();
  bridge.output(session, utf8("12 passed"));
  bridge.exit(session);
  const attach = vi.spyOn(bridge, "attach").mockRejectedValueOnce("the core stopped");
  const panel = (shown: boolean) => (
    <TerminalPanel bridge={bridge} session={session} shown={shown} ended />
  );
  const { rerender } = render(panel(true));
  await vi.waitFor(() => expect(attach).toHaveBeenCalledTimes(1));
  await act(async () => {});

  rerender(panel(false));
  rerender(panel(true));

  await vi.waitFor(() => expect(line(0)).toBe("12 passed"));
  expect(attach).toHaveBeenCalledTimes(2);
});
