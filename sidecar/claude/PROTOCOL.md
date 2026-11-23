# Claude sidecar protocol

The core runs one Claude sidecar per agent session in chat mode (ADR 0015). They talk over
the sidecar's stdin and stdout, one JSON object per line, UTF-8, each line ending in `\n`. This
file is the contract. The fake sidecar in `fixtures/fake-agents/claude-sidecar` speaks it in the
core's tests.

## Launch

```
<sidecar> --settings <file> --mcp-config <file> --session-id <uuid> --claude <path> [--resume]
```

| Flag | Meaning |
|---|---|
| `--settings <file>` | The session's `claude-settings.json`, with grove's hooks. The same file terminal mode passes to `claude --settings`. |
| `--mcp-config <file>` | The session's `mcp.json`. The same file terminal mode passes to `claude --mcp-config`. |
| `--session-id <uuid>` | The conversation's id, which grove picks. Hooks carry it, and terminal mode resumes it with `claude --resume`. |
| `--claude <path>` | The `claude` to drive, as grove's config names it. A bare name is looked up on `PATH`. |
| `--resume` | Go on with the conversation `--session-id` names, through the SDK's `resume`, and send its `history` first. Claude writes its record only after the first message, so when the record holds no messages, the sidecar sends an empty `history` and starts a new conversation with that id. Without `--resume` it starts a new conversation with that id. |

- The sidecar runs in the worktree, with the session's environment: the user's login shell
  environment plus `GROVE_SESSION_ID` and `GROVE_SOCKET`.
- It writes nothing into the worktree. Its stderr goes to `sidecar.log` in the session's folder
  under grove's home, for debugging. The core never reads it.
- It leads its own process session, so `kill` ends it and everything it started.

## Core to sidecar

### `user_message`

Starts a turn with the user's message.

```json
{"type":"user_message","text":"add a test for the parser"}
```

End of stdin means the core is done with the session. The sidecar ends the conversation and
exits 0.

### `answer`

The user's answer to a pending `approval_request` or `question_request`, by its `id`. The core
sends at most one per request, and only while it is pending. `reply` is one of:

| `reply` | For | Meaning |
|---|---|---|
| `allow_once` | Approvals | Run this call. |
| `allow_session` | Approvals | Run this call, and stop asking for calls like it for the rest of the session. |
| `deny` | Both | Do not run it. An optional `message` says why, or what to do instead. |
| `answers` | Questions | `answers` has one list per question, in the request's order: the labels of the chosen options, or the user's own text. A list may be empty. |

```json
{"type":"answer","id":"toolu_01","reply":"allow_once"}
```

```json
{"type":"answer","id":"toolu_01","reply":"allow_session"}
```

```json
{"type":"answer","id":"toolu_01","reply":"deny","message":"use pnpm, not npm"}
```

```json
{"type":"answer","id":"toolu_02","reply":"answers","answers":[["vitest"],["unit","integration"]]}
```

How the sidecar answers the SDK's `canUseTool`:

- `allow_once`: `{behavior: "allow", updatedInput: input}`.
- `allow_session`: the same, plus `updatedPermissions` set to the call's `suggestions`. Without
  suggestions it is `allow_once`.
- `deny`: `{behavior: "deny", message}`. Without a `message` the sidecar writes its own short one,
  since the SDK needs one.
- `answers`: `{behavior: "allow", updatedInput: {...input, answers}}`, where `answers` maps each
  question's `question` text to its list joined with `", "`, the SDK's form for multi-select.

An `answer` for an id that is not pending, such as one the turn's end already dropped, is
skipped.

### `interrupt`

Stops the current turn, through the SDK's `interrupt()`. The sidecar then ends the turn with
`turn_end` and `interrupted`. Pending requests end with the turn. With no turn running it is
skipped.

```json
{"type":"interrupt"}
```

### `set_model`

Sets the model, the effort or both, from the next turn on, through the SDK's `setModel` and
`applyFlagSettings({ effortLevel })`. A missing field stays as it is. `model` is an `id` from
`models`, and `effort` one of that model's `efforts`. During a turn the sidecar keeps the choice
until the turn ends, so the running turn keeps its model. Several during one turn add up, the
later field winning.

```json
{"type":"set_model","model":"opus","effort":"high"}
```

### `side_question`

Asks a side question: a question answered in a fork of the conversation, which stays out of the
conversation. `id` is the core's number for it. It works during a turn too.

```json
{"type":"side_question","id":1,"text":"which test covers the lexer?"}
```

- The fork is a second SDK `query()` with `resume` on the conversation's id, `forkSession: true`
  and `persistSession: false`, so Claude writes no record for it. It runs with the session's
  model and effort.
- A conversation with no record yet has nothing to fork, so the question is asked with no
  history.
- The fork gets neither `--settings` nor `--mcp-config`. No grove hook fires for it and it has
  no `grove-mcp`, so it sets no agent state and opens no panel.
- The fork only reads. A `PreToolUse` hook denies every tool but `Read`, `Grep` and `Glob`, and
  `canUseTool` denies every request, so it asks for no approval and no question.
- One side question is open at a time. A new one ends the one before.
- The fork ends when its answer is whole.

### `end_side_question`

Ends the side question `id` and closes its fork. Nothing more comes for it. With another `id`
open, or none, it is skipped. End of stdin ends the open side question too.

```json
{"type":"end_side_question","id":1}
```

## Sidecar to core

### `history`

The conversation so far, read from Claude's own record of it with the SDK's
`getSessionMessages`, in the worktree's project. Sent only with `--resume`, as the first line,
before any other. `messages` is empty when the record cannot be read, and when there is none
yet, as for a session that never got a message.

```json
{"type":"history","messages":[{"type":"user","text":"fix the parser"},{"type":"turn_start"},{"type":"text_delta","text":"Done."},{"type":"turn_end"}]}
```

Each message the user typed starts a turn, as a `user` message with its `text` followed by
`turn_start`, and the turn ends with `turn_end` before the next one. In between come the
turn's `text_delta` (one per text block, whole), `tool_call` and `tool_result` messages, in the
shapes below. Subagents' messages are left out, as they are live. The record keeps no
interrupts or errors, so every turn ends plain. The core adds the messages to the
conversation and sets no state from them.

In a `history`, `turn_start`, `tool_call` and `turn_end` also carry `time_unix_ms`: when Claude
wrote the message to its record, in milliseconds since the Unix epoch. A turn starts at its user
message's time and ends at the time of its last message. The field is left out when the record
has no time. Live messages never carry it: the core stamps the time it gets them.

```json
{"type":"tool_call","id":"toolu_1","tool":"Bash","input":{"command":"ls"},"time_unix_ms":1763560921500}
```

### `account`

Who is logged in, as the agent reports it. Sent once the sidecar knows, and again if it
changes. `account` is null when nobody is logged in. Every field may be null or missing.

```json
{"type":"account","account":{"email":"ann@example.com","organization":"Ann's org","plan":"max","api_key_source":null}}
```

| Field | Meaning |
|---|---|
| `email` | The logged-in account's email. |
| `organization` | Its organization. |
| `plan` | The subscription plan, such as `pro` or `max`. Null with an API key. |
| `api_key_source` | Where the API key came from, such as `ANTHROPIC_API_KEY`. Null with a subscription login. |

The core sends it on as `CoreEvent::Account`. grove never sees or stores credentials.

### `models`

The models the user can pick, from the SDK's `supportedModels()`. Sent once the sidecar knows
them, and sent empty if the SDK cannot list them, so the core never waits for it. The core's
`models` call returns them.

```json
{"type":"models","models":[{"id":"default","name":"Default (recommended)","description":"Opus 5.5","efforts":["low","medium","high","xhigh","max"],"default_effort":null,"is_default":true}]}
```

| Field | Meaning |
|---|---|
| `id` | What `set_model` takes, such as `default` or `opus`. |
| `name` | The name the picker shows. |
| `description` | A short line about it. |
| `efforts` | The efforts it takes, in order. Empty when it takes none. |
| `default_effort` | Its default effort, or null. The SDK names none, so the sidecar sends null. |
| `is_default` | It is the user's default, the SDK's `default` entry. |

### `turn_start`

The agent started working on a message. The session becomes `Working`.

```json
{"type":"turn_start"}
```

### `text_delta`

The next piece of the agent's reply text, in order.

```json
{"type":"text_delta","text":"I'll add the test"}
```

### `tool_call`

The agent called a tool: one per `tool_use` block of an assistant message. `id` is the call's
id, which an `approval_request` for the call carries too. `input` is the tool's input as the SDK
gave it. A tool call does not change the session's state.

```json
{"type":"tool_call","id":"toolu_03","tool":"Bash","input":{"command":"npm test","description":"Run the tests"}}
```

### `tool_result`

What a tool call returned, by the call's `id`: one per `tool_result` block of a user message.
`output` is its text blocks joined into one string. `is_error` is false when missing.

```json
{"type":"tool_result","id":"toolu_03","output":"1 passed","is_error":false}
```

Tool calls and results inside a subagent are not sent.

### `commands`

The slash commands and skills the user can run, from the SDK's `supportedCommands()`. Sent once
the sidecar knows them, and again whenever they change. Sent whole each time: the new list
replaces the old one. `argument_hint` may be empty.

```json
{"type":"commands","commands":[{"name":"review","description":"Review a pull request","argument_hint":"<pr>"}]}
```

### `approval_request`

The agent waits for the user to allow or deny a tool call: the SDK called `canUseTool` for a
tool other than `AskUserQuestion`. The session becomes `NeedsInput` (Approval). `id` is unique
within the session, such as the call's `toolUseID`. `input` is the tool's input as the SDK gave
it.

```json
{"type":"approval_request","id":"toolu_01","tool":"Bash","input":{"command":"npm test","description":"Run the tests"}}
```

### `question_request`

The agent asks the user questions: the SDK called `canUseTool` for `AskUserQuestion`. The
session becomes `NeedsInput` (Question). `questions` is the tool input's `questions`, with
`multiSelect` written `multi_select`. Each option has a `label` and a `description`.

```json
{"type":"question_request","id":"toolu_02","questions":[{"question":"Which test runner?","header":"Runner","multi_select":false,"options":[{"label":"vitest","description":"Fast, Vite native"},{"label":"jest","description":"What the repo used before"}]}]}
```

Several requests can wait at once, and the user answers them in any order. While any approval
waits the session is `NeedsInput` (Approval), else while a question waits it is `NeedsInput`
(Question). Once none waits it is `Working` again. A request whose `id` is already pending is
skipped.

### `turn_end`

The turn ended. Requests still pending end with it. The session becomes `Done`.

```json
{"type":"turn_end"}
```

With `interrupted`, the turn stopped because of `interrupt`, and the session becomes `Idle`.

```json
{"type":"turn_end","interrupted":true}
```

With an `error`, the turn failed and the session becomes `Failed`. `kind` is the SDK's own name
for the error: the assistant message's `error` (such as `rate_limit`, `overloaded` or
`authentication_failed`) when it has one, else the result's `subtype` (such as
`error_max_turns` or `error_during_execution`). `message` is the text the user reads.

```json
{"type":"turn_end","error":{"kind":"overloaded","message":"API Error: 529 overloaded"}}
```

### `side`

One message of the answer to side question `id`. `message` is a `turn_start`, a `text_delta` or
a `turn_end`, as above. The core sends it to the side question's own stream. It never joins the
conversation and never changes the agent state.

```json
{"type":"side","id":1,"message":{"type":"text_delta","text":"tests/lexer.rs covers it."}}
```

- The answer is text only. The fork's tool calls are not sent. Text after one starts a new
  paragraph.
- A `turn_end` is the last message for its `id`. Its `error` is as for a turn, with `kind`
  `fork` when the fork could not start or ended with no result.

## Exit

- Exit 0 is a clean end: the session becomes `Exited`. So does a `kill` from grove.
- Any other exit is a failure: the session becomes `Failed`.
- The core reads what the sidecar wrote before it exited, for up to 500 ms, before it applies
  the exit.

## Rules

- The core skips a line that is not a JSON object of a type listed here.
- Agent states from these messages and from the exit have source `Protocol` (ADR 0014). Hooks
  still run in chat mode, but they never set the state.
- States from `answer` come from the core itself, when it sends the answer. All the others come
  from the sidecar's messages.
