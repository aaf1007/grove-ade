# Codex payloads, screens and app-server answers

The fake's payloads, screens and `hooks/list` answers come from the real CLI.
The version is in [payloads/VERSION](payloads/VERSION), Codex CLI 0.158.0.
Re-record them when grove moves to a newer CLI.

## Payloads

`payloads/<event>[-<qualifier>].json` is the stdin of one hook call. The
qualifier is `source` for `SessionStart` and `tool_name` for tool events.

| File | Fired when |
|---|---|
| `SessionStart-startup` | The first prompt of a new session is sent, not at launch. |
| `SessionStart-resume` | The first prompt after `codex resume <id>` is sent. Keeps the id. |
| `UserPromptSubmit` | A prompt is sent, including the first one from argv. |
| `PreToolUse-Bash` | Before a shell call. In hooks, Codex names its shell tool `Bash`. |
| `PermissionRequest-Bash` | When the approval prompt shows. |
| `PostToolUse-Bash` | After the approved call. |
| `Interrupt` | Esc on the approval prompt. |
| `PreToolUse-request_user_input` | The model asks a question. By default Codex rejects the tool and `Stop` follows 30 to 127 ms later, over three recordings. Spike #9 measured about 50 ms. With the `default_mode_request_user_input` feature on, the question shows instead. |
| `PostToolUse-request_user_input` | The user answered the question. `tool_response` has the answer. |
| `Stop` | A turn ends. It looks the same after a rejected question. |
| `SessionEnd` | On `/quit`, with `"reason": "other"`. |

`record/record.py` recorded all of them. None is hand-written.

Scrubbing put placeholders (see [../README.md](../README.md)) in place of the
session id, the scratch folder, `CODEX_HOME`, `HOME` and the prompt in
`UserPromptSubmit`. Other ids (`turn_id`, `tool_use_id`) and the date in
`transcript_path` are from the recording run and stay as they are.

Hooks got Codex's own environment plus the variables the login shell adds:
`PWD`, `SHLVL`, `_`, and on macOS `__CF_USER_TEXT_ENCODING`. The recording put one
`GROVE_SESSION_ID` in the Codex environment and another in
`shell_environment_policy.set`. Hooks and the MCP server both got the first.

## Screens

`screens/<name>.ansi` is the raw terminal output from launch up to one moment,
at 120 columns by 40 rows:

| File | Shows |
|---|---|
| `hooks-review` | The "Hooks need review" prompt at startup, for 12 hooks not trusted yet. |
| `idle-prompt` | The input prompt after the hooks were trusted and a turn ran an approved shell command. "1 warning" is about the fake model's name. |

The folder path is `{{cwd}}`. The recorder removes terminal queries, so
printing a screen never makes the terminal answer into the fake's stdin. They
are cursor position, DA1, XTVERSION, kitty keyboard, and OSC 10 and 11 colors. The screens keep
the TUI's switch to the alternate screen and mouse tracking.

## App server

`app-server/hooks-list-<state>.json` has the `args` after `codex` and every
message both ways, in order, for one `initialize`, `initialized`,
`hooks/list` exchange with `codex app-server`:

| File | Hooks |
|---|---|
| `hooks-list-untrusted` | Not approved yet. |
| `hooks-list-trusted` | Approved through "Trust all and continue". |
| `hooks-list-modified` | The approved hooks with `--session x` added to every command. |

The host name is `fixture-host`. `hooks/list` answered in 3 to 38 ms.

## Recording

```
python3 fixtures/fake-agents/codex-cli/record/record.py "$(command -v codex)"
```

It takes about a minute and rewrites `payloads/`, `screens/` and
`app-server/`. Review the diff before committing. It exits 1 if a step timed
out or anything personal survived scrubbing.

How it stays safe:

- The CLI runs with a scratch `HOME`, a scratch `CODEX_HOME` and a scratch git
  repo in a new folder under `/tmp`, deleted at the end. `--keep` keeps it.
  It never reads or writes `~/.codex`.
- The scratch `config.toml` points a model provider at `record/fake_model.py`
  on localhost, with a dummy API key, and `chatgpt_base_url` at the same
  server, so no request leaves the machine. It needs no account and calls no
  real model, so it costs nothing. The fake model picks replies from markers in
  the prompt: `GROVE_SHELL`, `GROVE_ASK`.
- The scratch config also trusts the scratch repo, and sets
  `approval_policy = "on-request"` and `sandbox_mode = "workspace-write"`, so
  the shell call needs approval.
- The flags are the ones grove passes: `--no-daemon`, `-c hooks.<Event>` for
  all 12 events running `grove-hook codex` with a 1 second timeout,
  `-c mcp_servers.grove` running `grove-mcp` with
  `env_vars = ["GROVE_SESSION_ID"]`, and
  `-c shell_environment_policy.set.GROVE_SESSION_ID`. On `PATH`, `grove-hook`
  is `record/relay.py`, which saves stdin and the environment, prints nothing
  and exits 0, and `grove-mcp` is `record/mcp_stub.py`, an MCP server with no
  tools that saves its environment.
- The recorder approves the hooks through Codex's own "Hooks need review"
  prompt, choosing "Trust all and continue", as a user would. It never passes `hooks.state` or
  `--dangerously-bypass-hook-trust`.

`../claude-code/record/pty_drive.py` drives two TUI launches. The first trusts
the hooks, approves a shell call, presses Esc at a second approval, gets a
question rejected and quits with `/quit`. Then the recorder turns on
`default_mode_request_user_input` in the scratch config. The second resumes
the session, answers a question and quits.
