interface ShortcutText {
  /** As `aria-keyshortcuts` writes them, such as "Meta+N". "1-9" is any one of those digits. */
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
  id: "stopTurn" | "pickModel" | "pickEffort" | "switchMode";
  when: "agentPanel";
}

/** A key that works while the agent and model picker is open on one of its lists. */
export interface PickerShortcut extends ShortcutText {
  id: "pickItem";
  when: "pickerList";
}

/** No shortcut fires while a dialog is open. */
export type Shortcut = WindowShortcut | PanelShortcut | PickerShortcut;
export type ShortcutId = Shortcut["id"];

const TURN = "Turn and composer";

export const SHORTCUTS: readonly Shortcut[] = [
  { id: "newSession", keys: "Meta+N", label: "New session", group: "Agent sessions", when: "unclaimed" },
  { id: "search", keys: "Meta+K", label: "Search", group: "Agent sessions", when: "unclaimed" },
  // Plain Ctrl-J is a newline in a terminal, so Cmd-J is free to come first.
  { id: "jump", keys: "Meta+J", label: "Needs you", group: "Agent sessions", when: "anywhere" },
  // In chat mode only. In terminal mode Esc belongs to the agent's TUI.
  { id: "stopTurn", keys: "Escape", label: "Stop the turn", group: TURN, when: "agentPanel" },
  { id: "pickModel", keys: "Shift+Meta+I", label: "Open the model list", group: TURN, when: "agentPanel" },
  { id: "pickEffort", keys: "Shift+Meta+E", label: "Open the effort list", group: TURN, when: "agentPanel" },
  { id: "switchMode", keys: "Shift+Meta+M", label: "Switch chat mode and terminal mode", group: TURN, when: "agentPanel" },
  { id: "pickItem", keys: "1-9", label: "Pick an item from the open list", group: TURN, when: "pickerList" },
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

const DIGITS = "1-9";

// Keys a hint shows by another name than the list gives them.
const KEY_NAMES: Record<string, string> = { Escape: "Esc", [DIGITS]: "1 to 9" };

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
  const pressesKey = key === DIGITS ? /^[1-9]$/.test(event.key) : event.key.toLowerCase() === key.toLowerCase();
  return (
    pressesKey &&
    MODIFIERS.every(
      ([name, flag]) => (name === "Shift" && !isLetter) || event[flag] === modifiers.includes(name),
    )
  );
}

/**
 * The shortcut of the list that `event` presses where `when` holds. None once something took the
 * key, or while a dialog is open.
 */
export function pressedShortcut(event: KeyboardEvent, when: WindowShortcut["when"]): WindowShortcut | undefined;
export function pressedShortcut(event: KeyboardEvent, when: PanelShortcut["when"]): PanelShortcut | undefined;
export function pressedShortcut(event: KeyboardEvent, when: Shortcut["when"]): Shortcut | undefined {
  if (event.defaultPrevented || document.querySelector('[aria-modal="true"]')) return undefined;
  return SHORTCUTS.find((shortcut) => shortcut.when === when && matchesKeys(shortcut.keys, event));
}
