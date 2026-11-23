import type { LucideIcon } from "lucide-react";
import { Check } from "lucide-react";
import {
  type ButtonHTMLAttributes,
  type ReactNode,
  type Ref,
  useEffect,
  useEffectEvent,
  useImperativeHandle,
  useRef,
  useState,
} from "react";

import { openedByPointer } from "../motion";
import { IconButton } from "./IconButton";
import "./Menu.css";

export interface MenuItem {
  key: string;
  label: string;
  /** A second line under the label. */
  hint?: string;
  /** Makes it a checkbox item, with a check when true. */
  checked?: boolean;
  /** A key that picks it while the menu is open, wherever focus is. */
  shortcut?: string;
  /** The group it sits in with the items next to it that name the same one. Its name shows over them. */
  group?: string;
  onSelect(): void;
}

/** The items in runs that share a group, in order. */
function byGroup(items: MenuItem[]): { group: string | undefined; items: MenuItem[] }[] {
  const runs: { group: string | undefined; items: MenuItem[] }[] = [];
  for (const item of items) {
    const last = runs.at(-1);
    if (last && last.group === item.group) last.items.push(item);
    else runs.push({ group: item.group, items: [item] });
  }
  return runs;
}

/** What the menu's owner can ask of it. */
export interface MenuHandle {
  /** Opens the list with focus on its first item, as a click on the button does. An open list stays open. */
  open(): void;
}

/**
 * An icon button that opens a list of actions under it. With `trigger`, a full-width button holding
 * it opens the list instead. Opening focuses the first item, Up and Down move through the items,
 * Escape, Tab or a click outside closes it, and an item's shortcut picks it. Focus goes back to the button when the menu closes from the keyboard or a pick.
 * Items that name a group sit together under a rule and the group's name.
 */
export function Menu({
  label,
  icon,
  heading,
  items,
  align = "start",
  trigger,
  triggerProps,
  ref,
}: {
  /** Names the button and the menu. */
  label: string;
  icon: LucideIcon;
  /** A title over the items. */
  heading?: string;
  items: MenuItem[];
  /** Which edge of the button the menu lines up with. */
  align?: "start" | "end";
  /** What a full-width trigger shows, in place of the icon button. */
  trigger?: ReactNode;
  /** More attributes for the full-width trigger. Its own class replaces `ui-menu-trigger`. */
  triggerProps?: Omit<ButtonHTMLAttributes<HTMLButtonElement>, "type" | "onClick">;
  ref?: Ref<MenuHandle>;
}) {
  const [open, setOpen] = useState(false);
  const [entering, setEntering] = useState(false);
  const holder = useRef<HTMLSpanElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  useImperativeHandle(ref, () => ({
    open: () => {
      if (!open) setEntering(openedByPointer());
      setOpen(true);
    },
  }));

  // The item with focus goes away, so focus goes back to the button, where a dialog finds it.
  function close() {
    button.current?.focus();
    setOpen(false);
  }

  function toggle() {
    setEntering(!open && openedByPointer());
    setOpen(!open);
  }

  function pick(item: MenuItem) {
    close();
    item.onSelect();
  }

  const onKey = useEffectEvent((event: KeyboardEvent) => {
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    // Tab leaves the menu as it would anywhere else, and the menu closes behind it.
    if (event.key === "Tab") {
      setOpen(false);
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      close();
      return;
    }
    const step = { ArrowDown: 1, ArrowUp: -1 }[event.key];
    if (step !== undefined) {
      const buttons = [...(list.current?.querySelectorAll("button") ?? [])];
      const at = buttons.findIndex((item) => item === document.activeElement);
      if (at === -1) return;
      event.preventDefault();
      buttons[(at + step + buttons.length) % buttons.length].focus();
      return;
    }
    const hit = items.find((item) => item.shortcut?.toUpperCase() === event.key.toUpperCase());
    if (!hit) return;
    event.preventDefault();
    pick(hit);
  });

  // Keys go in the capture phase, so a shortcut works even with focus in a text field, and types
  // nothing there.
  useEffect(() => {
    if (!open) return;
    list.current?.querySelector("button")?.focus();
    const outside = (event: PointerEvent) => {
      if (!holder.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("keydown", onKey, true);
    };
  }, [open]);

  return (
    <span ref={holder} className={trigger === undefined ? "ui-menu" : "ui-menu ui-menu-wide"}>
      {trigger === undefined ? (
        <IconButton
          ref={button}
          icon={icon}
          label={label}
          size="xs"
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={toggle}
        />
      ) : (
        <button
          className="ui-menu-trigger"
          {...triggerProps}
          ref={button}
          type="button"
          aria-label={label}
          aria-haspopup="menu"
          aria-expanded={open}
          onClick={toggle}
        >
          {trigger}
        </button>
      )}
      {open && (
        <div
          ref={list}
          className={`ui-menu-list ui-menu-list-${align}${entering ? " popover-enter" : ""}`}
          role="menu"
          aria-label={label}
        >
          {heading && <div className="ui-menu-heading">{heading}</div>}
          {byGroup(items).map(({ group, items: run }) => {
            const buttons = run.map((item) => (
              <button
                key={item.key}
                type="button"
                className="ui-menu-item"
                tabIndex={-1}
                role={item.checked === undefined ? "menuitem" : "menuitemcheckbox"}
                aria-checked={item.checked}
                aria-label={item.label}
                aria-keyshortcuts={item.shortcut}
                onClick={() => pick(item)}
              >
                {item.checked !== undefined && (
                  <span className="ui-menu-check">{item.checked && <Check aria-hidden="true" />}</span>
                )}
                <span className="ui-menu-label">
                  {item.label}
                  {item.hint && <span className="ui-menu-hint">{item.hint}</span>}
                </span>
                {item.shortcut && <kbd>{item.shortcut}</kbd>}
              </button>
            ));
            if (group === undefined) return buttons;
            return (
              <div key={run[0].key} className="ui-menu-group" role="group" aria-label={group}>
                {/* A screen reader says the group's name, so it skips this copy. */}
                <div className="ui-menu-heading" aria-hidden="true">
                  {group}
                </div>
                {buttons}
              </div>
            );
          })}
        </div>
      )}
    </span>
  );
}
