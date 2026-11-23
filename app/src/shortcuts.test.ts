import { expect, test } from "vitest";

import { keyHint, matchesKeys, type Shortcut, SHORTCUTS } from "./shortcuts";

// Pairs of shortcuts that one key press could fire both of. Every `when` includes the case where
// focus is in no terminal or editor, so two shortcuts overlap whenever their keys are the same.
function conflicts(shortcuts: readonly Shortcut[]): [string, string][] {
  return shortcuts.flatMap((first, index) =>
    shortcuts
      .slice(index + 1)
      .filter((second) => first.keys.toLowerCase() === second.keys.toLowerCase())
      .map((second): [string, string] => [first.id, second.id]),
  );
}

test("no two shortcuts in the list share their keys", () => {
  expect(conflicts(SHORTCUTS)).toEqual([]);
});

test("two shortcuts with the same keys conflict, even written in another case", () => {
  const [first, second] = SHORTCUTS;
  expect(conflicts([first, { ...second, keys: first.keys.toLowerCase() }])).toEqual([[first.id, second.id]]);
});

test("ids are unique, and every shortcut has a label and a group", () => {
  expect(new Set(SHORTCUTS.map((shortcut) => shortcut.id)).size).toBe(SHORTCUTS.length);
  for (const shortcut of SHORTCUTS) {
    expect(shortcut.label).not.toBe("");
    expect(shortcut.group).not.toBe("");
  }
});

test("a hint writes the keys as macOS shows them", () => {
  expect(keyHint("Meta+N")).toBe("⌘N");
  expect(keyHint("Meta+/")).toBe("⌘/");
  expect(keyHint("Shift+Meta+K")).toBe("⇧⌘K");
  expect(keyHint("Escape")).toBe("Esc");
  expect(keyHint("1-9")).toBe("1 to 9");
});

test("keys match only their own key with exactly their modifiers", () => {
  const press = (init: KeyboardEventInit) => new KeyboardEvent("keydown", init);
  expect(matchesKeys("Meta+N", press({ key: "n", metaKey: true }))).toBe(true);
  expect(matchesKeys("Meta+N", press({ key: "N", metaKey: true }))).toBe(true);
  expect(matchesKeys("Meta+N", press({ key: "n" }))).toBe(false);
  expect(matchesKeys("Meta+N", press({ key: "n", ctrlKey: true }))).toBe(false);
  expect(matchesKeys("Meta+N", press({ key: "n", metaKey: true, shiftKey: true }))).toBe(false);
  expect(matchesKeys("Meta+N", press({ key: "m", metaKey: true }))).toBe(false);
  expect(matchesKeys("Meta+/", press({ key: "/", metaKey: true }))).toBe(true);
});

test("a key that some layouts type with Shift, such as / on a German keyboard, matches with Shift too", () => {
  const press = (init: KeyboardEventInit) => new KeyboardEvent("keydown", init);
  expect(matchesKeys("Meta+/", press({ key: "/", code: "Digit7", metaKey: true, shiftKey: true }))).toBe(true);
  // A letter's Shift is still its own key: Cmd-Shift-N is not Cmd-N.
  expect(matchesKeys("Meta+N", press({ key: "N", metaKey: true, shiftKey: true }))).toBe(false);
});

test("1-9 matches any one of those digits, typed with Shift or not, and no other key", () => {
  const press = (init: KeyboardEventInit) => new KeyboardEvent("keydown", init);
  expect(matchesKeys("1-9", press({ key: "1" }))).toBe(true);
  expect(matchesKeys("1-9", press({ key: "9" }))).toBe(true);
  // Such as a French keyboard, where the digits take Shift.
  expect(matchesKeys("1-9", press({ key: "4", shiftKey: true }))).toBe(true);
  expect(matchesKeys("1-9", press({ key: "0" }))).toBe(false);
  expect(matchesKeys("1-9", press({ key: "!", shiftKey: true }))).toBe(false);
  expect(matchesKeys("1-9", press({ key: "1", metaKey: true }))).toBe(false);
  expect(matchesKeys("1-9", press({ key: "F1" }))).toBe(false);
});
