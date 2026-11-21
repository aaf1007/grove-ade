import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { Plus } from "lucide-react";
import { createRef } from "react";
import { afterEach, expect, test } from "vitest";

import type { AgentState as State } from "../bridge";
import { AgentState } from "./AgentState";
import { Button } from "./Button";
import { Card } from "./Card";
import { IconButton } from "./IconButton";
import { Menu, type MenuHandle } from "./Menu";
import { Segmented } from "./Segmented";

afterEach(cleanup);

test.each<[State, string, string]>([
  [{ state: "Working" }, "Working", "lucide-circle-dashed"],
  [{ state: "NeedsInput", kind: "Approval" }, "Approval", "lucide-shield"],
  [{ state: "NeedsInput", kind: "Question" }, "Question", "lucide-message-circle-question-mark"],
  [{ state: "Failed" }, "Failed", "lucide-circle-alert"],
  [{ state: "Done" }, "Done", "lucide-circle-check"],
  [{ state: "Idle" }, "Idle", "lucide-circle-dot"],
  [{ state: "Exited" }, "Exited", "lucide-circle"],
])("%o shows the word %s and its icon", (state, word, icon) => {
  const { container } = render(<AgentState state={state} />);

  expect(container.textContent).toBe(word);
  const svg = container.querySelector("svg")!;
  expect(svg.classList.contains(icon)).toBe(true);
  expect(svg.getAttribute("aria-hidden")).toBe("true");
});

test("the source shows on hover only when asked for, never as text", () => {
  const change = { session: "4", state: "Working", source: "Hook" } as const;
  const { container, rerender } = render(<AgentState state={change} />);
  const state = () => container.firstElementChild as HTMLElement;
  expect(state().title).toBe("");

  rerender(<AgentState state={change} source={change.source} />);
  expect(container.textContent).toBe("Working");
  expect(state().title).toBe("State from Hook");
});

test("a button takes focus and is named by its label", () => {
  render(<Button variant="primary">Add repo</Button>);

  const button = screen.getByRole("button", { name: "Add repo" });
  button.focus();
  expect(document.activeElement).toBe(button);
  expect(button.getAttribute("type")).toBe("button");
});

test("an icon button takes focus and is named by its label, its icon hidden", () => {
  render(<IconButton icon={Plus} label="New worktree in grove" />);

  const button = screen.getByRole("button", { name: "New worktree in grove" });
  button.focus();
  expect(document.activeElement).toBe(button);
  expect(button.querySelector("svg")!.getAttribute("aria-hidden")).toBe("true");
});

test("a segmented switch is a named group whose options take focus and say which is on", () => {
  const picked: string[] = [];
  render(
    <Segmented
      label="Mode"
      options={[
        ["Chat", "Chat"],
        ["Terminal", "Terminal"],
      ]}
      value="Chat"
      onChange={(mode) => picked.push(mode)}
    />,
  );

  const group = screen.getByRole("group", { name: "Mode" });
  const terminal = within(group).getByRole("button", { name: "Terminal" });
  terminal.focus();
  expect(document.activeElement).toBe(terminal);
  expect(within(group).getByRole("button", { name: "Chat" }).getAttribute("aria-pressed")).toBe("true");
  expect(terminal.getAttribute("aria-pressed")).toBe("false");

  fireEvent.click(terminal);
  expect(picked).toEqual(["Terminal"]);
});

test("a menu opens from its named button, focuses its first item, and closes on Escape", () => {
  const picked: string[] = [];
  render(
    <Menu
      label="Actions for feat-36"
      icon={Plus}
      items={[
        { key: "base", label: "Change base branch", onSelect: () => picked.push("base") },
        { key: "delete", label: "Delete", onSelect: () => picked.push("delete") },
      ]}
    />,
  );
  const trigger = screen.getByRole("button", { name: "Actions for feat-36" });
  expect(trigger.getAttribute("aria-haspopup")).toBe("menu");

  fireEvent.click(trigger);
  const menu = screen.getByRole("menu", { name: "Actions for feat-36" });
  expect(document.activeElement).toBe(within(menu).getByRole("menuitem", { name: "Change base branch" }));

  fireEvent.keyDown(document.activeElement!, { key: "ArrowDown" });
  expect(document.activeElement).toBe(within(menu).getByRole("menuitem", { name: "Delete" }));

  fireEvent.keyDown(document.activeElement!, { key: "Escape" });
  expect(screen.queryByRole("menu")).toBeNull();
  expect(document.activeElement).toBe(trigger);
  expect(picked).toEqual([]);
});

test("Tab closes a menu without moving focus itself, and its items are not in the Tab order", () => {
  const picked: string[] = [];
  render(
    <Menu
      label="Open a panel"
      icon={Plus}
      items={[{ key: "Files", label: "Files", shortcut: "F", onSelect: () => picked.push("Files") }]}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Open a panel" }));
  const item = screen.getByRole("menuitem", { name: "Files" });
  expect(item.tabIndex).toBe(-1);

  const tab = fireEvent.keyDown(item, { key: "Tab" });

  expect(tab).toBe(true);
  expect(screen.queryByRole("menu")).toBeNull();
  fireEvent.keyDown(document.body, { key: "f" });
  expect(picked).toEqual([]);
});

test("a menu item's shortcut picks it while the menu is open", () => {
  const picked: string[] = [];
  render(
    <Menu
      label="Open a panel"
      icon={Plus}
      items={[{ key: "Files", label: "Files", checked: true, shortcut: "F", onSelect: () => picked.push("Files") }]}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "Open a panel" }));
  const item = screen.getByRole("menuitemcheckbox", { name: "Files" });
  expect(item.getAttribute("aria-checked")).toBe("true");

  fireEvent.keyDown(document.body, { key: "f" });

  expect(picked).toEqual(["Files"]);
  expect(screen.queryByRole("menu")).toBeNull();
});

test("a full-width menu takes its trigger's attributes, and its owner can open it", () => {
  const menu = createRef<MenuHandle>();
  render(
    <Menu
      ref={menu}
      label="New session"
      icon={Plus}
      items={[{ key: "local", label: "Local checkout", onSelect() {} }]}
      trigger="New session"
      triggerProps={{ className: "nav-row", "aria-keyshortcuts": "Meta+N" }}
    />,
  );
  const trigger = screen.getByRole("button", { name: "New session" });
  expect(trigger.className).toBe("nav-row");
  expect(trigger.getAttribute("aria-keyshortcuts")).toBe("Meta+N");

  act(() => menu.current!.open());
  act(() => menu.current!.open());

  expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Local checkout" }));
  expect(trigger.getAttribute("aria-expanded")).toBe("true");
});

test("a card is a region named by its title", () => {
  render(
    <Card title="Changed files" actions={<Button size="xs">Review diff</Button>}>
      body
    </Card>,
  );

  const card = screen.getByRole("region", { name: "Changed files" });
  expect(within(card).getByRole("button", { name: "Review diff" })).toBeTruthy();
});
