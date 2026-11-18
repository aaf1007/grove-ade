import { type ReactNode, useEffect, useRef } from "react";
import { createPortal } from "react-dom";

import { Button } from "./ui/Button";

/**
 * A modal form over the whole window. Escape or a click outside it cancels. It opens with focus
 * on its first control, unless a control took focus itself, and gives focus back to what had it
 * when it closes.
 */
export function Dialog({
  label,
  role = "dialog",
  onSubmit,
  onCancel,
  children,
}: {
  label: string;
  role?: "dialog" | "alertdialog";
  onSubmit(): void;
  onCancel(): void;
  children: ReactNode;
}) {
  const form = useRef<HTMLFormElement>(null);
  // Read on the first render, before a control takes focus.
  const opener = useRef(document.activeElement instanceof HTMLElement ? document.activeElement : null);

  useEffect(() => {
    if (!form.current?.contains(document.activeElement)) {
      form.current?.querySelector<HTMLElement>("input, select, button")?.focus();
    }
    const previous = opener.current;
    return () => {
      if (previous?.isConnected) previous.focus();
    };
  }, []);

  // In the body, so no region's styles or overflow reach it.
  return createPortal(
    <div className="overlay" onPointerDown={(event) => event.target === event.currentTarget && onCancel()}>
      <form
        ref={form}
        className="dialog"
        role={role}
        aria-modal="true"
        aria-label={label}
        onKeyDown={(event) => {
          if (event.key === "Escape") return onCancel();
          if (event.key !== "Tab") return;
          // Tab wraps around the dialog's controls, so it never reaches the window behind it.
          const controls = [
            ...event.currentTarget.querySelectorAll<HTMLElement>(
              "input:not(:disabled), select:not(:disabled), button:not(:disabled)",
            ),
          ];
          const [first, last] = [controls[0], controls.at(-1)];
          const wrapTo = event.shiftKey
            ? document.activeElement === first && last
            : document.activeElement === last && first;
          if (wrapTo) {
            event.preventDefault();
            wrapTo.focus();
          }
        }}
        onSubmit={(event) => {
          event.preventDefault();
          onSubmit();
        }}
      >
        <h3>{label}</h3>
        {children}
      </form>
    </div>,
    document.body,
  );
}

/** Cancel, then the dialog's submit button, which stays off while `busy` or `disabled`. */
export function Actions({
  submit,
  busy = false,
  disabled = false,
  onCancel,
}: {
  /** null leaves only Cancel. */
  submit: string | null;
  busy?: boolean;
  disabled?: boolean;
  onCancel(): void;
}) {
  return (
    <div className="dialog-actions">
      <Button type="button" size="sm" onClick={onCancel}>
        Cancel
      </Button>
      {submit && (
        <Button type="submit" variant="primary" size="sm" disabled={busy || disabled}>
          {submit}
        </Button>
      )}
    </div>
  );
}
