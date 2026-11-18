import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { fakeBridge } from "../fakeBridge";
import { Gallery, GALLERY_REPO } from "./Gallery";

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
  await act(async () => render(<Gallery bridge={bridge} />));

  for (const name of ["Button", "IconButton", "AgentState", "Segmented", "PanelTabs", "Menu", "Card", "AgentMarkdown"]) {
    expect(screen.getByRole("region", { name })).toBeTruthy();
  }
  const states = screen.getByRole("region", { name: "AgentState" });
  for (const word of ["Working", "Approval", "Question", "Failed", "Done", "Idle", "Exited"]) {
    expect(within(states).getAllByText(word)).toHaveLength(4);
  }
  expect(document.documentElement.dataset.theme).toBe("dark");

  fireEvent.click(within(screen.getByRole("group", { name: "Theme" })).getByRole("button", { name: "Light" }));

  expect(document.documentElement.dataset.theme).toBe("light");
});
