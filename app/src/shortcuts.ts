interface ShortcutText {
  /** As `aria-keyshortcuts` writes them, such as "Meta+N". */
  keys: string;
  label: string;
  /** The heading Keyboard shortcuts shows it under. */
  group: string;
}

/** A key that works anywhere in the window. */
export interface WindowShortcut extends ShortcutText {
  id: "newSession" | "search" | "jump" | "showShortcuts";
  /**
   * "anywhere": it comes first, so it fires with focus in a terminal or an editor too.
   * "unclaimed": it fires only when the focused terminal or editor did not take the key itself.
   */
  when: "anywhere" | "unclaimed";
}

/** A key that works with focus in an agent panel, on that panel's agent session. */
export interface PanelShortcut extends ShortcutText {
  id: "stopTurn";
  when: "agentPanel";
}

/** No shortcut fires while a dialog is open. */
export type Shortcut = WindowShortcut | PanelShortcut;
export type ShortcutId = Shortcut["id"];

export const SHORTCUTS: readonly Shortcut[] = [
  { id: "newSession", keys: "Meta+N", label: "New session", group: "Agent sessions", when: "unclaimed" },
  { id: "search", keys: "Meta+K", label: "Search", group: "Agent sessions", when: "unclaimed" },
  // Plain Ctrl-J is a newline in a terminal, so Cmd-J is free to come first.
  { id: "jump", keys: "Meta+J", label: "Needs you", group: "Agent sessions", when: "anywhere" },
  // In chat mode only. In terminal mode Esc belongs to the agent's TUI.
  { id: "stopTurn", keys: "Escape", label: "Stop the turn", group: "Turn and composer", when: "agentPanel" },
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

// Keys that macOS shows by another name than the one `aria-keyshortcuts` gives them.
const KEY_NAMES: Record<string, string> = { Escape: "Esc" };

/** The keys as macOS menus show them, such as "⌘N". */
export function keyHint(keys: string): string {
  const { modifiers, key } = parse(keys);
  const symbols = MODIFIERS.filter(([name]) => modifiers.includes(name)).map(([, , symbol]) => symbol);
  return symbols.join("") + (KEY_NAMES[key] ?? key.toUpperCase());
}

/**
 * Whether `event` is the key with exactly the modifiers `keys` names. Shift counts only for a
 * letter: some layouts need it to type a sign such as /, and then the key itself says which sign.
 */
export function matchesKeys(keys: string, event: KeyboardEvent): boolean {
  const { modifiers, key } = parse(keys);
  const isLetter = key.toLowerCase() !== key.toUpperCase();
  return (
    event.key.toLowerCase() === key.toLowerCase() &&
    MODIFIERS.every(
      ([name, flag]) => (name === "Shift" && !isLetter) || event[flag] === modifiers.includes(name),
    )
  );
}

/**
 * The shortcut of the list that `event` presses where `when` holds. None once something took the
 * key, while an input method composes with it, or while a dialog is open.
 */
export function pressedShortcut(event: KeyboardEvent, when: WindowShortcut["when"]): WindowShortcut | undefined;
export function pressedShortcut(event: KeyboardEvent, when: PanelShortcut["when"]): PanelShortcut | undefined;
export function pressedShortcut(event: KeyboardEvent, when: Shortcut["when"]): Shortcut | undefined {
  if (event.defaultPrevented || event.isComposing || document.querySelector('[aria-modal="true"]')) return undefined;
  return SHORTCUTS.find((shortcut) => shortcut.when === when && matchesKeys(shortcut.keys, event));
}
