import { expect, test, vi } from "vitest";

import { parseFrame, parseSnapshot, tauriBridge, type WarningSent } from "./bridge";

// Stands in for Tauri: `sent` is what the `warnings` command returns, `emit` sends a `warning`
// event while something listens, and `listenFails` makes `listen` reject.
const tauri = vi.hoisted(() => ({
  sent: [] as unknown[],
  listenFails: false,
  emit: undefined as ((payload: unknown) => void) | undefined,
}));
vi.mock("@tauri-apps/api/core", () => ({
  Channel: class {},
  invoke: async (command: string) => {
    if (command !== "warnings") throw `unexpected command ${command}`;
    return tauri.sent;
  },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, handler: (event: { payload: unknown }) => void) => {
    if (event !== "warning") throw `unexpected event ${event}`;
    if (tauri.listenFails) throw "no event plugin";
    tauri.emit = (payload) => handler({ payload });
    return () => (tauri.emit = undefined);
  },
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

test("a warning listener gets the warnings sent before it listened, then the new ones", async () => {
  const atStart: WarningSent = { id: 0, kind: "LoginShellEnv", reason: "timed out" };
  const later: WarningSent = { id: 1, kind: "LoginShellEnv", reason: "no environment" };
  tauri.sent = [atStart];
  const got: WarningSent[] = [];

  const stop = tauriBridge.onWarning((warning) => got.push(warning));
  await vi.waitFor(() => expect(got).toEqual([atStart]));
  tauri.emit!(later);
  stop();
  await vi.waitFor(() => expect(tauri.emit).toBeUndefined());

  expect(got).toEqual([atStart, later]);
});

test("a warning listener still gets the earlier warnings when listening fails", async () => {
  const atStart: WarningSent = { id: 0, kind: "LoginShellEnv", reason: "timed out" };
  tauri.sent = [atStart];
  tauri.listenFails = true;
  const got: WarningSent[] = [];

  const stop = tauriBridge.onWarning((warning) => got.push(warning));
  await vi.waitFor(() => expect(got).toEqual([atStart]));
  stop();
  tauri.listenFails = false;
});
