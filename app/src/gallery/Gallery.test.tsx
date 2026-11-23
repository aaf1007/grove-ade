import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { fakeBridge } from "../fakeBridge";
import type { MonacoDiffProps } from "../MonacoDiff";
import { tokensInDocument } from "../tokensInDocument";
import { addGallerySessions, Gallery, GALLERY_DIFFS, GALLERY_REPO } from "./Gallery";

// Monaco needs a real browser. The stub only shows the new text.
vi.mock("../MonacoDiff", () => ({
  MonacoDiff: ({ modified }: MonacoDiffProps) => <pre aria-label="Modified">{modified}</pre>,
}));

let removeTokens = () => {};

afterEach(() => {
  cleanup();
  removeTokens();
  vi.unstubAllGlobals();
});

test("the gallery shows every component, every agent state, and switches between light and dark", async () => {
  // jsdom has no ResizeObserver, which the pane's composer uses, nor the IntersectionObserver of
  // the Diff panel.
  const observer = class {
    observe() {}
    disconnect() {}
  };
  vi.stubGlobal("ResizeObserver", observer);
  vi.stubGlobal("IntersectionObserver", observer);
  // The inline page takes its theme from the tokens.
  removeTokens = tokensInDocument();
  const bridge = fakeBridge(GALLERY_REPO, GALLERY_DIFFS);
  await bridge.addRepo(Object.keys(GALLERY_REPO)[0]);
  await addGallerySessions(bridge);
  await act(async () => render(<Gallery bridge={bridge} />));

  for (const name of ["Button", "IconButton", "AgentState", "Segmented", "PanelTabs", "Menu", "Card", "AgentMarkdown", "DetailsCard", "DiffView", "InlinePage"]) {
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
  // One Diff panel per base, each opened on its own, and one for a plain folder.
  const diffs = screen.getByRole("region", { name: "DiffView" });
  const pressed = () =>
    within(diffs)
      .getAllByRole("group", { name: "Diff base" })
      .map((bases) => within(bases).getByRole("button", { pressed: true }).textContent);
  expect(pressed()).toEqual(["HEAD", "Merge base"]);
  const changed = () =>
    within(diffs)
      .getAllByRole("list", { name: "Changed files" })
      .map((files) => within(files).queryAllByRole("listitem").map((file) => file.getAttribute("aria-label")));
  await waitFor(() => expect(changed()).toEqual([["src/lexer.rs"], ["src/lexer.rs", "tests/lexer.rs"], []]));
  expect(await within(diffs).findByText("This folder is not a git repo, so it has no diff.")).toBeTruthy();
  // The sidebar lists the gallery's sessions, the ended one dimmed.
  const ended = await screen.findByText("Spike the port router", { selector: ".session-title" });
  expect(ended.closest(".session-item")!.classList.contains("dimmed")).toBe(true);

  // The pane's composer lists the session's commands.
  const composer = await screen.findByRole("textbox", { name: "Message" });
  fireEvent.change(composer, { target: { value: "/" } });
  expect(within(screen.getByRole("listbox", { name: "Commands" })).getAllByRole("option")).toHaveLength(5);

  fireEvent.click(within(screen.getByRole("group", { name: "Theme" })).getByRole("button", { name: "Light" }));

  expect(document.documentElement.dataset.theme).toBe("light");
  const page = screen.getByTitle<HTMLIFrameElement>("Agent sessions by state");
  expect(page.getAttribute("sandbox")).toBe("allow-scripts");
  expect(page.srcdoc).toContain("var(--muted)");
});
