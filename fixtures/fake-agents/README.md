# Fake agents

These fakes stand in for the agent CLIs, so core tests never run a real
agent. Through `CoreConfig`, an adapter launches the fake in place of the real
CLI. They need Python 3.9 or later and nothing outside the standard library,
and they run on macOS and Linux. All three import `fake_agent.py`, which holds
what they share: the scenario steps, the log, raw stdin and running command hooks.

Run the suite from the repo root:

```
python3 -m unittest discover fixtures/fake-agents
```

## Fake Claude Code: `claude`

`claude` takes the flags grove passes and nothing else:

| Flag | Notes |
|---|---|
| `--settings <file>` | Must be JSON. Every hook in it must be a command hook. |
| `--mcp-config=<file>` | Only the `=` form works. The real CLI takes several values after `--mcp-config` and swallows a trailing prompt. Must be JSON with an `mcpServers` object. |
| `--session-id <uuid>` | Sets the session id that hooks carry. |
| `--resume <uuid>` | Sets it too, for a resumed session. Not allowed together with `--session-id`. |
| `--model <model>`, `--effort <effort>` | The session's model and effort. Taken and logged in argv, with no other effect. |
| `<prompt>` | An optional first prompt. |

Anything else, a flag given twice, or a bad file exits 2 with `fake claude: ...`
on stderr, before any step runs. The real CLI silently ignores a bad
settings file. The fake fails instead, so a grove bug shows up.

Without `--session-id` or `--resume` the session id is a random UUID.

Two environment variables control it:

| Variable | Meaning |
|---|---|
| `FAKE_CLAUDE_SCENARIO` | Path of the scenario file to play. Required. |
| `FAKE_CLAUDE_LOG` | Path of the log file. Each run appends to it. No log when unset. |
| `FAKE_CLAUDE_RECORD` | The [record](#record) folder, shared with `claude-sidecar`. |

The fake reads both from its own environment, so a test has to get them into
the environment grove spawns the agent with.

On a terminal, stdin goes into raw mode the way the real CLI's does: no echo,
no line buffering, and Ctrl-C arrives as a byte instead of a signal. Output
processing stays on, so `\n` prints as `\r\n`.

### Hooks

A `hook` step loads a recorded payload, fills in its placeholders and runs every
matching command hook from the settings file, in file order:

- The payload goes to stdin as compact JSON, as the real CLI sends it.
- A group's `matcher` is checked against `tool_name` for `PreToolUse`,
  `PostToolUse` and `PermissionRequest`, `source` for `SessionStart`, `reason`
  for `SessionEnd`, `notification_type` for `Notification`, `error` for
  `StopFailure` and `agent_type` for `SubagentStop`. Other events ignore it.
  `*` or an empty matcher matches all, `A|B` matches those names exactly, and
  anything else is a regex search.
- Commands run through the shell in the fake's folder, with the fake's
  environment plus `CLAUDE_PROJECT_DIR`. The timeout is the hook's `timeout`
  in seconds, 60 by default.
- The fake captures and logs hook output and never shows it. It carries on
  whatever the exit code is.

The payloads, the screens and how they were recorded are in
[claude-code/README.md](claude-code/README.md).

## Fake Codex: `codex`

`codex` takes the arguments grove passes and nothing else:

| Arguments | Notes |
|---|---|
| `codex [flags] [<prompt>]` | A new session, with an optional first prompt. |
| `codex resume <uuid> [flags] [<prompt>]` | A resumed session. Hooks carry that session id. |
| `codex app-server [-c ...]` | Answers JSON-RPC on stdio. See [App server](#app-server). |

| Flag | Notes |
|---|---|
| `-c hooks.<Event>=<groups>` | A TOML array of groups: `[{matcher = "*", hooks = [{type = "command", command = "...", timeout = 1}]}]`. `<Event>` is one of the 12 Codex events, such as `SessionStart` or `Interrupt`. A matcher must be `*`, the only one the recording checked. A hook takes only `type = "command"`, `command` and `timeout` in whole seconds. |
| `-c mcp_servers.grove=<table>` | A `command`, plus `args` and `env_vars` as lists of strings. An `mcp_call` step starts it. |
| `-c shell_environment_policy.set.<NAME>=<value>` | String values only. Hooks never see them, as in the real CLI. |
| `-c model_reasoning_effort=<effort>` | A string. Taken with no other effect. |
| `-m <model>` | At most once, as the real CLI refuses a second one. Not with `app-server`. Taken with no other effect. |
| `--no-daemon` | At most once. Not with `app-server`, which the real CLI rejects too. |

`resume` and `app-server` must come first. Keys can be split between `-c`
flags any way TOML allows, such as `-c hooks={Stop = [...]}`, and a later flag
replaces an earlier one. A value that is not TOML is taken as a plain string,
as in the real CLI. Anything else exits 2 with `fake codex: ...` on stderr,
before any step runs: another flag such as `--dangerously-bypass-hook-trust`,
another key such as `model` or `hooks.state`, two prompts, or a resume id that
is not a UUID.

Without `resume` the session id is a random UUID.

| Variable | Meaning |
|---|---|
| `FAKE_CODEX_SCENARIO` | Path of the scenario file to play. Required, except for `app-server`, which never reads it. |
| `FAKE_CODEX_APP_SERVER_SCENARIO` | Path of the scenario `app-server` plays. Optional. See [App server](#app-server). |
| `FAKE_CODEX_ACCOUNT` | The account `account/read` returns, as JSON. `null` (no login) when unset. |
| `FAKE_CODEX_LOG` | Path of the log file. Each run appends to it. No log when unset. |
| `FAKE_CODEX_RECORD` | The [record](#record) folder, shared by the TUI and `app-server`. |
| `FAKE_CODEX_HOOK_TRUST` | `trusted` (the default), `untrusted` or `modified`. See [Hooks not trusted](#hooks-not-trusted). |
| `CODEX_HOME` | Fills `{{codex_home}}`. When unset, `$HOME/.codex`, as in the real CLI. |

On a terminal, stdin goes into raw mode, as for `claude`.

### Codex hooks

A `hook` step runs the command hooks for the payload's event, in `-c` order:

- The payload goes to stdin as compact JSON, as the real CLI sends it.
- Each command runs as `$SHELL -lc <command>` (`/bin/sh` when `SHELL` is
  unset), in the fake's folder, with the fake's own environment.
  `shell_environment_policy.set` never reaches hooks, so a hook sees
  `GROVE_SESSION_ID` only when it is in the environment the fake was spawned
  with.
- The timeout is the hook's `timeout` in seconds. Without one it is 600, or 1
  for `SessionEnd` and `Interrupt`, the defaults `hooks/list` reports.
- The fake captures and logs hook output and never shows it. It carries on
  whatever the exit code is.

### Hooks not trusted

With `FAKE_CODEX_HOOK_TRUST` set to `untrusted` or `modified` and at least one
hook, the fake prints the recorded `hooks-review` screen ("Hooks need review")
before the first step, as the real TUI does at startup. Then no hook runs.
Each `hook` step logs `hook_skipped` and `trust` instead. `untrusted` means never
approved. `modified` means approved before with other fields, such as a new
argument in the command.

### App server

`codex app-server` reads JSON-RPC messages from stdin, one per line, and
writes its own the same way. It answers these requests itself, in the shapes
`codex app-server generate-ts` gives for 0.158.0, cut down to the fields grove
reads:

- `initialize` gets `userAgent`, `codexHome`, `platformFamily` and
  `platformOs`. A second one gets error -32600 `Already initialized`.
- The `initialized` notification gets a `remoteControl/status/changed`
  notification back, as from the real server.
- `hooks/list` lists the `-c` hooks the way 0.158.0 does, with one entry per
  path in `params.cwds`, or one for the fake's folder when there is none.
  Every hook's `trustStatus` is `FAKE_CODEX_HOOK_TRUST`. `currentHash` is the
  fake's own. The same entry always gets the same hash, but not the hash the
  real CLI gives.
- `account/read` gets `FAKE_CODEX_ACCOUNT`.
- `thread/start` gets a thread with a new UUID and no turns, and
  `thread/resume` the thread its `threadId` names, which must be a UUID, with
  its [record](#record) as its `turns`. Either is followed by a
  `thread/started` notification. The fake runs one thread, so a second one
  gets an error.
- `model/list` gets two models, `gpt-5.5` (the default, efforts `low`,
  `medium` and `high`, `medium` by default) and `gpt-5.5-mini` (efforts `low`
  and `medium`, `low` by default), and no next cursor.
- `skills/list` gets one entry per path in `params.cwds`, or one for the
  fake's folder when there is none. Each lists two skills: `review`, enabled,
  and `off`, turned off.
- `turn/start` on that thread, with `input` a list of text items that each
  have `text_elements`, gets a turn with a new UUID. One turn runs at a time:
  it ends when the scenario sends `turn/completed`. An optional `model` and
  `effort` hold for that turn and the later ones, as in Codex. A thread starts
  on `gpt-5.5` at `medium`. A model not in the list, or an effort the model
  does not take, gets an error.
- `turn/interrupt` gets `{}` when it names the running turn.
- Any other request, a request that breaks these rules, and any request before
  `initialize` gets error -32600. Other notifications get nothing.
- A line that is not a JSON object exits 1. End of stdin exits 0.

With `FAKE_CODEX_APP_SERVER_SCENARIO` set, it also plays that scenario, which
takes only the `send`, `wait_message`, `record`, `sleep` and `exit` steps:

- `send` writes a JSON-RPC message: a notification, or a request to the
  client such as an approval. Placeholders fill as for the other fakes, plus
  `{{thread_id}}` and `{{turn_id}}`, the thread and the latest turn, and
  `{{model}}` and `{{effort}}`, what the latest turn runs with, and
  `{{text}}`, the latest turn's first text input.
- `wait_message` waits for the client's next request with that method, or for
  its next reply to a request with `"response"`. A request the fake rejected
  never counts. A test reads what a reply says from the log.

When the steps run out it serves until stdin closes. A turn that waits for a
command approval, then completes:

```json
{"steps": [
  {"wait_message": "turn/start"},
  {"send": {"method": "turn/started", "params": {"threadId": "{{thread_id}}", "turn": {"id": "{{turn_id}}", "status": "inProgress"}}}},
  {"send": {"id": 0, "method": "item/commandExecution/requestApproval", "params": {"threadId": "{{thread_id}}", "turnId": "{{turn_id}}", "itemId": "call_1", "command": "npm test"}}},
  {"wait_message": "response"},
  {"send": {"method": "turn/completed", "params": {"threadId": "{{thread_id}}", "turn": {"id": "{{turn_id}}", "status": "completed"}}}}
]}
```

The payloads, the screens, the recorded `hooks/list` exchanges and how they
were recorded are in [codex-cli/README.md](codex-cli/README.md).

## Fake Claude sidecar: `claude-sidecar`

`claude-sidecar` stands in for the Claude sidecar in chat mode and speaks the
protocol in [sidecar/claude/PROTOCOL.md](../../sidecar/claude/PROTOCOL.md). It
takes exactly the flags grove passes, each once: `--settings <file>`,
`--mcp-config <file>`, `--session-id <uuid>` and `--claude <path>`. It checks
both files as `claude` does and logs them. A missing, unknown or repeated flag,
a session id that is not a UUID, or a bad file exits 2 with
`fake claude-sidecar: ...` on stderr, before any step runs.

| Variable | Meaning |
|---|---|
| `FAKE_CLAUDE_SIDECAR_SCENARIO` | Path of the scenario file to play. Required. |
| `FAKE_CLAUDE_SIDECAR_LOG` | Path of the log file. Each run appends to it. No log when unset. |
| `FAKE_CLAUDE_RECORD` | The [record](#record) folder, shared with `claude`. |

With `--resume`, given once, it first sends `{"type": "history", "messages":
[...]}`, the conversation's [record](#record) read back.

It takes only the `send`, `wait_message`, `replay`, `record`, `sleep`, `exit`
and `signal` steps. It runs no hooks. A `send` step also fills `{{model}}` and
`{{effort}}`, what the current turn runs with: `default` and `default` until a
`set_model` line comes, which takes effect at the next `turn_start` the
scenario sends, as PROTOCOL.md says. A field the line leaves out stays. A turn that streams two deltas:

```json
{"steps": [
  {"send": {"type": "account", "account": {"email": "ann@example.com", "plan": "max"}}},
  {"wait_message": "user_message"},
  {"send": {"type": "turn_start"}},
  {"send": {"type": "text_delta", "text": "Hel"}},
  {"send": {"type": "text_delta", "text": "lo"}},
  {"send": {"type": "turn_end"}},
  {"wait_message": "user_message"}
]}
```

A turn that waits for an approval, then for an interrupt. The fake never
checks what an `answer` says. A test reads it from the log's `stdin` lines.

```json
{"steps": [
  {"wait_message": "user_message"},
  {"send": {"type": "turn_start"}},
  {"send": {"type": "approval_request", "id": "toolu_01", "tool": "Bash", "input": {"command": "npm test"}}},
  {"wait_message": "answer"},
  {"wait_message": "interrupt"},
  {"send": {"type": "turn_end", "interrupted": true}}
]}
```

### Replay

A `replay` step plays a recording such as
[../chat-session/session.jsonl](../chat-session/session.jsonl). A recording
has one JSON object per line: `t`, the seconds since the recording started,
`from`, `core` or `sidecar`, and `message`, a protocol message.

- A `sidecar` line is sent at its `t` divided by `speed`, with placeholders
  filled as in `send`.
- A `core` line is waited for, as `wait_message` waits for its `type`. The
  clock restarts when it comes, so a slow user never makes later lines rush.
- `t` never goes back. The path is taken as given, from the fake's folder.

The fake checks the recording with the rest of the scenario, before the first
step runs. Repeat the step to play a recording more than once. The whole
recording at 10x, then a wait until the core is done:

```json
{"steps": [
  {"replay": "/path/to/grove/fixtures/chat-session/session.jsonl", "speed": 10},
  {"wait_message": "never"}
]}
```

## Scenario format

A scenario is a JSON object with one key, `steps`, a list the fake runs in
order. Each step is an object with exactly one action key. Both fakes take the
same steps. The sidecar fake takes its own few. Payloads and screens come from
the fake's own data folder: `claude-code/` or `codex-cli/`.

| Step | Does |
|---|---|
| `{"hook": "Stop"}` | Runs the hooks for payload `payloads/Stop.json`. |
| `{"hook": "UserPromptSubmit", "set": {"prompt": "hi"}}` | Same, after replacing top-level payload fields with the ones in `set`. |
| `{"print": "text"}` | Writes the text to stdout as UTF-8. Placeholders are filled. |
| `{"screen": "trust-dialog"}` | Writes `screens/trust-dialog.ansi`, with placeholders filled. |
| `{"mode": "bracketed_paste", "on": true}` | Writes the DEC private mode set (`on: true`) or reset (`on: false`) sequence. Modes: `bracketed_paste` (2004), `focus_events` (1004), `alternate_screen` (1049), `show_cursor` (25). |
| `{"wait_input": "\r"}` | Waits until stdin has the text, as UTF-8, after the end of the last match. Exits 1 if stdin closes first. |
| `{"send": {"type": "turn_start"}}` | Sidecar only. Writes the object to stdout as one compact JSON line. Placeholders in its strings are filled. |
| `{"wait_message": "user_message"}` | Sidecar only. Waits for the next whole stdin line, after the last match, that is a JSON object with that `type`, skipping other lines. Exits 1 if stdin closes first. |
| `{"replay": "<path>", "speed": 10}` | Sidecar only. Plays a recorded protocol stream. See [Replay](#replay). `speed` is optional, 1 by default. |
| `{"record": {...}}` | Adds the object, placeholders filled, to the conversation's [record](#record). |
| `{"mcp_call": "grove", "tool": "subagent_spawn", "arguments": {...}}` | Starts that MCP server from the config grove passed, as the agent would, calls the tool, and logs the reply. See [MCP calls](#mcp-calls). |
| `{"sleep": 0.5}` | Sleeps that many seconds. |
| `{"session_id": "<uuid>"}` | Hooks from here on carry this session id, as after `/clear`. |
| `{"exit": 1}` | Exits with that code. |
| `{"signal": "KILL"}` | Sends itself the signal, with or without the `SIG` prefix, after resetting it to its default action, so `INT` and `PIPE` kill the fake too. If it is still running half a second later, as with `WINCH`, it exits 1. |

When the steps run out, the fake exits 0. It checks the whole scenario before
the first step runs. An unknown action or option, a missing payload or screen,
or a bad value exits 2.

A `/clear`, then a turn with an approved Bash call:

```json
{"steps": [
  {"hook": "SessionStart-startup"},
  {"screen": "idle-prompt"},
  {"wait_input": "\r"},
  {"hook": "SessionEnd-clear"},
  {"session_id": "0d5a7e3b-9c41-4f2a-8b6d-1e2f3a4b5c6d"},
  {"hook": "SessionStart-clear"},
  {"hook": "UserPromptSubmit", "set": {"prompt": "add a test"}},
  {"hook": "PreToolUse-Bash"},
  {"hook": "PermissionRequest-Bash"},
  {"wait_input": "1"},
  {"hook": "PostToolUse-Bash"},
  {"hook": "Stop"},
  {"wait_input": "\u0003\u0003"}
]}
```

Two Codex scenarios. The first runs a turn with an approved shell call, then
a turn with a rejected question. The second is for `codex resume <id>` and
answers a question:

```json
{"steps": [
  {"hook": "SessionStart-startup"},
  {"hook": "UserPromptSubmit"},
  {"hook": "PreToolUse-Bash"},
  {"hook": "PermissionRequest-Bash"},
  {"wait_input": "\r"},
  {"hook": "PostToolUse-Bash"},
  {"hook": "Stop"},
  {"wait_input": "\r"},
  {"hook": "UserPromptSubmit", "set": {"prompt": "ask me"}},
  {"hook": "PreToolUse-request_user_input"},
  {"sleep": 0.05},
  {"hook": "Stop"}
]}
```

```json
{"steps": [
  {"hook": "SessionStart-resume"},
  {"hook": "UserPromptSubmit"},
  {"hook": "PreToolUse-request_user_input"},
  {"wait_input": "\r"},
  {"hook": "PostToolUse-request_user_input"},
  {"hook": "Stop"}
]}
```

### MCP calls

An `mcp_call` step starts the server on stdio, sends `initialize`, the
`notifications/initialized` notification and one `tools/call`, waits for each
reply, then closes the server's stdin. Placeholders in `arguments` are filled.
The server's environment is `HOME`, `LOGNAME`, `PATH`, `SHELL`, `TERM` and `USER`
from the fake's own, plus what its config passes: `env` in `claude`'s MCP
config, and for `codex` the variables `env_vars` names, read from the fake's own
environment. So a test shows the config passes what the server needs. A server
not in the config exits 2. The call gets 10 seconds, then the server is killed.

### Placeholders

Payloads, screens and `print` text can hold these:

| Placeholder | Filled with |
|---|---|
| `{{session_id}}` | The current session id. For the sidecar, its `--session-id`. |
| `{{cwd}}` | The fake's working folder. |
| `{{home}}` | `$HOME`. |
| `{{prompt}}` | The first prompt from argv, or empty. |
| `{{text}}` | The user's last message: what came before the last `wait_input` match, or the `text` of the last `wait_message` line. Empty before either. |
| `{{project}}` | `claude` only. The working folder with every character that is not a letter or digit replaced by `-`, as in the real CLI's transcript paths. |
| `{{tmp}}` | `claude` only. `/tmp/claude-<uid>` with symlinks resolved, the real CLI's temp folder. |
| `{{codex_home}}` | `codex` only. `$CODEX_HOME`, or `$HOME/.codex` when unset. |

## Record

Each agent keeps its own record of a conversation, which grove reads back when
the conversation resumes in chat mode. The fakes keep one too: a `record` step
adds a line to `<folder>/<id>.jsonl`, where the folder is `FAKE_CLAUDE_RECORD`
or `FAKE_CODEX_RECORD` and the id is the session id, or `app-server`'s thread
id. A `record` step without the folder set exits 2. A test writes the lines in
the shape the reader expects: `history` messages for Claude, turns for Codex.

## Log format

The log has one JSON object per line. Every line has `t`, seconds since the
fake started, and `pid`, then one of these:

| Keys | Written |
|---|---|
| `argv`, `cwd`, `env` | First, at launch. `argv[0]` is the fake's path. |
| `file`, `content` | `claude` and `claude-sidecar`. For the settings file and the MCP config, as read. |
| `config` | `codex` only. The config the `-c` flags add up to, as a JSON object. |
| `stdin` | For every read from stdin, and every line for `codex app-server`. Each byte is one character from U+0000 to U+00FF, so `stdin.encode("latin-1")` gives the bytes back. |
| `hook`, `command`, `code`, `stdout`, `stderr` | After each hook command. `code` is `null` when it timed out. |
| `hook_skipped`, `trust` | `codex` only. For a `hook` step while hooks are not trusted. |
| `error` | Before a failure exit. Same text as on stderr. |
| `signal` | Before a `signal` step. |
| `mcp_call`, `reply`, `server_exit`, `stderr` | After an `mcp_call` step: the tool, the server's JSON-RPC reply to the call, its exit code and its stderr. `mcp_call` and `error` instead when the server could not start. |
| `exit` | Last, with the exit code. Missing when a signal ended the fake. |
