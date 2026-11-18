import { type ReactNode, useId } from "react";

import "./Card.css";

/**
 * A bordered surface with a head: its title, then its actions on the right. A region named by its
 * title, so it can be found by name.
 */
export function Card({
  title,
  actions,
  children,
}: {
  title: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
}) {
  const titleId = useId();
  return (
    <section className="ui-card" aria-labelledby={titleId}>
      <div className="ui-card-head">
        <span id={titleId} className="ui-card-title">
          {title}
        </span>
        {actions}
      </div>
      {children !== undefined && <div className="ui-card-body">{children}</div>}
    </section>
  );
}
