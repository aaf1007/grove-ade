import type { LucideIcon } from "lucide-react";
import type { ButtonHTMLAttributes, Ref } from "react";

import "./IconButton.css";

/**
 * A ghost button that shows only an icon. `label` is its accessible name, and its tooltip unless
 * `title` says more. aria-pressed or aria-expanded true gives it a fill.
 */
export function IconButton({
  icon: Icon,
  label,
  size = "sm",
  title,
  className,
  ref,
  ...rest
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children" | "aria-label"> & {
  icon: LucideIcon;
  label: string;
  /** sm is 28px square, xs 24px. */
  size?: "sm" | "xs";
  ref?: Ref<HTMLButtonElement>;
}) {
  return (
    <button
      ref={ref}
      type="button"
      aria-label={label}
      title={title ?? label}
      className={["ui-icon-button", `ui-icon-button-${size}`, className].filter(Boolean).join(" ")}
      {...rest}
    >
      <Icon aria-hidden="true" />
    </button>
  );
}
