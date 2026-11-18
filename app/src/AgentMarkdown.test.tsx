import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";

import { AgentMarkdown } from "./AgentMarkdown";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function shown(text: string, onOpenLink: (link: string) => void = () => {}) {
  return render(<AgentMarkdown text={text} onOpenLink={onOpenLink} />).container;
}

test("a list renders as list items", () => {
  shown("Steps:\n\n- add the test\n- make it pass\n\n1. first\n2. second");

  const lists = screen.getAllByRole("list");
  expect(lists.map((list) => list.tagName)).toEqual(["UL", "OL"]);
  expect(within(lists[0]).getAllByRole("listitem").map((item) => item.textContent)).toEqual([
    "add the test",
    "make it pass",
  ]);
});

test("a fenced code block renders as preformatted code with a copy button that copies it", async () => {
  const writeText = vi.fn(async () => {});
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
  const container = shown("Run this:\n\n```sh\ncargo test -p grove-core\ncargo fmt --check\n```\n\nThen push.");

  const code = container.querySelector("pre > code");
  expect(code?.textContent).toBe("cargo test -p grove-core\ncargo fmt --check\n");
  expect(container.textContent).not.toContain("```");
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Copy code" })));

  expect(writeText).toHaveBeenCalledWith("cargo test -p grove-core\ncargo fmt --check\n");
});

test("clicking a link hands it to onOpenLink and does not navigate the window", () => {
  const opened: string[] = [];
  shown("See [the docs](https://docs.rs/git2) for more.", (link) => opened.push(link));

  const link = screen.getByRole("link", { name: "the docs" });
  const navigated = fireEvent.click(link);

  expect(link.getAttribute("href")).toBe("https://docs.rs/git2");
  expect(navigated).toBe(false);
  expect(opened).toEqual(["https://docs.rs/git2"]);
});

test("a table renders as a table with its header and rows", () => {
  shown("| Gate | Result |\n| --- | --- |\n| clippy | pass |\n| vitest | fail |");

  const table = screen.getByRole("table");
  expect(within(table).getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual(["Gate", "Result"]);
  expect(within(table).getAllByRole("row")).toHaveLength(3);
  expect(within(table).getByRole("cell", { name: "fail" })).toBeTruthy();
});

test("headings, bold, italics, block quotes and inline code render as their elements", () => {
  const container = shown("## Plan\n\nThis is **bold**, *italic* and `code`.\n\n> quoted");

  expect(screen.getByRole("heading", { level: 2, name: "Plan" })).toBeTruthy();
  expect(container.querySelector("strong")?.textContent).toBe("bold");
  expect(container.querySelector("em")?.textContent).toBe("italic");
  expect(container.querySelector("p > code")?.textContent).toBe("code");
  expect(container.querySelector("blockquote")?.textContent?.trim()).toBe("quoted");
});

test("raw HTML in agent text shows as text, never as elements", () => {
  const container = shown('Shipping <b>ship</b> now.\n\n<script>window.hacked = true</script>\n\n<img src=x onerror="alert(1)">');

  expect(container.querySelector("b, script, img")).toBeNull();
  expect(container.textContent).toContain("<b>ship</b>");
  expect(container.textContent).toContain("<script>window.hacked = true</script>");
  expect((window as { hacked?: boolean }).hacked).toBeUndefined();
});

test("a code fence still streaming renders as a code block, not stray backticks", () => {
  const container = shown("Here is the fix:\n\n```rust\nfn main() {\n    println!(");

  expect(container.querySelector("pre > code")?.textContent).toBe("fn main() {\n    println!(\n");
  expect(container.textContent).not.toContain("```");
});

// The stylesheet shows a newline in a paragraph as a break, so a newline after a <br> would show
// as an empty line.
test("a hard line break is one break, and a single newline stays one newline", () => {
  const container = shown("one  \ntwo\\\nthree\nfour");

  expect(container.querySelector("p")!.innerHTML).toBe("one<br>two<br>three\nfour");
});

test("a link the page will not hold still hands what the agent wrote to onOpenLink, so the core can say why it refuses", () => {
  const opened: string[] = [];
  shown("[hosts](file:///etc/hosts) and [run](javascript:alert(1))", (link) => opened.push(link));

  const hosts = screen.getByRole("link", { name: "hosts" });
  const run = screen.getByRole("link", { name: "run" });
  fireEvent.click(hosts);
  fireEvent.click(run);

  expect(hosts.getAttribute("href")).toBe("#");
  expect(run.getAttribute("href")).toBe("#");
  expect(opened).toEqual(["file:///etc/hosts", "javascript:alert(1)"]);
});
