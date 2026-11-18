import {
  Bot,
  ChevronRight,
  FileText,
  Globe,
  ListTodo,
  type LucideIcon,
  Pencil,
  Search,
  SquareTerminal,
  Wrench,
  X,
} from "lucide-react";
import { useState } from "react";

/** One tool call of the agent, with its result once it has one. */
export interface ToolCall {
  id: string;
  tool: string;
  input: unknown;
  result?: { output: string; isError: boolean };
}

/** A run of tool calls: one quiet row each, which opens to show the call's output. */
export function ToolCalls({ calls, worktree }: { calls: ToolCall[]; worktree: string }) {
  return (
    <div className="tools">
      {calls.map((call) => (
        <ToolRow key={call.id} call={call} worktree={worktree} />
      ))}
    </div>
  );
}

function ToolRow({ call, worktree }: { call: ToolCall; worktree: string }) {
  const [open, setOpen] = useState(false);
  const failed = call.result?.isError ?? false;
  const { label, icon } = describe(call, worktree);
  const Icon = failed ? X : icon;
  return (
    <>
      <button className={failed ? "tool failed" : "tool"} aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon aria-hidden="true" />
        <span className="tool-label">{label}</span>
        <ChevronRight aria-hidden="true" className="chevron" />
      </button>
      {open && <pre className="tool-output">{call.result ? call.result.output : "Running"}</pre>}
    </>
  );
}

/** How each tool's call reads: the input field it acts on, the sentence, and the icon. */
const SENTENCES: Record<string, { field?: string; say(target: string): string; icon: LucideIcon }> = {
  Bash: { field: "command", say: (command) => `Ran ${command.split("\n")[0]}`, icon: SquareTerminal },
  commandExecution: { field: "command", say: (command) => `Ran ${command.split("\n")[0]}`, icon: SquareTerminal },
  Read: { field: "file_path", say: (path) => `Read ${path}`, icon: FileText },
  Write: { field: "file_path", say: (path) => `Wrote ${path}`, icon: Pencil },
  Edit: { field: "file_path", say: (path) => `Edited ${path}`, icon: Pencil },
  MultiEdit: { field: "file_path", say: (path) => `Edited ${path}`, icon: Pencil },
  NotebookEdit: { field: "notebook_path", say: (path) => `Edited ${path}`, icon: Pencil },
  Grep: { field: "pattern", say: (pattern) => `Searched for ${pattern}`, icon: Search },
  Glob: { field: "pattern", say: (pattern) => `Listed ${pattern}`, icon: Search },
  WebFetch: { field: "url", say: (url) => `Fetched ${url}`, icon: Globe },
  WebSearch: { field: "query", say: (query) => `Searched the web for ${query}`, icon: Globe },
  Task: { field: "description", say: (description) => `Ran a subagent: ${description}`, icon: Bot },
  TodoWrite: { say: () => "Updated the to-do list", icon: ListTodo },
};

/**
 * What a call did, as a past-tense sentence with its target, and the icon for its kind. A call
 * whose tool or input the panel does not know says only which tool it used.
 */
function describe({ tool, input }: ToolCall, worktree: string): { label: string; icon: LucideIcon } {
  if (tool === "fileChange") {
    const label = changedFiles(input, worktree);
    if (label) return { label, icon: Pencil };
  }
  const sentence = SENTENCES[tool];
  const target = sentence?.field ? (input as Record<string, unknown> | null)?.[sentence.field] : "";
  if (sentence && typeof target === "string" && (target !== "" || !sentence.field)) {
    return { label: sentence.say(relative(target, worktree)), icon: sentence.icon };
  }
  return { label: `Used ${tool}`, icon: Wrench };
}

// A Codex file change: its first file, and how many more.
function changedFiles(input: unknown, worktree: string): string | null {
  const changes = (input as { changes?: unknown } | null)?.changes;
  if (!Array.isArray(changes)) return null;
  const paths = changes.map((change) => (change as { path?: unknown } | null)?.path);
  if (typeof paths[0] !== "string") return null;
  const more = paths.length > 1 ? ` and ${paths.length - 1} more` : "";
  return `Edited ${relative(paths[0], worktree)}${more}`;
}

/** `path` from the worktree's root when it is inside the worktree, as is otherwise. */
export function relative(path: string, worktree: string): string {
  const prefix = worktree.endsWith("/") ? worktree : `${worktree}/`;
  return path.startsWith(prefix) ? path.slice(prefix.length) : path;
}
