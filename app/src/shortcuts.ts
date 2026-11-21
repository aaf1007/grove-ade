export type ShortcutId = "newWorktree" | "search" | "jump" | "showShortcuts";

/** A key that works anywhere in the window. No shortcut fires while a dialog is open. */
export interface Shortcut {
  id: ShortcutId;
  /** As `aria-keyshortcuts` writes them, such as "Meta+N". */
  keys: string;
  label: string;
  /** The heading Keyboard shortcuts shows it under. */
  group: string;
  /**
   * "anywhere": it comes first, so it fires with focus in a terminal or an editor too.
   * "unclaimed": it fires only when the focused terminal or editor did not take the key itself.
   */
  when: "anywhere" | "unclaimed";
}

export const SHORTCUTS: readonly Shortcut[] = [
  { id: "newWorktree", keys: "Meta+N", label: "New worktree", group: "Worktrees", when: "unclaimed" },
  { id: "search", keys: "Meta+K", label: "Search", group: "Worktrees", when: "unclaimed" },
  // Plain Ctrl-J is a newline in a terminal, so Cmd-J is free to come first.
  { id: "jump", keys: "Meta+J", label: "Needs you", group: "Agent sessions", when: "anywhere" },
  { id: "showShortcuts", keys: "Meta+/", label: "Keyboard shortcuts", group: "Help", when: "unclaimed" },
];

export function shortcut(id: ShortcutId): Shortcut {
  const found = SHORTCUTS.find((entry) => entry.id === id);
  if (!found) throw new Error(`no shortcut ${id}`);
  return found;
}

const MODIFIERS = [
  ["Control", "ctrlKey", "⌃"],
  ["Alt", "altKey", "⌥"],
  ["Shift", "shiftKey", "⇧"],
  ["Meta", "metaKey", "⌘"],
] as const;

function parse(keys: string): { modifiers: string[]; key: string } {
  const parts = keys.split("+");
  return { modifiers: parts.slice(0, -1), key: parts[parts.length - 1] };
}

/** The keys as macOS menus show them, such as "⌘N". */
export function keyHint(keys: string): string {
  const { modifiers, key } = parse(keys);
  return MODIFIERS.filter(([name]) => modifiers.includes(name)).map(([, , symbol]) => symbol).join("") + key.toUpperCase();
}

/** Whether `event` is the key with exactly the modifiers `keys` names. */
export function matchesKeys(keys: string, event: KeyboardEvent): boolean {
  const { modifiers, key } = parse(keys);
  return (
    event.key.toLowerCase() === key.toLowerCase() &&
    MODIFIERS.every(([name, flag]) => event[flag] === modifiers.includes(name))
  );
}
