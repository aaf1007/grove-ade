import type { LucideIcon } from "lucide-react";
import { Check } from "lucide-react";
import { useEffect, useEffectEvent, useRef, useState } from "react";

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
  onSelect(): void;
}

/**
 * An icon button that opens a list of actions under it. Opening focuses the first item, Up and
 * Down move through the items, Escape, Tab or a click outside closes it, and an item's shortcut
 * picks it. Focus goes back to the button when the menu closes from the keyboard or a pick.
 */
export function Menu({
  label,
  icon,
  heading,
  items,
  align = "start",
}: {
  /** Names the button and the menu. */
  label: string;
  icon: LucideIcon;
  /** A title over the items. */
  heading?: string;
  items: MenuItem[];
  /** Which edge of the button the menu lines up with. */
  align?: "start" | "end";
}) {
  const [open, setOpen] = useState(false);
  const holder = useRef<HTMLSpanElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);

  // The item with focus goes away, so focus goes back to the button, where a dialog finds it.
  function close() {
    trigger.current?.focus();
    setOpen(false);
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
      const at = buttons.findIndex((button) => button === document.activeElement);
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
    <span ref={holder} className="ui-menu">
      <IconButton
        ref={trigger}
        icon={icon}
        label={label}
        size="xs"
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      />
      {open && (
        <div ref={list} className={`ui-menu-list ui-menu-list-${align}`} role="menu" aria-label={label}>
          {heading && <div className="ui-menu-heading">{heading}</div>}
          {items.map((item) => (
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
          ))}
        </div>
      )}
    </span>
  );
}
