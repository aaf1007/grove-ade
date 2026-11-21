import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, test } from "vitest";

import { type ToolCall, ToolCalls } from "./ToolCalls";

afterEach(cleanup);

const worktree = "/code/grove";

function command(id: string, line: string, result?: ToolCall["result"]): ToolCall {
  return { id, tool: "commandExecution", input: { command: line, cwd: worktree }, result };
}

test.each([
  ["/bin/zsh -lc 'touch qa.txt'", "Ran touch qa.txt"],
  ['bash -lc "ls"', "Ran ls"],
  ["sh -c 'echo it'\\''s'", "Ran echo it's"],
])("a command wrapped in a shell, %s, reads as the command inside", (line, label) => {
  render(<ToolCalls calls={[command("1", line)]} worktree={worktree} live />);

  expect(screen.getByRole("button", { name: label })).toBeTruthy();
});

test("a command's output stays closed when it exits 0 and shows when it exits otherwise", () => {
  render(
    <ToolCalls
      calls={[
        command("1", "/bin/zsh -lc 'touch qa.txt'", { output: "exit_code=0", isError: false }),
        command("2", "/bin/zsh -lc 'cargo test'", { output: "1 failed", isError: true }),
      ]}
      worktree={worktree}
      live={false}
    />,
  );

  expect(screen.queryByText("exit_code=0")).toBeNull();
  expect(screen.getByText("1 failed")).toBeTruthy();
});

const work: ToolCall[] = [
  { id: "1", tool: "Read", input: { file_path: `${worktree}/a.ts` }, result: { output: "a", isError: false } },
  command("2", "/bin/zsh -lc 'npm test'", { output: "3 passed", isError: false }),
  { id: "3", tool: "Edit", input: { file_path: `${worktree}/a.ts` }, result: { output: "", isError: false } },
  { id: "4", tool: "Write", input: { file_path: `${worktree}/b.ts` }, result: { output: "", isError: false } },
  { id: "5", tool: "Edit", input: { file_path: `${worktree}/a.ts` }, result: { output: "", isError: false } },
  command("6", "npm run lint", { output: "", isError: false }),
  command("7", "/bin/zsh -lc 'git status'", { output: "", isError: false }),
  { id: "8", tool: "Grep", input: { pattern: "TODO" }, result: { output: "", isError: false } },
];

test("a finished block of work is one line that counts each kind, and opens to its rows", () => {
  render(<ToolCalls calls={work} worktree={worktree} live={false} />);

  const summary = screen.getByRole("button", { name: "Read 1 file, ran 3 commands, edited 2 files, used 1 other tool" });
  expect(screen.getAllByRole("button")).toEqual([summary]);

  fireEvent.click(summary);

  expect(summary.getAttribute("aria-expanded")).toBe("true");
  const rows = within(screen.getByRole("group", { name: summary.textContent! })).getAllByRole("button");
  expect(rows.map((row) => row.textContent)).toEqual([
    "Read a.ts",
    "Ran npm test",
    "Edited 2 files",
    "Ran 2 commands",
    "Searched for TODO",
  ]);
});

test("a block still working, or with a failed call, keeps its rows open", () => {
  const view = render(<ToolCalls calls={work} worktree={worktree} live />);
  expect(screen.getAllByRole("button")).toHaveLength(5);

  const failed = command("9", "cargo test", { output: "1 failed", isError: true });
  view.rerender(<ToolCalls calls={[...work, failed]} worktree={worktree} live={false} />);
  expect(screen.getAllByRole("button")).toHaveLength(5);
  expect(screen.getByText("1 failed")).toBeTruthy();
});

test("an opened command shows the whole command it ran, with its output", () => {
  const ran = "/bin/zsh -lc 'npm test\n--silent'";
  render(<ToolCalls calls={[command("1", ran, { output: "3 passed", isError: false })]} worktree={worktree} live />);

  fireEvent.click(screen.getByRole("button", { name: "Ran npm test" }));

  expect(document.querySelector(".tool-output")!.textContent).toBe(`${ran}\n3 passed`);
});

test("a failed command shows the whole command it ran over its error", () => {
  const ran = "bash -lc 'cargo test'";
  render(<ToolCalls calls={[command("1", ran, { output: "1 failed", isError: true })]} worktree={worktree} live />);

  expect(document.querySelector(".tool-output.failed")!.textContent).toBe(`${ran}\n1 failed`);
});

test("each command of a folded row reads as the command inside, and keeps the whole command as its title", () => {
  render(
    <ToolCalls
      calls={[command("1", "/bin/zsh -lc 'npm test'"), command("2", "npm run lint")]}
      worktree={worktree}
      live
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Ran 2 commands" }));

  const listed = within(screen.getByRole("list", { name: "Ran 2 commands" })).getAllByRole("listitem");
  expect(listed.map((item) => [item.textContent, item.title])).toEqual([
    ["npm test", "/bin/zsh -lc 'npm test'"],
    ["npm run lint", ""],
  ]);
});
