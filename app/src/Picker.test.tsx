import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, test } from "vitest";

import { FAKE_MODELS } from "./fakeBridge";
import { type ModelChoice, Picker } from "./Picker";

afterEach(cleanup);

const claude = FAKE_MODELS.Claude;
const fixedProps = { agent: "Claude", agentLocked: false, onAgent() {}, onChoose() {} } as const;
const loaded = { models: claude, choice: { model: claude[0], effort: "high" } satisfies ModelChoice };
const loading = { models: null, choice: null };

function popover() {
  return screen.getByRole("dialog", { name: "Agent and model" });
}

// What sets each row's height, top to bottom: a heading, a row with or without a second line, or
// the effort switch with its options.
function rows(popover: HTMLElement): string[] {
  return [...popover.children].map((row) => {
    if (row.tagName === "H4") return `heading ${row.textContent}`;
    if (row.classList.contains("opt")) return row.querySelector(".why") ? "row with hint" : "row";
    if (row.classList.contains("ui-segmented")) return `switch ${row.textContent}`;
    return row.className;
  });
}

test("the picker's rows do not move between loading and loaded", () => {
  // A session before this one listed Claude's models, so the picker knows the room they take.
  render(<Picker {...fixedProps} {...loaded} />);
  cleanup();

  const { rerender } = render(<Picker {...fixedProps} {...loading} />);
  fireEvent.click(screen.getByRole("button", { name: "Agent and model" }));
  const whileLoading = rows(popover());
  expect(popover().textContent).toContain("Loading models");

  rerender(<Picker {...fixedProps} {...loaded} />);

  expect(rows(popover())).toEqual(whileLoading);
  expect(whileLoading).toContain("heading Effort");
});

test("the picker opened by a pointer plays its enter transition, and by a key it does not", () => {
  render(<Picker {...fixedProps} {...loaded} />);
  const button = screen.getByRole("button", { name: "Agent and model" });

  fireEvent.pointerDown(button);
  fireEvent.click(button);
  expect(popover().classList.contains("popover-enter")).toBe(true);
  fireEvent.click(button);

  fireEvent.keyDown(button, { key: "Enter" });
  fireEvent.click(button);
  expect(popover().classList.contains("popover-enter")).toBe(false);
});

test("a click inside the open picker does not play its enter transition again", () => {
  render(<Picker {...fixedProps} {...loaded} />);
  const button = screen.getByRole("button", { name: "Agent and model" });
  fireEvent.keyDown(button, { key: "Enter" });
  fireEvent.click(button);

  const low = screen.getByRole("button", { name: "low" });
  fireEvent.pointerDown(low);
  fireEvent.click(low);

  expect(popover().classList.contains("popover-enter")).toBe(false);
});
