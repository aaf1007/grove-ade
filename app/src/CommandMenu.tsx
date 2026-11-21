import { type KeyboardEvent, type ReactNode, useId, useState } from "react";

import type { AgentMessage, SlashCommand } from "./bridge";
import "./ui/Menu.css";
import "./CommandMenu.css";

/**
 * `messages` without the command lists among them. The last list, if any, goes to `onCommands`.
 * A list is never part of the conversation.
 */
export function takeCommands(
  messages: AgentMessage[],
  onCommands: (commands: SlashCommand[]) => void,
): Exclude<AgentMessage, { type: "Commands" }>[] {
  let latest: SlashCommand[] | null = null;
  const rest: Exclude<AgentMessage, { type: "Commands" }>[] = [];
  for (const message of messages) {
    if (message.type === "Commands") latest = message.commands;
    else rest.push(message);
  }
  if (latest) onCommands(latest);
  return rest;
}

/** What the composer needs from the command menu. */
interface CommandMenu {
  /** The listbox, to go right above the composer, and its spoken row. */
  menu: ReactNode;
  /** The composer's attributes that tie it to the listbox. */
  composerProps: {
    "aria-autocomplete": "list";
    "aria-controls"?: string;
    "aria-activedescendant"?: string;
  };
  /** Takes the key when the menu is open and the key is one of its own. True when it took it. */
  onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>): boolean;
}

/**
 * The `/` menu of the composer. A draft that is `/` and then no space opens it, listing the
 * commands whose names hold the text after the `/`, those that start with it first. Up and Down
 * move, Enter or Tab picks, and Escape closes it until the draft changes, so a new `/` draft opens it again. Picking gives `onPick`
 * the command's invocation and a space.
 */
export function useCommandMenu(commands: SlashCommand[], draft: string, onPick: (text: string) => void): CommandMenu {
  const id = useId();
  // The draft Escape closed the menu on, until the draft changes.
  const [dismissed, setDismissed] = useState<string | null>(null);
  if (dismissed !== null && dismissed !== draft) setDismissed(null);
  // The selected row, for the query it was chosen in. A new query starts at the first row.
  const [active, setActive] = useState({ query: "", index: 0 });

  const query = /^\/(\S*)$/.exec(draft)?.[1];
  const shown = query === undefined || dismissed === draft ? [] : matching(commands, query);
  const open = shown.length > 0;
  const index = active.query === query ? Math.min(active.index, shown.length - 1) : 0;
  const selected = open ? shown[index] : undefined;

  function pick(command: SlashCommand) {
    onPick(`${command.invocation} `);
  }

  function onKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (!selected || query === undefined || event.nativeEvent.isComposing) return false;
    if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return false;
    const step = { ArrowDown: 1, ArrowUp: -1 }[event.key];
    if (step !== undefined) {
      setActive({ query, index: (index + step + shown.length) % shown.length });
    } else if (event.key === "Enter" || event.key === "Tab") {
      pick(selected);
    } else if (event.key === "Escape") {
      // Nothing behind the composer closes on this Escape too.
      event.stopPropagation();
      setDismissed(draft);
    } else {
      return false;
    }
    event.preventDefault();
    return true;
  }

  const option = (i: number) => `${id}-${i}`;
  const menu = (
    <>
      {open && (
        <div id={id} className="ui-menu-list command-menu" role="listbox" aria-label="Commands">
          {shown.map((command, i) => (
            <div
              key={command.invocation}
              id={option(i)}
              className="ui-menu-item"
              role="option"
              aria-selected={i === index}
              // Focus stays in the composer.
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => pick(command)}
            >
              <span className="ui-menu-label">
                <span className="command-name">{command.invocation}</span>
                {command.argumentHint && <span className="command-args">{command.argumentHint}</span>}
                {command.description && <span className="ui-menu-hint">{command.description}</span>}
              </span>
            </div>
          ))}
        </div>
      )}
      <div role="status" className="visually-hidden">
        {selected && [selected.invocation, selected.description].filter(Boolean).join(", ")}
      </div>
    </>
  );

  return {
    menu,
    composerProps: {
      "aria-autocomplete": "list",
      "aria-controls": open ? id : undefined,
      "aria-activedescendant": open ? option(index) : undefined,
    },
    onKeyDown,
  };
}

/** The commands whose names hold `query`, ignoring case, those that start with it first. */
function matching(commands: SlashCommand[], query: string): SlashCommand[] {
  const wanted = query.toLowerCase();
  const holds = commands.filter((c) => c.name.toLowerCase().includes(wanted));
  const starts = holds.filter((c) => c.name.toLowerCase().startsWith(wanted));
  return [...starts, ...holds.filter((c) => !starts.includes(c))];
}
