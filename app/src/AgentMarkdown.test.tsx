import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import { AgentMarkdown } from "./AgentMarkdown";
import { monaco } from "./monaco";

beforeEach(() => {
  stubClipboard();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
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

// The clipboard the copy button writes to. Once code is colored, Monaco's services also write to
// it on each click and key in WebKit, which jsdom claims to be, with a ClipboardItem, which jsdom
// lacks. Monaco cancels the text it promised on the next event, so the item lets that go.
function stubClipboard() {
  const writeText = vi.fn<(text: string) => Promise<void>>(async () => {});
  Object.defineProperty(navigator, "clipboard", { value: { writeText, write: async () => {} }, configurable: true });
  vi.stubGlobal(
    "ClipboardItem",
    class {
      constructor(items: Record<string, Promise<string>>) {
        Object.values(items).forEach((item) => item.catch(() => {}));
      }
    },
  );
  return writeText;
}

test("a fenced code block renders as preformatted code with a copy button that copies it", async () => {
  const writeText = stubClipboard();
  const container = shown("Run this:\n\n```sh\ncargo test -p grove-core\ncargo fmt --check\n```\n\nThen push.");

  const code = container.querySelector("pre > code");
  expect(code?.textContent).toBe("cargo test -p grove-core\ncargo fmt --check\n");
  expect(container.textContent).not.toContain("```");
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Copy code" })));

  expect(writeText).toHaveBeenCalledWith("cargo test -p grove-core\ncargo fmt --check\n");
});

test("a fenced ts block colors its syntax with the syntax tokens", async () => {
  const container = shown('```ts\nconst name: string = "grove";\n```');

  const keyword = await screen.findByText("const");
  expect(keyword.tagName).toBe("SPAN");
  expect(keyword.style.color).toBe("var(--syn-kw)");
  expect(screen.getByText('"grove"').style.color).toBe("var(--syn-str)");
  expect(container.querySelector("pre > code")?.textContent).toBe('const name: string = "grove";\n');
});

test("a block in a language Monaco does not know shows as plain text", async () => {
  const container = shown("```nosuchlang\nconst name = 1;\n```\n\n```ts\nlet ready = true;\n```");
  await screen.findByText("let");

  const plain = container.querySelectorAll("pre > code")[0];
  expect(plain.querySelector("span")).toBeNull();
  expect(plain.textContent).toBe("const name = 1;\n");
});

// Lines of code, then a comment of slashes, `length` characters in all with the final newline.
function codeOfLength(length: number) {
  const lines = "let a = 1;\n".repeat(1817);
  return lines + "/".repeat(length - lines.length - 1) + "\n";
}

test.each([
  [20_000, true],
  [20_001, false],
])("a block of %i characters is colored: %s", async (length, colored) => {
  const container = shown("```ts\n" + codeOfLength(length) + "```\n\n```ts\nconst ready = true;\n```");
  await screen.findByText("const");

  expect(container.querySelectorAll("pre > code")[0].querySelector("span") !== null).toBe(colored);
});

test("a block shown again is not colored again, and shows its colors at once", async () => {
  const tokenize = vi.spyOn(monaco.editor, "tokenize");
  const text = "```ts\nconst shownTwice = 1;\n```";
  shown(text);
  await screen.findByText("const");
  cleanup();

  shown(text);

  expect(screen.getByText("const").style.color).toBe("var(--syn-kw)");
  await act(async () => {});
  expect(tokenize.mock.calls.filter(([code]) => code.includes("shownTwice"))).toHaveLength(1);
});

test.each([
  ["a quote", "> ```ts\n> const quoted = 1;\n\nAfter the quote."],
  ["a list", "- step\n\n  ```ts\n  const listed = 1;\n- next step"],
])("a block closed by the end of %s is colored", async (_, text) => {
  shown(text);

  expect((await screen.findByText("const")).style.color).toBe("var(--syn-kw)");
});

test("code in a language embedded in another is colored the first time", async () => {
  shown("```html\n<script>\nconst inner = 1;\n</script>\n```");

  expect((await screen.findByText("const")).style.color).toBe("var(--syn-kw)");
});

test("the copy button copies the code as written, not the colored markup", async () => {
  const writeText = stubClipboard();
  const code = 'const greeting = `hi <b>${name}</b>`;\nexport default greeting;\n';
  shown("```ts\n" + code + "```");
  await screen.findByText("const");

  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Copy code" })));

  expect(writeText).toHaveBeenCalledWith(code);
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

test.each(["`", "``", "  ``"])(
  "a closing fence that has only partly streamed in (%s) shows no stray backticks",
  (partial) => {
    const container = shown("```ts\nconst done = true;\n" + partial);

    expect(container.querySelector("pre > code")?.textContent).toBe("const done = true;\n");
    expect(container.textContent).not.toContain("`");
  },
);

test("indented code that holds backticks keeps them, shown and copied", async () => {
  const writeText = stubClipboard();
  const container = shown('Before.\n\n    const marker = "```";\n    ``');

  const code = 'const marker = "```";\n``\n';
  expect(container.querySelector("pre > code")?.textContent).toBe(code);
  await act(async () => fireEvent.click(screen.getByRole("button", { name: "Copy code" })));
  expect(writeText).toHaveBeenCalledWith(code);
});

test("a block is colored once its closing fence arrives, not while it streams", async () => {
  const { container, rerender } = render(<AgentMarkdown text={"```ts\nconst a = 1;\n``"} onOpenLink={() => {}} />);
  await act(async () => {});
  expect(container.querySelector("pre span")).toBeNull();

  rerender(<AgentMarkdown text={"```ts\nconst a = 1;\n```"} onOpenLink={() => {}} />);

  expect((await screen.findByText("const")).style.color).toBe("var(--syn-kw)");
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
