import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, test } from "vitest";

import { Dialog } from "./Dialog";

afterEach(cleanup);

function renderDialog() {
  render(
    <Dialog label="New worktree" onSubmit={() => {}} onCancel={() => {}}>
      <input aria-label="Name" />
    </Dialog>,
  );
  return screen.getByRole("dialog", { name: "New worktree" });
}

test("a dialog opened by a pointer plays its enter transition", () => {
  fireEvent.pointerDown(document.body);

  const dialog = renderDialog();

  expect(dialog.classList.contains("popover-enter")).toBe(true);
  expect(dialog.parentElement!.classList.contains("overlay-enter")).toBe(true);
});

test("a dialog opened by a keyboard shortcut appears at once", () => {
  fireEvent.keyDown(document.body, { key: "n", metaKey: true });

  const dialog = renderDialog();

  expect(dialog.classList.contains("popover-enter")).toBe(false);
  expect(dialog.parentElement!.classList.contains("overlay-enter")).toBe(false);
});
