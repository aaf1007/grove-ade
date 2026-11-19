// The Claude sidecar: drives the Agent SDK for one chat-mode session and speaks PROTOCOL.md with
// the core. Everything here is free of Node's own APIs, so tests run it against a stubbed query.
import type {
  AccountInfo,
  CanUseTool,
  EffortLevel,
  ModelInfo,
  Options,
  PermissionResult,
  PermissionUpdate,
  Query,
  SDKMessage,
  SDKUserMessage,
  SessionMessage,
  SlashCommand,
} from "@anthropic-ai/claude-agent-sdk";

/** The flags grove launches the sidecar with. */
export type Launch = {
  settings: string;
  mcpConfig: string;
  sessionId: string;
  claude: string;
  /** Go on with the conversation `sessionId` names, in place of starting it. */
  resume: boolean;
};

export class Usage extends Error {}

const VALUE_FLAGS = {
  "--settings": "settings",
  "--mcp-config": "mcpConfig",
  "--session-id": "sessionId",
  "--claude": "claude",
} as const;

export function parseArgs(args: string[]): Launch {
  const launch: Partial<Launch> = { resume: false };
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (arg === "--resume") {
      launch.resume = true;
    } else if (arg in VALUE_FLAGS && i + 1 < args.length) {
      launch[VALUE_FLAGS[arg as keyof typeof VALUE_FLAGS]] = args[++i];
    } else {
      throw new Usage(`unknown argument ${arg}`);
    }
  }
  const missing = Object.entries(VALUE_FLAGS)
    .filter(([, key]) => launch[key] === undefined)
    .map(([flag]) => flag);
  if (missing.length > 0) throw new Usage(`missing ${missing.join(" ")}`);
  return launch as Launch;
}

/** An async queue: what is pushed comes out of the iterator in order until `close`. */
export class Inbox<T> implements AsyncIterable<T> {
  private items: T[] = [];
  private closed = false;
  private wake: (() => void) | undefined;

  push(item: T) {
    this.items.push(item);
    this.wake?.();
  }

  close() {
    this.closed = true;
    this.wake?.();
  }

  async *[Symbol.asyncIterator]() {
    for (;;) {
      const item = this.items.shift();
      if (item !== undefined) {
        yield item;
      } else if (this.closed) {
        return;
      } else {
        await new Promise<void>((resolve) => (this.wake = resolve));
        this.wake = undefined;
      }
    }
  }
}

export type Deps = {
  query: (params: { prompt: AsyncIterable<SDKUserMessage>; options: Options }) => Query;
  /** The core's lines, from stdin. */
  lines: AsyncIterable<string>;
  /** Writes one protocol message to the core. */
  write: (message: Record<string, unknown>) => void;
  /** The worktree. */
  cwd: string;
  /** Reads a conversation's record: the SDK's `getSessionMessages`, in the worktree's project. */
  sessionMessages: (sessionId: string, dir: string) => Promise<SessionMessage[]>;
};

type Question = {
  question: string;
  header: string;
  multiSelect?: boolean;
  options: { label: string; description: string }[];
};

type Pending = {
  input: Record<string, unknown>;
  suggestions: PermissionUpdate[] | undefined;
  /** Set for a question. */
  questions: Question[] | undefined;
  settle: (result: PermissionResult) => void;
};

type Turn = { interrupted: boolean; error?: { kind: string; message: string } };

type ModelChoice = { model?: string; effort?: EffortLevel };

const DENIED = "The user declined this. Ask them what to do instead.";

/**
 * Runs the conversation until the core closes stdin. Rejects if the SDK ends it first.
 */
export async function run(launch: Launch, deps: Deps): Promise<void> {
  const { write } = deps;
  // readline drops lines that arrive before an iterator exists, and the core sends the session's
  // saved model while the record below is still being read.
  const incoming = deps.lines[Symbol.asyncIterator]();
  // Before anything else, so the conversation so far comes ahead of any new turn.
  let resume = false;
  if (launch.resume) {
    const messages = await deps
      .sessionMessages(launch.sessionId, deps.cwd)
      .then((recorded) => {
        // Claude writes the record only after the first message. With none, there is nothing to
        // resume, and the conversation starts new under the same id. The SDK also gives an empty
        // list for a record it cannot read or parse, which Claude's own resume could not load
        // either, so starting new loses nothing there.
        resume = recorded.length > 0;
        return history(recorded);
      })
      .catch((error: unknown) => {
        // A record that cannot be read or converted gives an empty history, not a failed
        // sidecar, and the resume goes on.
        logError(error);
        resume = true;
        return [];
      });
    write({ type: "history", messages });
  }
  const prompt = new Inbox<SDKUserMessage>();
  const pending = new Map<string, Pending>();
  let turn: Turn | undefined;
  // What set_model chose during the running turn, for the next one.
  let nextModel: ModelChoice | undefined;
  let stdinEnded = false;
  // The uuids of the user's messages that no result has answered yet. A message can reach claude
  // after it counted its queue for a result, so the queue count alone can miss it.
  const unanswered = new Set<string>();

  const startTurn = () => {
    turn = { interrupted: false };
    write({ type: "turn_start" });
  };

  const canUseTool: CanUseTool = (tool, input, { signal, suggestions, toolUseID: id }) =>
    new Promise((resolve) => {
      const settle = (result: PermissionResult) => {
        pending.delete(id);
        signal.removeEventListener("abort", aborted);
        resolve(result);
      };
      const aborted = () => settle({ behavior: "deny", message: DENIED });
      if (signal.aborted) return aborted();
      signal.addEventListener("abort", aborted);
      const questions = tool === "AskUserQuestion" ? (input.questions as Question[]) : undefined;
      pending.set(id, { input, suggestions, questions, settle });
      if (questions) {
        write({
          type: "question_request",
          id,
          questions: questions.map((q) => ({
            question: q.question,
            header: q.header,
            multi_select: q.multiSelect ?? false,
            options: q.options.map(({ label, description }) => ({ label, description })),
          })),
        });
      } else {
        write({ type: "approval_request", id, tool, input });
      }
    });

  const q = deps.query({
    prompt,
    options: {
      cwd: deps.cwd,
      settingSources: ["user", "project", "local"],
      systemPrompt: { type: "preset", preset: "claude_code" },
      pathToClaudeCodeExecutable: launch.claude,
      settings: launch.settings,
      // By path, as terminal mode passes it: the SDK's mcpServers would put the file's
      // GROVE_SESSION_TOKEN on claude's command line.
      extraArgs: { "mcp-config": launch.mcpConfig },
      includePartialMessages: true,
      canUseTool,
      ...(resume ? { resume: launch.sessionId } : { sessionId: launch.sessionId }),
    },
  });

  const sendCommands = (commands: SlashCommand[]) =>
    write({
      type: "commands",
      commands: commands.map((c) => ({ name: c.name, description: c.description, argument_hint: c.argumentHint })),
    });
  void q.accountInfo().then((info) => write({ type: "account", account: account(info) }), logError);
  void q.supportedCommands().then(sendCommands, logError);
  // Sent even when listing fails, so the core never waits for it.
  void q.supportedModels().then(
    (list) => write({ type: "models", models: list.map(model) }),
    (error: unknown) => {
      logError(error);
      write({ type: "models", models: [] });
    },
  );

  const applyModel = ({ model, effort }: ModelChoice) => {
    if (model !== undefined) void q.setModel(model).catch(logError);
    if (effort !== undefined) void q.applyFlagSettings({ effortLevel: effort }).catch(logError);
  };

  const answer = (line: AnswerLine) => {
    const request = pending.get(line.id);
    if (!request) return;
    const { input, suggestions, questions } = request;
    if (line.reply === "deny") {
      request.settle({ behavior: "deny", message: line.message || DENIED });
    } else if (line.reply === "answers") {
      const answers = Object.fromEntries(
        (questions ?? []).map((q, i) => [q.question, (line.answers?.[i] ?? []).join(", ")]),
      );
      request.settle({ behavior: "allow", updatedInput: { ...input, answers } });
    } else if (line.reply === "allow_session" && suggestions?.length) {
      request.settle({ behavior: "allow", updatedInput: input, updatedPermissions: suggestions });
    } else {
      request.settle({ behavior: "allow", updatedInput: input });
    }
  };

  const onLine = (raw: string) => {
    const line = parseLine(raw);
    switch (line?.type) {
      case "user_message": {
        const uuid = crypto.randomUUID();
        unanswered.add(uuid);
        prompt.push({
          type: "user",
          message: { role: "user", content: line.text },
          parent_tool_use_id: null,
          origin: { kind: "human" },
          uuid,
        });
        if (!turn) startTurn();
        return;
      }
      case "answer":
        return answer(line);
      case "interrupt":
        if (!turn) return;
        turn.interrupted = true;
        void q.interrupt().catch(logError);
        return;
      case "set_model": {
        const choice = { model: line.model, effort: line.effort };
        // The running turn keeps its own model. The next one takes this.
        if (turn) nextModel = { model: choice.model ?? nextModel?.model, effort: choice.effort ?? nextModel?.effort };
        else applyModel(choice);
        return;
      }
      default:
        console.error(`skipped a line: ${raw}`);
    }
  };

  /** Ends the turn, and starts the next when `more` messages wait for one. */
  const endTurn = (more: boolean) => {
    if (!turn) return;
    for (const request of pending.values()) request.settle({ behavior: "deny", message: DENIED });
    if (turn.interrupted) write({ type: "turn_end", interrupted: true });
    else if (turn.error) write({ type: "turn_end", error: turn.error });
    else write({ type: "turn_end" });
    turn = undefined;
    if (nextModel) applyModel(nextModel);
    nextModel = undefined;
    if (more) startTurn();
  };

  const onMessage = (message: SDKMessage) => {
    switch (message.type) {
      case "stream_event": {
        const { event } = message;
        // Every API call starts with one. It starts any turn the uuids missed, such as after a
        // result that named none.
        if (message.parent_tool_use_id === null && event.type === "message_start" && !turn) startTurn();
        if (message.parent_tool_use_id === null && event.type === "content_block_delta" && event.delta.type === "text_delta") {
          write({ type: "text_delta", text: event.delta.text });
        }
        return;
      }
      case "assistant": {
        if (message.parent_tool_use_id !== null) return;
        const blocks = message.message.content;
        if (message.error && turn) {
          turn.error = { kind: message.error, message: blocks.flatMap((b) => (b.type === "text" ? [b.text] : [])).join("\n") };
        }
        for (const block of blocks) {
          if (block.type === "tool_use") write({ type: "tool_call", id: block.id, tool: block.name, input: block.input });
        }
        return;
      }
      case "user": {
        const { content } = message.message;
        if (message.parent_tool_use_id !== null || typeof content === "string") return;
        for (const block of content) {
          if (block.type !== "tool_result") continue;
          write({ type: "tool_result", id: block.tool_use_id, output: flatten(block.content), is_error: block.is_error ?? false });
        }
        return;
      }
      case "result": {
        if (turn && !turn.error) {
          if (message.subtype !== "success") {
            turn.error = { kind: message.subtype, message: message.errors.join("\n") || message.subtype };
          } else if (message.is_error) {
            turn.error = { kind: "unknown", message: message.result };
          }
        }
        const answered = message.user_message_uuids ?? (message.user_message_uuid ? [message.user_message_uuid] : undefined);
        if (answered) {
          for (const uuid of answered) unanswered.delete(uuid);
        } else {
          // A result that names none, such as a zeroed one, answers them all.
          unanswered.clear();
        }
        endTurn(unanswered.size > 0 || (!answered && (message.queued_turn_count ?? 0) > 0));
        return;
      }
      case "system":
        if (message.subtype === "commands_changed") sendCommands(message.commands);
        return;
    }
  };

  const reading = (async () => {
    for await (const raw of { [Symbol.asyncIterator]: () => incoming }) onLine(raw);
    stdinEnded = true;
    for (const request of pending.values()) request.settle({ behavior: "deny", message: DENIED });
    prompt.close();
    q.close();
  })();

  for await (const message of q) onMessage(message);
  if (!stdinEnded) throw new Error("the SDK ended the conversation before the core did");
  await reading;
}

type AnswerLine = {
  type: "answer";
  id: string;
  reply: "allow_once" | "allow_session" | "deny" | "answers";
  message?: string;
  answers?: string[][];
};

type Line =
  | { type: "user_message"; text: string }
  | AnswerLine
  | { type: "interrupt" }
  | ({ type: "set_model" } & ModelChoice);

/** The core's line, or undefined when it is not one of PROTOCOL.md's. */
function parseLine(raw: string): Line | undefined {
  let line: unknown;
  try {
    line = JSON.parse(raw);
  } catch {
    return undefined;
  }
  if (typeof line !== "object" || line === null) return undefined;
  const l = line as Record<string, unknown>;
  switch (l.type) {
    case "user_message":
      return typeof l.text === "string" ? (l as Line) : undefined;
    case "answer":
      return typeof l.id === "string" ? (l as Line) : undefined;
    case "interrupt":
    case "set_model":
      return l as Line;
  }
  return undefined;
}

/**
 * PROTOCOL.md's `history` messages for a conversation's record: each message the user typed
 * starts a turn, which ends before the next one. Subagents' messages are left out, as live. A turn
 * starts at its user message's time, a tool call has its message's time, and a turn ends at the
 * time of its last message. A message with no time in the record gives none.
 */
function history(recorded: SessionMessage[]): Record<string, unknown>[] {
  const messages: Record<string, unknown>[] = [];
  let open = false;
  // The time of the open turn's latest message.
  let last: number | undefined;
  const ended = () => ({ type: "turn_end", ...timed(last) });
  for (const entry of recorded) {
    const { type, message, parent_tool_use_id } = entry;
    if (parent_tool_use_id !== null) continue;
    const time = timeOf(entry);
    // The SDK types the body as unknown, and a record can hold one with none.
    const content = (message as { content?: unknown } | null)?.content;
    const blocks: Block[] = typeof content === "string" ? [{ type: "text", text: content }] : Array.isArray(content) ? content : [];
    if (type === "user") {
      for (const block of blocks) {
        if (block.type === "tool_result") {
          messages.push({ type: "tool_result", id: block.tool_use_id, output: flatten(block.content), is_error: block.is_error ?? false });
        }
      }
      const text = blocks.flatMap((b) => (b.type === "text" && b.text !== undefined ? [b.text] : [])).join("\n");
      if (!text) {
        last = time ?? last;
        continue;
      }
      if (open) messages.push(ended());
      messages.push({ type: "user", text }, { type: "turn_start", ...timed(time) });
      open = true;
      last = time;
    } else if (type === "assistant") {
      for (const block of blocks) {
        if (block.type === "text") messages.push({ type: "text_delta", text: block.text });
        if (block.type === "tool_use") {
          messages.push({ type: "tool_call", id: block.id, tool: block.name, input: block.input, ...timed(time) });
        }
      }
      last = time ?? last;
    }
  }
  if (open) messages.push(ended());
  return messages;
}

/**
 * When Claude wrote a recorded message, in milliseconds since the Unix epoch. The SDK gives each
 * one the record's `timestamp`, which its type does not list. undefined when it has none.
 */
function timeOf(entry: SessionMessage): number | undefined {
  const { timestamp } = entry as { timestamp?: unknown };
  const time = typeof timestamp === "string" ? Date.parse(timestamp) : NaN;
  return Number.isNaN(time) ? undefined : time;
}

/** A history message's `time_unix_ms` field, left out when there is no time. */
function timed(time: number | undefined): { time_unix_ms?: number } {
  return time === undefined ? {} : { time_unix_ms: time };
}

/** A content block of a recorded message, with the fields `history` reads. */
type Block = {
  type: string;
  text?: string;
  id?: string;
  name?: string;
  input?: unknown;
  tool_use_id?: string;
  content?: string | { type: string; text?: string }[];
  is_error?: boolean;
};

/** PROTOCOL.md's model. The SDK names no default effort, and its default model is "default". */
function model(info: ModelInfo) {
  return {
    id: info.value,
    name: info.displayName,
    description: info.description,
    efforts: info.supportedEffortLevels ?? [],
    default_effort: null,
    is_default: info.value === "default",
  };
}

/** PROTOCOL.md's account, or null when the SDK reports nobody logged in. */
function account(info: AccountInfo) {
  if (!info.email && !info.apiKeySource && !info.tokenSource) return null;
  return {
    email: info.email ?? null,
    organization: info.organization ?? null,
    plan: info.subscriptionType ?? null,
    api_key_source: info.apiKeySource ?? null,
  };
}

/** A tool result's text blocks, as one string. */
function flatten(content: string | readonly { type: string; text?: string }[] | undefined): string {
  if (content === undefined) return "";
  if (typeof content === "string") return content;
  return content.flatMap((b) => (b.type === "text" && b.text !== undefined ? [b.text] : [])).join("\n");
}

function logError(error: unknown) {
  console.error(error);
}
