import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { fakeBridge } from "../fakeBridge";
import { addGallerySessions, Gallery, GALLERY_REPO } from "./Gallery";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

test("the gallery shows every component, every agent state, and switches between light and dark", async () => {
  // jsdom has no ResizeObserver, which the pane's composer uses.
  vi.stubGlobal("ResizeObserver", class {
    observe() {}
    disconnect() {}
  });
  const bridge = fakeBridge(GALLERY_REPO);
  await bridge.addRepo(Object.keys(GALLERY_REPO)[0]);
  await addGallerySessions(bridge);
  await act(async () => render(<Gallery bridge={bridge} />));

  for (const name of ["Button", "IconButton", "AgentState", "Segmented", "PanelTabs", "Menu", "Card", "AgentMarkdown", "DetailsCard"]) {
    expect(screen.getByRole("region", { name })).toBeTruthy();
  }
  const states = screen.getByRole("region", { name: "AgentState" });
  for (const word of ["Working", "Approval", "Question", "Failed", "Done", "Idle", "Exited"]) {
    expect(within(states).getAllByText(word)).toHaveLength(1);
  }
  expect(document.documentElement.dataset.theme).toBe("dark");
  // One details card per agent: Claude's plan windows, and Codex's last good reading with its age.
  const cards = await within(screen.getByRole("region", { name: "DetailsCard" })).findAllByRole("region", { name: "Details" });
  expect(cards).toHaveLength(2);
  expect(within(cards[0]).getByRole("listitem", { name: "Usage limits" }).textContent).toContain("5-hour");
  expect(within(cards[0]).getByRole("listitem", { name: "Context window" }).textContent).toContain("42%");
  expect(within(cards[1]).getByRole("listitem", { name: "Usage limits" }).textContent).toContain("last read 12 min ago");
  // The sidebar lists the gallery's sessions, the ended one dimmed.
  const ended = await screen.findByText("Spike the port router", { selector: ".session-title" });
  expect(ended.closest(".session-item")!.classList.contains("dimmed")).toBe(true);

  // The pane's composer lists the session's commands.
  const composer = await screen.findByRole("textbox", { name: "Message" });
  fireEvent.change(composer, { target: { value: "/" } });
  expect(within(screen.getByRole("listbox", { name: "Commands" })).getAllByRole("option")).toHaveLength(5);

  fireEvent.click(within(screen.getByRole("group", { name: "Theme" })).getByRole("button", { name: "Light" }));

  expect(document.documentElement.dataset.theme).toBe("light");
});
