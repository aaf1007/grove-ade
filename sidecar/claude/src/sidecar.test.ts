import type {
  AccountInfo,
  CanUseTool,
  ModelInfo,
  Options,
  PermissionResult,
  PermissionUpdate,
  Query,
  SDKMessage,
  SDKUserMessage,
  SessionMessage,
} from "@anthropic-ai/claude-agent-sdk";
import { createInterface } from "node:readline";
import { PassThrough } from "node:stream";
import { describe, expect, it, vi } from "vitest";

import { Inbox, type Launch, parseArgs, run } from "./sidecar.ts";

const launch: Launch = {
  settings: "/grove/sessions/s1/claude-settings.json",
  mcpConfig: "/grove/sessions/s1/mcp.json",
  sessionId: "0b0f6a52-3f5e-4c2a-9d38-6a8c1f2e7d10",
  claude: "/Users/ann/.local/bin/claude",
  resume: false,
};
const ann: AccountInfo = { email: "ann@example.com", organization: "Ann's org", subscriptionType: "max" };
const models: ModelInfo[] = [
  {
    value: "default",
    displayName: "Default (recommended)",
    description: "Opus 5.5",
    supportsEffort: true,
    supportedEffortLevels: ["low", "high", "max"],
  },
  { value: "haiku", displayName: "Haiku 5.5", description: "Fastest" },
];

/** Runs the sidecar against a stubbed `query`. The test plays the SDK and the core. */
function start(
  overrides: Partial<Launch> = {},
  account: AccountInfo = ann,
  supportedModels: () => Promise<ModelInfo[]> = async () => models,
  sessionMessages: (sessionId: string, dir: string) => Promise<SessionMessage[]> = async () => [],
  /** stdin's lines, as main.ts reads them. The test's own `send` writes to the default. */
  lines?: AsyncIterable<string>,
) {
  const sdk = new Inbox<SDKMessage>();
  const core = new Inbox<string>();
  const out: Record<string, unknown>[] = [];
  let params: { prompt: AsyncIterable<SDKUserMessage>; options: Options } | undefined;
  const q = {
    [Symbol.asyncIterator]: () => sdk[Symbol.asyncIterator](),
    interrupt: vi.fn(async () => undefined),
    setModel: vi.fn(async () => undefined),
    applyFlagSettings: vi.fn(async () => undefined),
    supportedCommands: vi.fn(async () => [
      { name: "review", description: "Review a pull request", argumentHint: "<pr>" },
    ]),
    accountInfo: vi.fn(async () => account),
    supportedModels: vi.fn(supportedModels),
    close: vi.fn(() => sdk.close()),
  };
  const done = run({ ...launch, ...overrides }, {
    query: (p) => {
      params = p;
      return q as unknown as Query;
    },
    lines: lines ?? core,
    write: (message) => out.push(message),
    cwd: "/repo/worktrees/fix-parser",
    sessionMessages,
  });
  const options = () => params!.options;
  return {
    q,
    out,
    done,
    options,
    /** The core writes a line to the sidecar's stdin. */
    send: (line: object) => core.push(JSON.stringify(line)),
    sendRaw: (line: string) => core.push(line),
    endStdin: () => core.close(),
    /** The SDK yields a message. */
    yields: (message: object) => sdk.push(message as SDKMessage),
    /** The SDK asks `canUseTool`, as it would mid-turn. */
    ask: (tool: string, input: Record<string, unknown>, id: string, suggestions?: PermissionUpdate[]) => {
      const abort = new AbortController();
      const canUseTool = options().canUseTool as CanUseTool;
      const result = canUseTool(tool, input, {
        signal: abort.signal,
        suggestions,
        toolUseID: id,
        requestId: `req-${id}`,
      }) as Promise<PermissionResult>;
      return { result, abort };
    },
    /** The messages the SDK has been given so far. */
    prompts: async (n: number) => {
      const got: SDKUserMessage[] = [];
      for await (const message of params!.prompt) {
        got.push(message);
        if (got.length === n) break;
      }
      return got;
    },
  };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

const assistant = (content: object[], extra: object = {}) => ({
  type: "assistant",
  message: { role: "assistant", content },
  parent_tool_use_id: null,
  ...extra,
});
const user = (content: object[], parent: string | null = null) => ({
  type: "user",
  message: { role: "user", content },
  parent_tool_use_id: parent,
});
const result = (extra: object = {}) => ({ type: "result", subtype: "success", is_error: false, result: "", ...extra });

const recorded = (type: "user" | "assistant", content: string | object[], parent: string | null = null) =>
  ({ type, uuid: "u", session_id: launch.sessionId, message: { role: type, content }, parent_tool_use_id: parent, parent_agent_id: null }) as SessionMessage;

describe("query options", () => {
  it("load the user's own settings, the claude_code preset, the worktree, their claude and the session files", async () => {
    const s = start();
    await settle();
    expect(s.options()).toMatchObject({
      settingSources: ["user", "project", "local"],
      systemPrompt: { type: "preset", preset: "claude_code" },
      cwd: "/repo/worktrees/fix-parser",
      pathToClaudeCodeExecutable: "/Users/ann/.local/bin/claude",
      settings: launch.settings,
      extraArgs: { "mcp-config": launch.mcpConfig },
      sessionId: launch.sessionId,
      includePartialMessages: true,
    });
    expect(s.options().resume).toBeUndefined();
  });

  it("set no env, so Claude Code gets the sidecar's own, with GROVE_SESSION_ID and GROVE_SESSION_TOKEN", async () => {
    // A set env replaces the whole environment in the SDK, which would drop both.
    const s = start();
    await settle();
    expect(s.options()).not.toHaveProperty("env");
  });

  it("resume the conversation with --resume", async () => {
    const s = start({ resume: true }, ann, async () => models, async () => [recorded("user", "fix the parser")]);
    await settle();
    expect(s.options().resume).toBe(launch.sessionId);
    expect(s.options().sessionId).toBeUndefined();
  });

  it("start a new conversation under the same id when --resume finds no record, as for a session that never got a message", async () => {
    const s = start({ resume: true }, ann, async () => models, async () => []);
    await settle();
    expect(s.out[0]).toEqual({ type: "history", messages: [] });
    expect(s.options().sessionId).toBe(launch.sessionId);
    expect(s.options().resume).toBeUndefined();
  });

  // The core sends the session's saved model first, while the sidecar still reads the record.
  it("keeps the lines stdin brings while --resume reads the record", async () => {
    const stdin = new PassThrough();
    let read: (recorded: SessionMessage[]) => void = () => {};
    const recordRead = () => new Promise<SessionMessage[]>((resolve) => (read = resolve));
    const lines = createInterface({ input: stdin, crlfDelay: Infinity });
    const s = start({ resume: true }, ann, async () => models, recordRead, lines);

    stdin.write(`${JSON.stringify({ type: "set_model", model: "haiku", effort: "low" })}\n`);
    await settle();
    read([]);

    await vi.waitFor(() => expect(s.q.setModel).toHaveBeenCalledWith("haiku"));
    expect(s.q.applyFlagSettings).toHaveBeenCalledWith({ effortLevel: "low" });
    stdin.end();
  });

  it("still resume when the record cannot be read", async () => {
    const s = start({ resume: true }, ann, async () => models, async () => {
      throw new Error("permission denied");
    });
    await settle();
    expect(s.options().resume).toBe(launch.sessionId);
    expect(s.options().sessionId).toBeUndefined();
  });
});

describe("history", () => {

  it("comes first on --resume: the record's turns from the worktree's project, as chat mode shows them", async () => {
    const read = vi.fn(async () => [
      recorded("user", "fix the parser"),
      recorded("assistant", [{ type: "text", text: "Looking." }, { type: "tool_use", id: "toolu_1", name: "Bash", input: { command: "ls" } }]),
      recorded("user", [{ type: "tool_result", tool_use_id: "toolu_1", content: [{ type: "text", text: "src" }] }]),
      recorded("assistant", [{ type: "tool_use", id: "toolu_2", name: "Task", input: {} }], "toolu_9"),
      recorded("assistant", [{ type: "text", text: "Done." }]),
      recorded("user", [{ type: "text", text: "thanks" }]),
    ]);
    const s = start({ resume: true }, ann, async () => models, read);
    await settle();

    expect(read).toHaveBeenCalledWith(launch.sessionId, "/repo/worktrees/fix-parser");
    expect(s.out[0]).toEqual({
      type: "history",
      messages: [
        { type: "user", text: "fix the parser" },
        { type: "turn_start" },
        { type: "text_delta", text: "Looking." },
        { type: "tool_call", id: "toolu_1", tool: "Bash", input: { command: "ls" } },
        { type: "tool_result", id: "toolu_1", output: "src", is_error: false },
        { type: "text_delta", text: "Done." },
        { type: "turn_end" },
        { type: "user", text: "thanks" },
        { type: "turn_start" },
        { type: "turn_end" },
      ],
    });
  });

  it("gives each turn start, tool call and turn end the time its record has, and leaves it out when it has none", async () => {
    const at = (message: SessionMessage, timestamp: string) => ({ ...message, timestamp }) as SessionMessage;
    const read = async () => [
      at(recorded("user", "fix the parser"), "2026-11-19T14:02:00.000Z"),
      at(recorded("assistant", [{ type: "tool_use", id: "toolu_1", name: "Bash", input: { command: "ls" } }]), "2026-11-19T14:02:01.500Z"),
      at(recorded("assistant", [{ type: "text", text: "Done." }]), "2026-11-19T14:03:03.000Z"),
      recorded("user", "thanks"),
    ];
    const s = start({ resume: true }, ann, async () => models, read);
    await settle();

    expect(s.out[0]).toEqual({
      type: "history",
      messages: [
        { type: "user", text: "fix the parser" },
        { type: "turn_start", time_unix_ms: Date.UTC(2026, 10, 19, 14, 2, 0) },
        { type: "tool_call", id: "toolu_1", tool: "Bash", input: { command: "ls" }, time_unix_ms: Date.UTC(2026, 10, 19, 14, 2, 1, 500) },
        { type: "text_delta", text: "Done." },
        { type: "turn_end", time_unix_ms: Date.UTC(2026, 10, 19, 14, 3, 3) },
        { type: "user", text: "thanks" },
        { type: "turn_start" },
        { type: "turn_end" },
      ],
    });
  });

  it("skips a recorded message with no body", async () => {
    const read = async () => [{ ...recorded("assistant", []), message: null }, recorded("user", "hi")];
    const s = start({ resume: true }, ann, async () => models, read);
    await settle();

    expect(s.out[0]).toEqual({
      type: "history",
      messages: [{ type: "user", text: "hi" }, { type: "turn_start" }, { type: "turn_end" }],
    });
  });

  it("is empty when the record cannot be read, and a new conversation sends none", async () => {
    const failing = start({ resume: true }, ann, async () => models, async () => {
      throw new Error("no such session");
    });
    const fresh = start();
    await settle();

    expect(failing.out[0]).toEqual({ type: "history", messages: [] });
    expect(fresh.out.some((m) => m.type === "history")).toBe(false);
  });
});

describe("launch", () => {
  it("takes the flags grove passes", () => {
    const args = ["--settings", "a.json", "--mcp-config", "b.json", "--session-id", "id", "--claude", "claude"];
    expect(parseArgs(args)).toEqual({ settings: "a.json", mcpConfig: "b.json", sessionId: "id", claude: "claude", resume: false });
    expect(parseArgs([...args, "--resume"]).resume).toBe(true);
  });

  it("fails on a missing or unknown flag", () => {
    expect(() => parseArgs(["--settings", "a.json"])).toThrow(/missing --mcp-config/);
    expect(() => parseArgs(["--nope"])).toThrow(/unknown argument --nope/);
  });
});

describe("the account, the commands and the models", () => {
  it("are sent once the SDK knows them", async () => {
    const s = start();
    await vi.waitFor(() => expect(s.out).toHaveLength(3));
    expect(s.out).toContainEqual({
      type: "account",
      account: { email: "ann@example.com", organization: "Ann's org", plan: "max", api_key_source: null },
    });
    expect(s.out).toContainEqual({
      type: "commands",
      commands: [{ name: "review", description: "Review a pull request", argument_hint: "<pr>" }],
    });
    expect(s.out).toContainEqual({
      type: "models",
      models: [
        {
          id: "default",
          name: "Default (recommended)",
          description: "Opus 5.5",
          efforts: ["low", "high", "max"],
          default_effort: null,
          is_default: true,
        },
        { id: "haiku", name: "Haiku 5.5", description: "Fastest", efforts: [], default_effort: null, is_default: false },
      ],
    });
  });

  it("send no models when the SDK cannot list them, so the core stops waiting", async () => {
    const s = start({}, ann, async () => {
      throw new Error("not initialized");
    });
    await vi.waitFor(() => expect(s.out).toContainEqual({ type: "models", models: [] }));
  });

  it("send no account when nobody is logged in", async () => {
    const s = start({}, {});
    await vi.waitFor(() => expect(s.out).toContainEqual({ type: "account", account: null }));
  });

  it("send the commands again when they change", async () => {
    const s = start();
    await vi.waitFor(() => expect(s.out).toHaveLength(3));
    s.yields({
      type: "system",
      subtype: "commands_changed",
      commands: [{ name: "fix", description: "Fix it", argumentHint: "" }],
    });
    await vi.waitFor(() =>
      expect(s.out.at(-1)).toEqual({ type: "commands", commands: [{ name: "fix", description: "Fix it", argument_hint: "" }] }),
    );
  });
});

/** Starts the sidecar and a turn, and clears what it wrote before the turn. */
async function inTurn() {
  const s = start();
  await vi.waitFor(() => expect(s.out).toHaveLength(3));
  s.send({ type: "user_message", text: "add a test" });
  await vi.waitFor(() => expect(s.out.at(-1)).toEqual({ type: "turn_start" }));
  s.out.length = 0;
  return s;
}

describe("SDK messages", () => {
  it("a user message reaches the SDK from the human and starts a turn", async () => {
    const s = start();
    s.send({ type: "user_message", text: "add a test" });
    const [message] = await s.prompts(1);
    expect(message).toMatchObject({
      type: "user",
      message: { role: "user", content: "add a test" },
      parent_tool_use_id: null,
      origin: { kind: "human" },
    });
    expect(s.out).toContainEqual({ type: "turn_start" });
  });

  it("a second message mid-turn starts no second turn", async () => {
    const s = await inTurn();
    s.send({ type: "user_message", text: "and a second" });
    await s.prompts(2);
    expect(s.out).toEqual([]);
  });

  it("text deltas stream as text_delta, and a subagent's are dropped", async () => {
    const s = await inTurn();
    const delta = (text: string, parent: string | null = null) => ({
      type: "stream_event",
      event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } },
      parent_tool_use_id: parent,
    });
    s.yields(delta("I'll add"));
    s.yields(delta("hidden", "toolu_09"));
    s.yields({ type: "stream_event", event: { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: "hm" } }, parent_tool_use_id: null });
    s.yields(delta(" the test"));
    await vi.waitFor(() => expect(s.out).toHaveLength(2));
    expect(s.out).toEqual([
      { type: "text_delta", text: "I'll add" },
      { type: "text_delta", text: " the test" },
    ]);
  });

  it("tool_use blocks become tool_call, and the streamed text is not sent twice", async () => {
    const s = await inTurn();
    s.yields(
      assistant([
        { type: "text", text: "Running it" },
        { type: "tool_use", id: "toolu_03", name: "Bash", input: { command: "npm test" } },
      ]),
    );
    s.yields(assistant([{ type: "tool_use", id: "toolu_10", name: "Read", input: {} }], { parent_tool_use_id: "toolu_09" }));
    s.yields(assistant([{ type: "tool_use", id: "toolu_04", name: "Read", input: { file_path: "a.ts" } }]));
    await vi.waitFor(() => expect(s.out).toHaveLength(2));
    expect(s.out).toEqual([
      { type: "tool_call", id: "toolu_03", tool: "Bash", input: { command: "npm test" } },
      { type: "tool_call", id: "toolu_04", tool: "Read", input: { file_path: "a.ts" } },
    ]);
  });

  it("tool_result blocks become tool_result with their text in one string", async () => {
    const s = await inTurn();
    s.yields(
      user([
        { type: "tool_result", tool_use_id: "toolu_03", content: [{ type: "text", text: "1 passed" }, { type: "image", source: {} }, { type: "text", text: "done" }] },
        { type: "tool_result", tool_use_id: "toolu_04", content: "no such file", is_error: true },
        { type: "tool_result", tool_use_id: "toolu_05" },
      ]),
    );
    s.yields(user([{ type: "tool_result", tool_use_id: "toolu_10", content: "inside" }], "toolu_09"));
    s.yields(user([{ type: "text", text: "not a result" }]));
    s.yields(result());
    await vi.waitFor(() => expect(s.out.at(-1)).toEqual({ type: "turn_end" }));
    expect(s.out).toEqual([
      { type: "tool_result", id: "toolu_03", output: "1 passed\ndone", is_error: false },
      { type: "tool_result", id: "toolu_04", output: "no such file", is_error: true },
      { type: "tool_result", id: "toolu_05", output: "", is_error: false },
      { type: "turn_end" },
    ]);
  });

  it("a successful result ends the turn", async () => {
    const s = await inTurn();
    s.yields(result());
    await vi.waitFor(() => expect(s.out).toEqual([{ type: "turn_end" }]));
  });

  it("a message the result did not answer starts the next turn, even with nothing queued", async () => {
    const s = await inTurn();
    s.send({ type: "user_message", text: "and a second" });
    const [first, second] = await s.prompts(2);
    expect(second.uuid).not.toBe(first.uuid);
    // The second came after claude counted its queue.
    s.yields(result({ user_message_uuids: [first.uuid], queued_turn_count: 0 }));
    await vi.waitFor(() => expect(s.out).toEqual([{ type: "turn_end" }, { type: "turn_start" }]));

    s.send({ type: "interrupt" });
    await vi.waitFor(() => expect(s.q.interrupt).toHaveBeenCalledOnce());
    s.yields(result({ subtype: "error_during_execution", is_error: true, errors: [], user_message_uuids: [second.uuid] }));
    await vi.waitFor(() => expect(s.out.at(-1)).toEqual({ type: "turn_end", interrupted: true }));
    expect(s.out).toHaveLength(3);
  });

  it("output with no turn running starts one, so a turn a result named nothing for still ends", async () => {
    const s = await inTurn();
    s.send({ type: "user_message", text: "and a second" });
    await s.prompts(2);
    // A zeroed result names no messages, so it answers them all.
    s.yields(result({ queued_turn_count: 0 }));
    await vi.waitFor(() => expect(s.out).toEqual([{ type: "turn_end" }]));

    const messageStart = { type: "stream_event", event: { type: "message_start", message: {} }, parent_tool_use_id: null };
    s.yields(messageStart);
    // A second API call in the same turn starts no other.
    s.yields(messageStart);
    s.yields({
      type: "stream_event",
      event: { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: "second" } },
      parent_tool_use_id: null,
    });
    s.yields(result());
    await vi.waitFor(() =>
      expect(s.out).toEqual([
        { type: "turn_end" },
        { type: "turn_start" },
        { type: "text_delta", text: "second" },
        { type: "turn_end" },
      ]),
    );
  });

  it("messages answered by one turn start no next turn", async () => {
    const s = await inTurn();
    s.send({ type: "user_message", text: "and a second" });
    const [first, second] = await s.prompts(2);
    s.yields(result({ user_message_uuids: [first.uuid, second.uuid], queued_turn_count: 0 }));
    await vi.waitFor(() => expect(s.out).toEqual([{ type: "turn_end" }]));
  });

  it("an assistant error ends the turn with its kind and text", async () => {
    const s = await inTurn();
    s.yields(assistant([{ type: "text", text: "API Error: 529 overloaded" }], { error: "overloaded" }));
    s.yields(result({ is_error: true, result: "API Error: 529 overloaded" }));
    await vi.waitFor(() =>
      expect(s.out).toEqual([{ type: "turn_end", error: { kind: "overloaded", message: "API Error: 529 overloaded" } }]),
    );
  });

  it("an error result ends the turn with its subtype", async () => {
    const s = await inTurn();
    s.yields(result({ subtype: "error_max_turns", is_error: true, errors: ["Reached the maximum number of turns (3)"] }));
    await vi.waitFor(() =>
      expect(s.out).toEqual([
        { type: "turn_end", error: { kind: "error_max_turns", message: "Reached the maximum number of turns (3)" } },
      ]),
    );
  });

  it("a result outside a turn is skipped", async () => {
    const s = start();
    await vi.waitFor(() => expect(s.out).toHaveLength(3));
    s.yields(result());
    s.yields({ type: "system", subtype: "commands_changed", commands: [] });
    await vi.waitFor(() => expect(s.out).toHaveLength(4));
    expect(s.out.at(-1)).toEqual({ type: "commands", commands: [] });
  });
});

describe("approvals", () => {
  it("ask the core, and wait for its answer", async () => {
    const s = await inTurn();
    const input = { command: "npm test", description: "Run the tests" };
    const { result } = s.ask("Bash", input, "toolu_01");
    await vi.waitFor(() =>
      expect(s.out).toEqual([{ type: "approval_request", id: "toolu_01", tool: "Bash", input }]),
    );
    let answered = false;
    void result.then(() => (answered = true));
    await settle();
    expect(answered).toBe(false);

    s.send({ type: "answer", id: "toolu_01", reply: "allow_once" });
    expect(await result).toEqual({ behavior: "allow", updatedInput: input });
  });

  it("allow_session passes the call's suggestions on", async () => {
    const s = await inTurn();
    const suggestions: PermissionUpdate[] = [
      { type: "addRules", rules: [{ toolName: "Bash", ruleContent: "npm test" }], behavior: "allow", destination: "session" },
    ];
    const { result } = s.ask("Bash", { command: "npm test" }, "toolu_01", suggestions);
    const plain = s.ask("Bash", { command: "ls" }, "toolu_02");
    await vi.waitFor(() => expect(s.out).toHaveLength(2));
    s.send({ type: "answer", id: "toolu_01", reply: "allow_session" });
    s.send({ type: "answer", id: "toolu_02", reply: "allow_session" });
    expect(await result).toEqual({ behavior: "allow", updatedInput: { command: "npm test" }, updatedPermissions: suggestions });
    expect(await plain.result).toEqual({ behavior: "allow", updatedInput: { command: "ls" } });
  });

  it("deny returns a deny, with the user's message or a short one", async () => {
    const s = await inTurn();
    const first = s.ask("Bash", { command: "npm i" }, "toolu_01");
    const second = s.ask("Bash", { command: "rm -rf x" }, "toolu_02");
    await vi.waitFor(() => expect(s.out).toHaveLength(2));
    s.send({ type: "answer", id: "toolu_02", reply: "deny" });
    s.send({ type: "answer", id: "toolu_01", reply: "deny", message: "use pnpm, not npm" });
    expect(await first.result).toEqual({ behavior: "deny", message: "use pnpm, not npm" });
    expect(await second.result).toMatchObject({ behavior: "deny", message: expect.stringMatching(/.+/) });
  });

  it("an answer for an id that is not pending is skipped", async () => {
    const s = await inTurn();
    const { result } = s.ask("Bash", {}, "toolu_01");
    await vi.waitFor(() => expect(s.out).toHaveLength(1));
    s.send({ type: "answer", id: "toolu_99", reply: "allow_once" });
    s.send({ type: "answer", id: "toolu_01", reply: "deny" });
    expect(await result).toMatchObject({ behavior: "deny" });
    s.send({ type: "answer", id: "toolu_01", reply: "allow_once" });
    await settle();
  });

  it("end with the turn", async () => {
    const s = await inTurn();
    const asked = s.ask("Bash", {}, "toolu_01");
    await vi.waitFor(() => expect(s.out).toHaveLength(1));
    s.yields(result());
    expect(await asked.result).toMatchObject({ behavior: "deny" });
  });

  it("end when the SDK aborts them", async () => {
    const s = await inTurn();
    const { result, abort } = s.ask("Bash", {}, "toolu_01");
    await vi.waitFor(() => expect(s.out).toHaveLength(1));
    abort.abort();
    expect(await result).toMatchObject({ behavior: "deny" });
  });
});

describe("questions", () => {
  const input = {
    questions: [
      {
        question: "Which test runner?",
        header: "Runner",
        multiSelect: false,
        options: [
          { label: "vitest", description: "Fast, Vite native" },
          { label: "jest", description: "What the repo used before" },
        ],
      },
      {
        question: "Which kinds?",
        header: "Kinds",
        multiSelect: true,
        options: [
          { label: "unit", description: "Small" },
          { label: "integration", description: "Big" },
        ],
      },
    ],
  };

  it("AskUserQuestion asks the core, and the answers go back in the SDK's form", async () => {
    const s = await inTurn();
    const { result } = s.ask("AskUserQuestion", input, "toolu_02");
    await vi.waitFor(() =>
      expect(s.out).toEqual([
        {
          type: "question_request",
          id: "toolu_02",
          questions: [
            {
              question: "Which test runner?",
              header: "Runner",
              multi_select: false,
              options: [
                { label: "vitest", description: "Fast, Vite native" },
                { label: "jest", description: "What the repo used before" },
              ],
            },
            {
              question: "Which kinds?",
              header: "Kinds",
              multi_select: true,
              options: [
                { label: "unit", description: "Small" },
                { label: "integration", description: "Big" },
              ],
            },
          ],
        },
      ]),
    );
    s.send({ type: "answer", id: "toolu_02", reply: "answers", answers: [["vitest"], ["unit", "integration"]] });
    expect(await result).toEqual({
      behavior: "allow",
      updatedInput: { ...input, answers: { "Which test runner?": "vitest", "Which kinds?": "unit, integration" } },
    });
  });

  it("deny declines the question", async () => {
    const s = await inTurn();
    const { result } = s.ask("AskUserQuestion", input, "toolu_02");
    await vi.waitFor(() => expect(s.out).toHaveLength(1));
    s.send({ type: "answer", id: "toolu_02", reply: "deny" });
    expect(await result).toMatchObject({ behavior: "deny" });
  });
});

describe("commands from the core", () => {
  it("interrupt calls the SDK's interrupt, and the turn ends interrupted", async () => {
    const s = await inTurn();
    s.send({ type: "interrupt" });
    await vi.waitFor(() => expect(s.q.interrupt).toHaveBeenCalledOnce());
    s.yields(result({ subtype: "error_during_execution", is_error: true, errors: [] }));
    await vi.waitFor(() => expect(s.out).toEqual([{ type: "turn_end", interrupted: true }]));
  });

  it("interrupt with no turn running is skipped", async () => {
    const s = start();
    s.send({ type: "interrupt" });
    s.send({ type: "user_message", text: "go" });
    await vi.waitFor(() => expect(s.out).toContainEqual({ type: "turn_start" }));
    expect(s.q.interrupt).not.toHaveBeenCalled();
  });

  it("set_model calls setModel and applyFlagSettings, each only when given", async () => {
    const s = start();
    s.send({ type: "set_model", model: "opus", effort: "high" });
    s.send({ type: "set_model", effort: "low" });
    s.send({ type: "set_model", model: "sonnet" });
    await vi.waitFor(() => expect(s.q.setModel).toHaveBeenCalledTimes(2));
    expect(s.q.setModel.mock.calls).toEqual([["opus"], ["sonnet"]]);
    expect(s.q.applyFlagSettings.mock.calls).toEqual([[{ effortLevel: "high" }], [{ effortLevel: "low" }]]);
  });

  it("set_model during a turn waits for the turn to end, so the turn keeps its model", async () => {
    const s = await inTurn();
    s.send({ type: "set_model", model: "haiku", effort: "low" });
    s.send({ type: "set_model", effort: "max" });
    await settle();
    expect(s.q.setModel).not.toHaveBeenCalled();
    expect(s.q.applyFlagSettings).not.toHaveBeenCalled();

    s.yields(result());

    await vi.waitFor(() => expect(s.out).toEqual([{ type: "turn_end" }]));
    expect(s.q.setModel.mock.calls).toEqual([["haiku"]]);
    expect(s.q.applyFlagSettings.mock.calls).toEqual([[{ effortLevel: "max" }]]);
  });

  it("lines that are not protocol messages are skipped", async () => {
    const s = start();
    s.send({ type: "something_new" });
    s.send({ type: "user_message" });
    s.sendRaw("not json");
    s.send({ type: "user_message", text: "go" });
    const [message] = await s.prompts(1);
    expect(message.message.content).toBe("go");
  });

  it("end of stdin ends the conversation, and run returns", async () => {
    const s = start();
    const { result } = s.ask("Bash", {}, "toolu_01");
    s.endStdin();
    await s.done;
    expect(s.q.close).toHaveBeenCalled();
    expect(await result).toMatchObject({ behavior: "deny" });
  });
});

describe("the SDK", () => {
  it("ending the conversation on its own is an error", async () => {
    const s = start();
    s.q.close();
    await expect(s.done).rejects.toThrow(/ended/);
  });
});
