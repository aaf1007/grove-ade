# Claude Code payloads and screens

The fake's payloads and screens come from the real CLI. The version is in
[payloads/VERSION](payloads/VERSION), Claude Code 2.1.311. Re-record them when
grove moves to a newer CLI.

## Payloads

`payloads/<event>[-<qualifier>].json` is the stdin of one hook call. The
qualifier is the field the CLI matches matchers on.

| File | Fired when |
|---|---|
| `SessionStart-startup` | A new session starts. In an untrusted folder, only after the user accepts the trust dialog. |
| `SessionStart-resume` | `--resume <id>` starts. Keeps the id. |
| `SessionStart-clear` | After `/clear`, with a new session id. |
| `SessionEnd-clear` | On `/clear`, for the old session id. |
| `SessionEnd-prompt_input_exit` | On `/exit`. |
| `UserPromptSubmit` | A prompt is sent, including the first prompt from argv. |
| `PreToolUse-Bash` | Before a Bash call. |
| `PermissionRequest-Bash` | When the Bash approval prompt shows. |
| `Notification-permission_prompt` | About 6 s after an approval prompt shows. Grove ignores it. It is here to test that. |
| `PostToolUse-Bash` | After the approved Bash call. |
| `PreToolUse-AskUserQuestion` | When the question shows. |
| `PermissionRequest-AskUserQuestion` | Right after it. The CLI fires this for the question tool too. |
| `PostToolUse-AskUserQuestion` | After the user answers. |
| `PreToolUse-Agent` | Before a subagent call. |
| `PostToolUse-Agent` | Right after the call, because the subagent runs in the background. |
| `SubagentStop` | The subagent ends. Carries the parent's `session_id`. |
| `Stop` | A turn ends. |
| `StopFailure` | A turn ends on an API error. An HTTP 400 gave `"error": "unknown"`. |

`record/record.py` recorded all of them. None is hand-written.

Scrubbing put placeholders (see [../README.md](../README.md)) in place of the
session ids, the scratch folder, HOME, the CLI's temp folder, the project slug
in transcript paths, and the prompt in `UserPromptSubmit`. Other ids
(`prompt_id`, `agent_id`, `tool_use_id`) are random values from the recording
run and stay as they are.

## Screens

`screens/<name>.ansi` is the raw terminal output from launch up to one moment,
at 120 columns by 40 rows:

| File | Shows |
|---|---|
| `trust-dialog` | The folder trust dialog. "No, exit" is selected. |
| `idle-prompt` | The input prompt after a turn that ran Bash and ended, no error. |
| `error-prompt` | The input prompt with `API Error: 400 fixture: forced failure` above it. |

The folder path is `{{cwd}}`. Terminal queries (DA1, XTVERSION, kitty keyboard,
OSC 7501) are removed, so printing a screen never makes the terminal answer
into the fake's stdin. After the trust dialog the CLI switches to the alternate
screen and turns on mouse tracking, and the screens keep those sequences.

## Recording

```
python3 fixtures/fake-agents/claude-code/record/record.py "$(command -v claude)"
```

It takes under a minute and rewrites `payloads/` and `screens/`. Review the
diff before committing. It exits 1 if a step timed out or anything personal
survived scrubbing.

How it stays safe:

- The CLI runs with a scratch HOME and a scratch git repo in a new folder under
  the system temp folder, deleted at the end (`--keep` keeps it). It never
  reads or writes `~/.claude` or `~/.claude.json`.
- `ANTHROPIC_BASE_URL` points at `record/fake_api.py` on localhost, with a
  dummy API key. It needs no account and calls no real model, so it costs
  nothing. The fake API picks replies from markers in the prompt:
  `GROVE_BASH`, `GROVE_ASK`, `GROVE_AGENT`, `GROVE_FAIL`.
- `DISABLE_AUTOUPDATER` and `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` are set.
- `--settings` points every hook at `record/relay.sh`, which saves the payload,
  prints nothing and exits 0, like `grove-hook`.
- The only trust dialog accepted is the scratch repo's.

The recording passes `--permission-mode default`, because the CLI's own default
is now auto mode, which settles most tool calls without an approval prompt.

`record/pty_drive.py`, adapted from the spike #9 scripts, drives three launches
through a PTY. The first accepts the trust dialog, approves a Bash call and
quits with `/exit`. The second resumes, answers a question, runs `/clear`,
starts a subagent and quits with a double Ctrl-C. The third sends a prompt
whose turn fails.
