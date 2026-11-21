import {
  Ban,
  Bot,
  ChevronRight,
  FileText,
  Globe,
  ListChecks,
  ListTodo,
  type LucideIcon,
  Pencil,
  Search,
  SquareTerminal,
  Wrench,
  X,
} from "lucide-react";
import { type ReactNode, useId, useState } from "react";

/** One tool call of the agent, with its result once it has one. */
export interface ToolCall {
  id: string;
  tool: string;
  input: unknown;
  result?: { output: string; isError: boolean };
  /** When the agent made it, in milliseconds since the Unix epoch. null or undefined when unknown. */
  time?: number | null;
}

/**
 * A run of tool calls as quiet rows. Calls of the same kind in a row fold into one row with a
 * count, which opens to list them. A failed call has a row of its own, with its error under it.
 * While `live`, a call with no result yet shimmers. Once the run is done, its rows fold into one
 * line that counts them by kind, unless a call failed.
 */
export function ToolCalls({ calls, worktree, live }: { calls: ToolCall[]; worktree: string; live: boolean }) {
  const runs = fold(calls);
  const rows = runs.map((run) =>
    run.length === 1 ? (
      <ToolRow key={run[0].id} call={run[0]} worktree={worktree} live={live} />
    ) : (
      <FoldedRow key={run[0].id} calls={run} worktree={worktree} live={live} />
    ),
  );
  const done = !live && runs.length > 1 && !calls.some((call) => call.result?.isError);
  return <div className="tools">{done ? <Summary label={summarize(calls, worktree)}>{rows}</Summary> : rows}</div>;
}

/** Finished work as one line, "Ran 3 commands, edited 2 files", which opens to its rows. */
function Summary({ label, children }: { label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button className="tool" aria-expanded={open} onClick={() => setOpen(!open)}>
        <ListChecks aria-hidden="true" />
        <span className="tool-label">{label}</span>
        <ChevronRight aria-hidden="true" className="chevron" />
      </button>
      {open && (
        <div className="tool-rows" role="group" aria-label={label}>
          {children}
        </div>
      )}
    </>
  );
}

/** How many calls of each kind `calls` made, in the order the kinds came: "Read 2 files, ran 1 command". */
function summarize(calls: ToolCall[], worktree: string): string {
  const byKind = new Map<Kind | null, string[]>();
  for (const call of calls) {
    const found = folds(call) ? targets(call)!.map((target) => relative(target, worktree)) : null;
    const kind = found ? KINDS[call.tool] : null;
    if (!byKind.has(kind)) byKind.set(kind, []);
    // A call of another kind counts once.
    byKind.get(kind)!.push(...(found ?? [call.id]));
  }
  const parts = [...byKind].map(([kind, all], i) => {
    const count = kind?.unique ? new Set(all).size : all.length;
    const { verb, noun, nouns } = kind ?? OTHER;
    return `${i === 0 ? verb : verb.toLowerCase()} ${count} ${count === 1 ? noun : nouns}`;
  });
  return parts.join(", ");
}

/**
 * A call the user declined. It never ran, so its row shows what it would have acted on, with no
 * output: the agent's result for it only repeats why it was declined.
 */
export function DeclinedCall({ call, worktree }: { call: ToolCall; worktree: string }) {
  const what = targets(call)?.map((target) => relative(target, worktree)).join(", ") ?? call.tool;
  return (
    <div className="tool declined">
      <Ban aria-hidden="true" />
      <span className="tool-label">{what}</span>
      <Clock time={call.time} />
    </div>
  );
}

function ToolRow({ call, worktree, live }: { call: ToolCall; worktree: string; live: boolean }) {
  const [open, setOpen] = useState(false);
  const timeId = useId();
  const { label, icon: Icon } = describe(call, worktree);
  const command = wholeCommand(call);
  const ran = command && <span className="tool-command">{`${command}\n`}</span>;
  if (call.result?.isError) {
    return (
      <>
        <div className="tool failed">
          <X aria-hidden="true" />
          <span className="tool-label">{label}</span>
          <Clock time={call.time} />
        </div>
        <pre className="tool-output failed">
          {ran}
          {call.result.output}
        </pre>
      </>
    );
  }
  return (
    <>
      <button
        className="tool"
        aria-expanded={open}
        aria-describedby={known(call.time) ? timeId : undefined}
        onClick={() => setOpen(!open)}
      >
        <Icon aria-hidden="true" />
        <Label text={label} running={live && !call.result} />
        <Clock time={call.time} id={timeId} />
        <ChevronRight aria-hidden="true" className="chevron" />
      </button>
      {open && (
        <pre className="tool-output">
          {ran}
          {call.result ? call.result.output : "Running"}
        </pre>
      )}
    </>
  );
}

/**
 * Calls of one kind in a row: "Read 4 files", which opens to list each, one line per target. Each
 * line opens to the output of its calls.
 */
function FoldedRow({ calls, worktree, live }: { calls: ToolCall[]; worktree: string; live: boolean }) {
  const [open, setOpen] = useState(false);
  const timeId = useId();
  const kind = KINDS[calls[0].tool];
  const time = calls.find((call) => known(call.time))?.time;
  // A file read or edited twice is one file. A command run twice ran twice.
  const listed = new Map<string, ToolCall[]>();
  calls.forEach((call, i) => {
    for (const target of targets(call)!.map((target) => relative(target, worktree))) {
      const key = kind.unique ? target : `${i} ${target}`;
      listed.set(key, [...(listed.get(key) ?? []), call]);
    }
  });
  const label =
    listed.size === 1 ? describe(calls[0], worktree).label : `${kind.verb} ${listed.size} ${kind.nouns}`;
  return (
    <>
      <button
        className="tool"
        aria-expanded={open}
        aria-describedby={known(time) ? timeId : undefined}
        onClick={() => setOpen(!open)}
      >
        <kind.icon aria-hidden="true" />
        <Label text={label} running={live && calls.some((call) => !call.result)} />
        <Clock time={time} id={timeId} />
        <ChevronRight aria-hidden="true" className="chevron" />
      </button>
      {open && (
        <ul className="tool-calls" aria-label={label}>
          {[...listed].map(([key, of]) => (
            <FoldedCall key={key} target={kind.unique ? key : targets(of[0])![0]} calls={of} />
          ))}
        </ul>
      )}
    </>
  );
}

/** One line of an opened folded row: a file or a command, which opens to the output of its calls. */
function FoldedCall({ target, calls }: { target: string; calls: ToolCall[] }) {
  const [open, setOpen] = useState(false);
  // A command has one call, and keeps its whole command as its title.
  const command = wholeCommand(calls[0]);
  return (
    <li title={command ?? undefined}>
      <button className="tool-call" aria-expanded={open} onClick={() => setOpen(!open)}>
        {target}
      </button>
      {open &&
        calls.map((call) => (
          <pre key={call.id} className="tool-output">
            {command && <span className="tool-command">{`${command}\n`}</span>}
            {call.result ? call.result.output : "Running"}
          </pre>
        ))}
    </li>
  );
}

/**
 * When a call happened, as a short clock time such as 14:02. Nothing when the time is unknown.
 * Given an `id`, it sits in a button that names it as its description, so it stays out of the
 * button's name, which says what the call did.
 */
function Clock({ time, id }: { time: number | null | undefined; id?: string }) {
  if (!known(time)) return null;
  const at = new Date(time);
  return (
    <time
      className="tool-time"
      id={id}
      dateTime={at.toISOString()}
      title={at.toLocaleString()}
      aria-hidden={id ? "true" : undefined}
    >
      {at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hourCycle: "h23" })}
    </time>
  );
}

function known(time: number | null | undefined): time is number {
  return time !== null && time !== undefined;
}

function Label({ text, running }: { text: string; running: boolean }) {
  return <span className={running ? "tool-label shimmer" : "tool-label"}>{text}</span>;
}

/** The kinds of call that fold, by tool. */
interface Kind {
  verb: string;
  noun: string;
  nouns: string;
  /** Count each target once. */
  unique: boolean;
  icon: LucideIcon;
}
const READ: Kind = { verb: "Read", noun: "file", nouns: "files", unique: true, icon: FileText };
const EDIT: Kind = { verb: "Edited", noun: "file", nouns: "files", unique: true, icon: Pencil };
const COMMAND: Kind = { verb: "Ran", noun: "command", nouns: "commands", unique: false, icon: SquareTerminal };
/** In a summary, the calls of every kind that does not fold. */
const OTHER = { verb: "Used", noun: "other tool", nouns: "other tools" };
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
  return [KINDS[tool] === COMMAND ? shortCommand(target) : target];
}

/** A command call's whole command, when its label shows less of it. null otherwise. */
function wholeCommand({ tool, input }: ToolCall): string | null {
  const command = (input as { command?: unknown } | null)?.command;
  if (KINDS[tool] !== COMMAND || typeof command !== "string") return null;
  return shortCommand(command) === command ? null : command;
}

/** What a command reads as: its first line, from inside the shell that wraps it, if one does. */
function shortCommand(command: string): string {
  return unwrapped(command).split("\n")[0];
}

/**
 * `command` without a leading shell wrapper such as `/bin/zsh -lc '...'` or `bash -lc "..."`, as
 * Codex sends its commands. As is when no shell wraps it.
 */
function unwrapped(command: string): string {
  const inner = /^(?:\/(?:usr\/)?bin\/)?(?:zsh|bash|sh) -l?c (.+)$/s.exec(command)?.[1];
  if (inner === undefined) return command;
  const quoted = /^'(.*)'$/s.exec(inner)?.[1];
  if (quoted !== undefined) return quoted.replaceAll("'\\''", "'");
  const doubleQuoted = /^"(.*)"$/s.exec(inner)?.[1];
  if (doubleQuoted !== undefined) return doubleQuoted.replace(/\\(["\\$`])/g, "$1");
  return inner;
}

/** How each tool's call reads: the input field it acts on, the sentence, and the icon. */
const SENTENCES: Record<string, { field?: string; say(target: string): string; icon: LucideIcon }> = {
  Bash: { field: "command", say: (command) => `Ran ${shortCommand(command)}`, icon: SquareTerminal },
  commandExecution: { field: "command", say: (command) => `Ran ${shortCommand(command)}`, icon: SquareTerminal },
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
