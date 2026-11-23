import { expect, test, vi } from "vitest";

import { parseFrame, parseSnapshot, tauriBridge, type WarningSent } from "./bridge";

// The warnings command reads the active list. The event requests a fresh read.
const tauri = vi.hoisted(() => ({
  sent: [] as unknown[] | Promise<unknown[]>,
  listenFails: false,
  read: vi.fn(),
  emit: undefined as ((payload: unknown) => void) | undefined,
}));
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class {},
  invoke: async (command: string) => {
    if (command !== "warnings") throw `unexpected command ${command}`;
    tauri.read();
    return tauri.sent;
  },
}));
// Stands in for the dialog plugin: `picked` is what the folder picker returns.
const dialog = vi.hoisted(() => ({ picked: null as string | null, options: [] as unknown[] }));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: async (options: unknown) => {
    dialog.options.push(options);
    return dialog.picked;
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, handler: (event: { payload: unknown }) => void) => {
    if (event !== "warnings-changed") throw `unexpected event ${event}`;
    if (tauri.listenFails) throw "no event plugin";
    tauri.emit = (payload) => handler({ payload });
    return () => (tauri.emit = undefined);
  },
}));

// Stands in for the window: `fullscreen` is what it reports, `resized` fires its resize listener.
const tauriWindow = vi.hoisted(() => ({ fullscreen: false, resized: undefined as (() => void) | undefined }));
vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: () => ({
    isFullscreen: async () => tauriWindow.fullscreen,
    onResized: async (handler: () => void) => {
      tauriWindow.resized = handler;
      return () => (tauriWindow.resized = undefined);
    },
  }),
}));

test("a frame from the core is its seq in 8 big-endian bytes, then its output", () => {
  const body = new Uint8Array([0, 0, 0, 0, 0, 0, 1, 2, 0x68, 0x69, 0xe6]).buffer;

  const frame = parseFrame(body);

  expect(frame.seq).toBe(258);
  expect([...frame.bytes]).toEqual([0x68, 0x69, 0xe6]);
});

test("a snapshot from the core is its seq in 8 big-endian bytes, its cols and rows in 2 each, then its bytes", () => {
  const body = new Uint8Array([0, 0, 0, 0, 0, 0, 1, 2, 1, 44, 0, 24, 0x1b, 0x5b, 0x48]).buffer;

  const snapshot = parseSnapshot(body);

  expect([snapshot.seq, snapshot.cols, snapshot.rows]).toEqual([258, 300, 24]);
  expect([...snapshot.bytes]).toEqual([0x1b, 0x5b, 0x48]);
});

test("a warning listener receives the active list at start and after each change", async () => {
  const atStart: WarningSent = { id: 0, kind: "LoginShellEnv", reason: "timed out" };
  const later: WarningSent = { id: 1, kind: "SessionNotRestored", worktree: "/w", reason: "gone" };
  tauri.sent = [atStart];
  const got: WarningSent[][] = [];
  const stop = tauriBridge.onWarnings((warnings) => got.push(warnings));
  await vi.waitFor(() => expect(got).toEqual([[atStart]]));
  tauri.sent = [atStart, later];
  tauri.emit!(null);
  await vi.waitFor(() => expect(got).toEqual([[atStart], [atStart, later]]));
  stop();
  await vi.waitFor(() => expect(tauri.emit).toBeUndefined());
});

test("a clear during the initial warning list read wins over its late reply", async () => {
  tauri.read.mockClear();
  let finish!: (warnings: WarningSent[]) => void;
  tauri.sent = new Promise<WarningSent[]>((resolve) => { finish = resolve; });
  const got: WarningSent[][] = [];
  const stop = tauriBridge.onWarnings((warnings) => got.push(warnings));
  await vi.waitFor(() => expect(tauri.read).toHaveBeenCalledTimes(1));
  tauri.sent = [];
  tauri.emit!(null);
  await vi.waitFor(() => expect(got).toEqual([[]]));
  finish([{ id: 1, kind: "WatcherNotStarted", worktree: "/gone", reason: "missing" }]);
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(got).toEqual([[]]);
  stop();
});

test("a warning listener still gets the earlier warnings when listening fails", async () => {
  const atStart: WarningSent = { id: 0, kind: "LoginShellEnv", reason: "timed out" };
  tauri.sent = [atStart];
  tauri.listenFails = true;
  const got: WarningSent[][] = [];
  const stop = tauriBridge.onWarnings((warnings) => got.push(warnings));
  await vi.waitFor(() => expect(got).toEqual([[atStart]]));
  stop();
  tauri.listenFails = false;
});

test("pickFolder opens the system picker for one folder, and gives its path or null when cancelled", async () => {
  dialog.picked = "/code/grove";
  expect(await tauriBridge.pickFolder()).toBe("/code/grove");

  dialog.picked = null;
  expect(await tauriBridge.pickFolder()).toBeNull();

  expect(dialog.options).toEqual([
    { directory: true, multiple: false, title: "Add repo" },
    { directory: true, multiple: false, title: "Add repo" },
  ]);
});

test("a full screen listener hears the window's state at once, then after each resize", async () => {
  const got: boolean[] = [];

  const stop = tauriBridge.onFullscreenChanged((fullscreen) => got.push(fullscreen));

  await vi.waitFor(() => expect(got).toEqual([false]));
  tauriWindow.fullscreen = true;
  tauriWindow.resized?.();
  await vi.waitFor(() => expect(got).toEqual([false, true]));
  stop();
  await vi.waitFor(() => expect(tauriWindow.resized).toBeUndefined());
});

test("a full screen listener hears nothing once it unsubscribed, even from a check still running", async () => {
  const got: boolean[] = [];

  tauriBridge.onFullscreenChanged((fullscreen) => got.push(fullscreen))();

  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(got).toEqual([]);
});
