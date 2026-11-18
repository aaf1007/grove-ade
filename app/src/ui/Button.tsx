import type { LucideIcon } from "lucide-react";
import type { ButtonHTMLAttributes, Ref } from "react";

import "./Button.css";

export type ButtonVariant = "primary" | "outline" | "ghost";

/** md is 32px, sm 28px, xs 24px. Use xs inside the conversation and in card heads. */
export type ButtonSize = "md" | "sm" | "xs";

/**
 * The one way to trigger an action, labeled with a short verb. Use `primary` at most once per
 * screen. It is type="button" unless told otherwise, so it never submits a form by accident.
 */
export function Button({
  variant = "outline",
  size = "md",
  icon: Icon,
  className,
  type = "button",
  ref,
  children,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** Drawn before the label. */
  icon?: LucideIcon;
  ref?: Ref<HTMLButtonElement>;
}) {
  return (
    <button
      ref={ref}
      type={type}
      className={["ui-button", `ui-button-${variant}`, `ui-button-${size}`, className].filter(Boolean).join(" ")}
      {...rest}
    >
      {Icon && <Icon aria-hidden="true" />}
      {children}
    </button>
  );
}
