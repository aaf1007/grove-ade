import "@xterm/xterm/css/xterm.css";

import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";

import type { Bridge, OutputFrame, SessionId, Snapshot } from "./bridge";
import { cellSize } from "./cellSize";
import { buildOrReport, codeFont, onAppearanceChange, terminalTheme } from "./codeTheme";

// How long the panel keeps a size before the terminal and its PTY take it, so dragging the window
// edge resizes once.
const RESIZE_DEBOUNCE_MS = 100;
// xterm.js draws its scrollbar over the right edge, so no column goes under it.
const SCROLLBAR_WIDTH = 14;
// The panel writes this before each snapshot, so it lands after any frame of an earlier attach.
// CAN ends a sequence that frame left unfinished, and RIS resets the terminal. `term.reset()` would
// act before those frames and keep the parser's state. In xterm.js 6.0 RIS keeps a hidden cursor
// hidden and keeps colors set by OSC, and a snapshot never shows the cursor or sets colors. So this
// also shows the cursor and restores the theme's palette (OSC 104), foreground (110), background
// (111) and cursor color (112).
const RESET = new TextEncoder().encode(
  "\x18\x1bc\x1b[?25h\x1b]104\x07\x1b]110\x07\x1b]111\x07\x1b]112\x07",
);
// Shift+Enter sends this, the newline Claude Code takes without the kitty keyboard protocol.
const ESC_CR = "\x1b\r";
const MODIFIER_KEYS = new Set(["Alt", "Shift", "Control"]);
const encoder = new TextEncoder();

/** A session's terminal. It shows the session's output while `shown`. */
export function TerminalPanel({
  bridge,
  session,
  shown,
  ended = false,
  focusRequest = 0,
  onFocus,
  onBlur,
}: {
  bridge: Bridge;
  session: SessionId;
  shown: boolean;
  /**
   * The session's process has ended. Its output has an end, so the panel attaches once, now or
   * when first shown, and stays attached while hidden until it has it all. A session the core
   * kept nothing of shows an empty terminal. Typing and resizing stop.
   */
  ended?: boolean;
  /** Each new number puts the keyboard in the terminal once it is shown. 0 asks for nothing. */
  focusRequest?: number;
  /** Called each time the terminal gets focus. */
  onFocus?: () => void;
  /** Called each time the terminal loses focus, including when the window does. */
  onBlur?: () => void;
}) {
  const host = useRef<HTMLDivElement>(null);
  const panel = useRef<Panel | null>(null);
  const [error, setError] = useState<string | null>(null);
  const focusedRequest = useRef(0);

  useEffect(() => {
    const created = new Panel(bridge, session, host.current!, (e) => setError(String(e)));
    panel.current = created;
    return () => {
      created.dispose();
      panel.current = null;
    };
  }, [bridge, session]);

  // Before `show`, so a panel made for an ended session attaches as one. Also runs after the panel
  // above is made again.
  useEffect(() => {
    if (ended) panel.current?.end();
  }, [bridge, session, ended]);

  // Also runs after the panel above is made again.
  useEffect(() => panel.current?.show(shown), [bridge, session, shown]);

  useEffect(() => {
    if (!shown || focusRequest === 0 || focusRequest === focusedRequest.current) return;
    focusedRequest.current = focusRequest;
    panel.current?.focus();
  }, [focusRequest, shown]);

  return (
    <div className="terminal-panel" hidden={!shown} onFocus={onFocus} onBlur={onBlur}>
      {error && <p role="alert">{error}</p>}
      <div className="terminal" ref={host} />
    </div>
  );
}

// WebKit keeps 16 live WebGL contexts and drops the one drawn least recently (spike #10).
const shownTerminals = new Set<Terminal>();

/** xterm.js in `host`, attached to the session's output while shown. */
class Panel {
  private readonly term = createTerminal();
  private readonly programColors = trackProgramColors(this.term);
  // Shown or hidden, the terminal takes the new colors when the theme or palette changes. It waits
  // for the output already written, so the colors the program set are the ones it set by then.
  private readonly stopFollowingTheme = onAppearanceChange(() => this.term.write("", () => this.applyTheme()));
  private shown = false;
  private opened = false;
  // The session's process ended, so there is nothing to type into or resize.
  private ended = false;
  // The size the panel last took, which its PTY has too.
  private size: { cols: number; rows: number } | undefined;
  // The attach frames go to now. Frames of an earlier one are neither drawn nor acked.
  private attached: { current: boolean } | null = null;
  private readonly sizes = new ResizeObserver((entries) => {
    const box = entries[0].contentRect;
    clearTimeout(this.resizing);
    this.resizing = setTimeout(() => this.fit(box), RESIZE_DEBOUNCE_MS);
  });
  private resizing: ReturnType<typeof setTimeout> | undefined;
  private webgl: WebglAddon | undefined;
  private webglFrame: number | undefined;
  // Without the addon xterm.js draws with its DOM renderer at once, where xterm.js's own
  // `onContextLoss` would leave a dead canvas for 3 s first.
  private readonly webglLost = () => {
    this.webgl?.dispose();
    this.webgl = undefined;
    if (this.shown) this.loadWebgl();
  };
  // An Option dead key started a macOS accent, which the next key must not take.
  private optionDeadKey = false;
  private readonly pageVisibilityChanged = () => this.attachWhileVisible();

  constructor(
    private readonly bridge: Bridge,
    private readonly session: SessionId,
    private readonly host: HTMLElement,
    private readonly fail: (e: unknown) => void,
  ) {
    swallowQueries(this.term);
    this.term.attachCustomKeyEventHandler((event) => this.key(event));
    this.term.onData((data) => this.send(encoder.encode(data)));
    // Mouse reports in the X10 encoding, one byte per character.
    this.term.onBinary((data) => this.send(Uint8Array.from(data, (c) => c.charCodeAt(0))));
    this.sizes.observe(host);
    document.addEventListener("visibilitychange", this.pageVisibilityChanged);
    // The canvas's event does not bubble, so the panel hears it on the way down.
    host.addEventListener("webglcontextlost", this.webglLost, true);
  }

  show(shown: boolean) {
    this.shown = shown;
    if (shown) shownTerminals.add(this.term);
    else shownTerminals.delete(this.term);
    // xterm.js measures its cells when it opens, so it opens only once shown.
    if (shown && !this.opened) {
      this.term.open(this.host);
      this.opened = true;
      this.term.focus();
      this.dropOptionDeadKeyAccents(this.term.textarea!);
    }
    if (shown) this.loadWebgl();
    this.attachWhileVisible();
  }

  focus() {
    this.term.focus();
  }

  end() {
    this.ended = true;
    this.attachWhileVisible();
  }

  dispose() {
    this.stopFollowingTheme();
    document.removeEventListener("visibilitychange", this.pageVisibilityChanged);
    this.host.removeEventListener("webglcontextlost", this.webglLost, true);
    this.sizes.disconnect();
    clearTimeout(this.resizing);
    if (this.webglFrame !== undefined) cancelAnimationFrame(this.webglFrame);
    shownTerminals.delete(this.term);
    this.detach();
    this.term.dispose();
  }

  // A new theme replaces every color, so the colors the program set go back on after it, straight
  // into xterm.js's colors. Bytes written to the terminal would cut a sequence split across frames.
  private applyTheme() {
    const theme = buildOrReport(terminalTheme);
    if (!theme) return;
    const service = themeService(this.term);
    const kept = service ? [...this.programColors].map((slot) => [slot, colorAt(service.colors, slot)] as const) : [];
    this.term.options.theme = theme;
    if (kept.length) {
      service?.modifyColors((current) => {
        for (const [slot, color] of kept) {
          if (!color) continue;
          if (typeof slot === "number") current.ansi[slot] = color;
          else current[slot] = color;
        }
      });
    }
  }

  // Kept while hidden, so showing a panel makes no new context. A disposed context still counts
  // until garbage collection. Only a shown panel makes one, the first time or after a loss.
  private loadWebgl() {
    if (this.webgl || this.webglFrame !== undefined) return;
    // The shown panels draw first, so the context WebKit drops for this one is a hidden panel's.
    for (const term of shownTerminals) term.refresh(0, term.rows - 1);
    this.webglFrame = requestAnimationFrame(() => {
      this.webglFrame = undefined;
      if (!this.shown) return;
      this.webgl = new WebglAddon();
      this.term.loadAddon(this.webgl);
    });
  }

  // Cmd chords belong to grove, so xterm.js leaves them alone. Everything else goes to the PTY.
  private key(event: KeyboardEvent): boolean {
    if (event.metaKey) return false;
    if (event.type === "keydown" && !MODIFIER_KEYS.has(event.key)) {
      // xterm.js sends ESC and the key itself. Only the accent macOS starts needs undoing.
      if (event.altKey && event.key === "Dead") {
        this.optionDeadKey = true;
        return true;
      }
      if (this.optionDeadKey) {
        // Option+E twice starts a new accent.
        this.optionDeadKey = event.altKey && event.isComposing;
        const plain = plainKey(event);
        if (plain !== undefined) {
          event.preventDefault();
          this.send(encoder.encode(event.altKey ? `\x1b${plain}` : plain));
          return false;
        }
      }
    }
    if (event.key === "Enter" && event.shiftKey && !event.altKey && !event.ctrlKey) {
      if (event.type === "keydown") {
        event.preventDefault();
        this.send(encoder.encode(ESC_CR));
      }
      return false;
    }
    return true;
  }

  // Runs before xterm.js reads the composed text, in its own setTimeout 0, so the accent is dropped.
  private dropOptionDeadKeyAccents(textarea: HTMLTextAreaElement) {
    textarea.addEventListener(
      "compositionend",
      () => {
        if (this.optionDeadKey) setTimeout(() => (textarea.value = ""), 0);
      },
      true,
    );
  }

  private fit(box: { width: number; height: number }) {
    const cell = cellSize(this.term);
    if (!cell) return;
    const cols = Math.floor((box.width - SCROLLBAR_WIDTH) / cell.width);
    const rows = Math.floor(box.height / cell.height);
    // A hidden or collapsed panel measures 0x0. xterm.js needs at least 2 columns.
    if (cols < 2 || rows < 1) return;
    this.size = { cols, rows };
    this.term.resize(cols, rows);
    if (!this.ended) this.bridge.resize(this.session, cols, rows).catch(this.fail);
  }

  // Attached only while both the panel and the page are shown. A hidden page draws nothing and runs
  // timers about once a second, so it would ack late and slow the session down (spike #6). An ended
  // session has no process left to slow, so its panel stays attached.
  private attachWhileVisible() {
    if (this.ended) {
      if (this.shown && !this.attached) this.attach();
      return;
    }
    const visible = this.shown && document.visibilityState === "visible";
    if (visible && !this.attached && !this.ended) this.attach();
    if (!visible) this.detach();
  }

  private send(bytes: Uint8Array) {
    if (this.ended) return;
    this.bridge.write(this.session, bytes).catch(this.fail);
  }

  private attach() {
    const attached = { current: true };
    this.attached = attached;
    // Frames that come before the snapshot wait here, since they follow it.
    let early: OutputFrame[] | null = [];
    let snapshotSeq = 0;
    const draw = ({ seq, bytes }: OutputFrame) => {
      if (seq <= snapshotSeq) return;
      this.term.write(bytes, () => {
        if (attached.current) this.bridge.ack(this.session, bytes.length).catch(this.fail);
      });
    };
    this.bridge
      .attach(this.session, (frame) => {
        if (!attached.current) return;
        if (early) early.push(frame);
        else draw(frame);
      })
      .then((snapshot) => {
        if (!attached.current) return;
        this.replay(snapshot);
        snapshotSeq = snapshot.seq;
        early?.forEach(draw);
        early = null;
      })
      .catch((e: unknown) => {
        // Not attached, so the next show tries again.
        attached.current = false;
        if (this.attached === attached) this.attached = null;
        // An ended session the core kept nothing of, such as one a restart brought back, has
        // nothing to show.
        if (this.ended && String(e) === `session ${this.session} is not running`) return;
        this.fail(e);
      });
  }

  // Draws the session's terminal as the snapshot has it, in place of what this one shows.
  private replay({ cols, rows, bytes }: Snapshot) {
    this.term.write(RESET, () => this.term.resize(cols, rows));
    // The panel may have taken a new size while the snapshot was on its way.
    this.term.write(bytes, () => {
      if (this.size) this.term.resize(this.size.cols, this.size.rows);
    });
  }

  private detach() {
    if (!this.attached) return;
    this.attached.current = false;
    this.attached = null;
    this.bridge.detach(this.session).catch(this.fail);
  }
}

// Colors and font come from the tokens, or stay xterm.js's own when a token cannot be read. The
// theme only maps the 16 colors, the ground and the cursor, so what a program prints, such as an
// agent's own interface, is drawn as it asks.
function createTerminal() {
  const font = buildOrReport(codeFont);
  const theme = buildOrReport(terminalTheme);
  return new Terminal({
    scrollback: 5000,
    macOptionIsMeta: true,
    ...(font && { fontFamily: font.fontFamily, fontSize: font.fontSize }),
    ...(theme && { theme }),
  });
}

// The key as the US layout has it, ignoring Option and any accent. undefined for a key it does not
// cover, which xterm.js then handles.
function plainKey(event: KeyboardEvent): string | undefined {
  if (/^Key[A-Z]$/.test(event.code)) {
    const letter = event.code[3];
    return event.shiftKey ? letter : letter.toLowerCase();
  }
  if (/^Digit[0-9]$/.test(event.code) && !event.shiftKey) return event.code[5];
  if (event.code === "Space") return " ";
  return undefined;
}

// The `CSI t` operations that report something.
const WINDOW_REPORTS = new Set([11, 13, 14, 15, 16, 18, 19, 20, 21]);

// The core's headless parser answers every terminal query, attached or not, so xterm.js must answer
// none of the ones it would: each query gets exactly one reply (spike #7).
function swallowQueries(term: Terminal) {
  const parser = term.parser;
  const swallow = () => true;
  parser.registerCsiHandler({ final: "c" }, swallow); // DA1
  parser.registerCsiHandler({ prefix: ">", final: "c" }, swallow); // DA2
  parser.registerCsiHandler({ final: "n" }, swallow); // DSR
  parser.registerCsiHandler({ prefix: "?", final: "n" }, swallow); // DECXCPR
  parser.registerCsiHandler({ intermediates: "$", final: "p" }, swallow); // DECRQM
  parser.registerCsiHandler({ prefix: "?", intermediates: "$", final: "p" }, swallow);
  parser.registerCsiHandler({ final: "t" }, (params) => WINDOW_REPORTS.has(params[0] as number));
  parser.registerDcsHandler({ intermediates: "$", final: "q" }, swallow); // DECRQSS
  // Color queries only: setting a color has no `?`, so xterm.js still applies it.
  for (const color of [4, 10, 11, 12]) {
    parser.registerOscHandler(color, (data) => data.split(";").includes("?"));
  }
}

// A color a program can set: a palette slot (OSC 4), the foreground (10), background (11) or
// cursor (12).
type ColorSlot = number | "foreground" | "background" | "cursor";
const SPECIAL_SLOTS: Record<number, ColorSlot> = { 10: "foreground", 11: "background", 12: "cursor" };

/** The slots the program has set with OSC 4, 10, 11 and 12 and not reset. xterm.js still applies each. */
function trackProgramColors(term: Terminal): Set<ColorSlot> {
  const slots = new Set<ColorSlot>();
  const parser = term.parser;
  parser.registerOscHandler(4, (data) => {
    const parts = data.split(";");
    for (let index = 0; index + 1 < parts.length; index += 2) {
      // xterm.js ignores a slot outside its 256-color palette.
      const slot = /^\d+$/.test(parts[index]) ? Number(parts[index]) : 256;
      if (parts[index + 1] !== "?" && slot < 256) slots.add(slot);
    }
    return false;
  });
  parser.registerOscHandler(104, (data) => {
    const reset = data ? data.split(";").filter((slot) => /^\d+$/.test(slot)).map(Number) : undefined;
    for (const slot of slots) {
      if (typeof slot === "number" && (!reset || reset.includes(slot))) slots.delete(slot);
    }
    return false;
  });
  for (const osc of [10, 11, 12]) {
    // OSC 10 may set 10, 11 and 12 in turn, and OSC 11 may set 11 and 12.
    parser.registerOscHandler(osc, (data) => {
      data.split(";").forEach((spec, offset) => {
        if (spec !== "?" && osc + offset <= 12) slots.add(SPECIAL_SLOTS[osc + offset]);
      });
      return false;
    });
    parser.registerOscHandler(osc + 100, () => {
      slots.delete(SPECIAL_SLOTS[osc]);
      return false;
    });
  }
  return slots;
}

// xterm.js's own color set, which OSC 4, 10, 11 and 12 change and which has no public setter.
// undefined until the terminal opens; until then no program color has been applied.
interface XtermColor {
  css: string;
  rgba: number;
}
interface XtermColors {
  foreground: XtermColor;
  background: XtermColor;
  cursor: XtermColor;
  ansi: XtermColor[];
}
interface XtermThemeService {
  colors: XtermColors;
  modifyColors(change: (colors: XtermColors) => void): void;
}

function themeService(term: Terminal): XtermThemeService | undefined {
  return (term as unknown as { _core: { _themeService?: XtermThemeService } })._core._themeService;
}

function colorAt(colors: XtermColors, slot: ColorSlot): XtermColor {
  return typeof slot === "number" ? colors.ansi[slot] : colors[slot];
}
