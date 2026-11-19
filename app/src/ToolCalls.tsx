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

/**
 * A run of tool calls as quiet rows. Calls of the same kind in a row fold into one row with a
 * count, which opens to list them. A failed call has a row of its own, with its error under it.
 * While `live`, a call with no result yet shimmers.
 */
export function ToolCalls({ calls, worktree, live }: { calls: ToolCall[]; worktree: string; live: boolean }) {
  return (
    <div className="tools">
      {fold(calls).map((run) =>
        run.length === 1 ? (
          <ToolRow key={run[0].id} call={run[0]} worktree={worktree} live={live} />
        ) : (
          <FoldedRow key={run[0].id} calls={run} worktree={worktree} live={live} />
        ),
      )}
    </div>
  );
}

function ToolRow({ call, worktree, live }: { call: ToolCall; worktree: string; live: boolean }) {
  const [open, setOpen] = useState(false);
  const { label, icon: Icon } = describe(call, worktree);
  if (call.result?.isError) {
    return (
      <>
        <div className="tool failed">
          <X aria-hidden="true" />
          <span className="tool-label">{label}</span>
        </div>
        <pre className="tool-output failed">{call.result.output}</pre>
      </>
    );
  }
  return (
    <>
      <button className="tool" aria-expanded={open} onClick={() => setOpen(!open)}>
        <Icon aria-hidden="true" />
        <Label text={label} running={live && !call.result} />
        <ChevronRight aria-hidden="true" className="chevron" />
      </button>
      {open && <pre className="tool-output">{call.result ? call.result.output : "Running"}</pre>}
    </>
  );
}

/** Calls of one kind in a row: "Read 4 files", which opens to list each, one line per target. */
function FoldedRow({ calls, worktree, live }: { calls: ToolCall[]; worktree: string; live: boolean }) {
  const [open, setOpen] = useState(false);
  const kind = KINDS[calls[0].tool];
  const all = calls.flatMap((call) => targets(call)!.map((target) => relative(target, worktree)));
  // A file read or edited twice is one file. A command run twice ran twice.
  const listed = kind.unique ? [...new Set(all)] : all;
  const label =
    listed.length === 1 ? describe(calls[0], worktree).label : `${kind.verb} ${listed.length} ${kind.noun}`;
  return (
    <>
      <button className="tool" aria-expanded={open} onClick={() => setOpen(!open)}>
        <kind.icon aria-hidden="true" />
        <Label text={label} running={live && calls.some((call) => !call.result)} />
        <ChevronRight aria-hidden="true" className="chevron" />
      </button>
      {open && (
        <ul className="tool-calls" aria-label={label}>
          {listed.map((target, i) => (
            <li key={i}>{target}</li>
          ))}
        </ul>
      )}
    </>
  );
}

function Label({ text, running }: { text: string; running: boolean }) {
  return <span className={running ? "tool-label shimmer" : "tool-label"}>{text}</span>;
}

/** The kinds of call that fold, by tool. */
interface Kind {
  verb: string;
  noun: string;
  /** Count each target once. */
  unique: boolean;
  icon: LucideIcon;
}
const READ: Kind = { verb: "Read", noun: "files", unique: true, icon: FileText };
const EDIT: Kind = { verb: "Edited", noun: "files", unique: true, icon: Pencil };
const COMMAND: Kind = { verb: "Ran", noun: "commands", unique: false, icon: SquareTerminal };
const KINDS: Record<string, Kind> = {
  Read: READ,
  Edit: EDIT,
  Write: EDIT,
  MultiEdit: EDIT,
  NotebookEdit: EDIT,
  fileChange: EDIT,
  Bash: COMMAND,
  commandExecution: COMMAND,
};

/** `calls` split into runs: calls of one kind in a row share a run, and every other call has its own. */
function fold(calls: ToolCall[]): ToolCall[][] {
  const runs: ToolCall[][] = [];
  let kind: Kind | null = null;
  for (const call of calls) {
    const own = folds(call) ? KINDS[call.tool] : null;
    if (own && own === kind) runs[runs.length - 1].push(call);
    else runs.push([call]);
    kind = own;
  }
  return runs;
}

function folds(call: ToolCall): boolean {
  return KINDS[call.tool] !== undefined && !call.result?.isError && targets(call) !== null;
}

/** The files or commands a call acts on, as the agent named them. null when its input has none. */
function targets({ tool, input }: ToolCall): string[] | null {
  if (tool === "fileChange") {
    const changes = (input as { changes?: unknown } | null)?.changes;
    if (!Array.isArray(changes)) return null;
    const paths = changes.map((change) => (change as { path?: unknown } | null)?.path);
    return paths.length > 0 && paths.every((path) => typeof path === "string") ? (paths as string[]) : null;
  }
  const field = SENTENCES[tool]?.field;
  const target = field ? (input as Record<string, unknown> | null)?.[field] : undefined;
  if (typeof target !== "string" || target === "") return null;
  return [KINDS[tool] === COMMAND ? target.split("\n")[0] : target];
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
