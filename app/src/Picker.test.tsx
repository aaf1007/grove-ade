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

function openPicker() {
  fireEvent.click(screen.getByRole("button", { name: "Agent and model" }));
}

// A popover opened while the models load keeps its height until it closes, so the models fill in
// below the rows already shown and no row moves. The height itself is the stylesheet's.
const held = () => popover().classList.contains("picker-pop-held");

const fiveModels = [0, 1, 2, 3, 4].map((index) => ({ ...claude[1], id: `model-${index}`, name: `Model ${index}` }));

// The picker remembers nothing between loads, so the first load and any later count behave alike.
test.each([
  ["two models", claude],
  ["five models", fiveModels],
  ["one model", claude.slice(0, 1)],
])("the picker's rows do not move when a list of %s arrives", (_, models) => {
  const { rerender } = render(<Picker {...fixedProps} {...loading} />);
  openPicker();
  expect(held()).toBe(true);

  rerender(<Picker {...fixedProps} models={models} choice={{ model: models[0], effort: null }} />);

  expect(held()).toBe(true);
  expect(screen.getByRole("button", { name: (name) => name.startsWith(models.at(-1)!.name) })).toBeTruthy();
});

test("a picker opened once the models are in takes the height of its rows", () => {
  const { rerender } = render(<Picker {...fixedProps} {...loading} />);
  openPicker();
  rerender(<Picker {...fixedProps} {...loaded} />);
  openPicker();

  openPicker();

  expect(held()).toBe(false);
});

test("while the models load, the list has one loading status and nothing else", () => {
  render(<Picker {...fixedProps} {...loading} />);
  openPicker();

  expect(screen.getByRole("status").textContent).toBe("Loading models");
  expect(screen.queryByRole("group", { name: "Effort" })).toBeNull();
  expect(screen.queryByText("Effort")).toBeNull();
});

test("after an agent listed no models, its next load still says it is loading", () => {
  render(<Picker {...fixedProps} models={[]} choice={null} />);
  cleanup();

  render(<Picker {...fixedProps} {...loading} />);
  openPicker();

  expect(screen.getByRole("status").textContent).toBe("Loading models");
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
