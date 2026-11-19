import { fireEvent, screen } from "@testing-library/react";

/** Opens the menu of the worktree named `name` and gives its items' text. Leaves the menu open. */
export function menuItems(name: string) {
  fireEvent.click(screen.getByRole("button", { name: `Actions for ${name}` }));
  return screen.getAllByRole("menuitem").map((item) => item.textContent);
}
