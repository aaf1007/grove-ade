# Chat session

A recorded Claude chat session, as the sidecar's protocol stream, for the
Performance tests in `docs/acceptance.md`. Those run three fake chat-mode
sessions that replay it next to two real ones.

| File | What it is |
|---|---|
| `session.jsonl` | Every protocol line of one real session, both ways, each with its time. |
| `record.py` | Records a new `session.jsonl`. |
| `check.py` | Checks the recording. |

## Format

One JSON object per line:

```json
{"t":12.345,"from":"sidecar","message":{"type":"text_delta","text":"I'll add"}}
```

- `t`: seconds since the recording started. It never goes back.
- `from`: `core` for a line the core sent, `sidecar` for one the sidecar sent.
- `message`: the line itself, as in
  [sidecar/claude/PROTOCOL.md](../../sidecar/claude/PROTOCOL.md).

Paths and the session id are placeholders, filled when the fake replays it:
`{{cwd}}` for the scratch repo, `{{home}}` for the home folder and
`{{session_id}}`.

## Replay

The fake sidecar's `replay` step plays it at its recorded speed or faster. It
sends each `sidecar` line at its time and waits for each `core` line, so each
recorded user message has to be sent for the replay to go on. See
[../fake-agents/README.md](../fake-agents/README.md#replay).

The core's tests in `crates/grove-core/tests/chat.rs` replay it and check that
the conversation the core sends to a panel equals the recording:

- the busiest 2 s at 1x and the busiest 10 s at 10x, also checking each
  message comes on time;
- the whole recording at 100x.

The whole recording at 10x and at 1x are ignored tests, since they take over
a minute and over 10 minutes. Run them with:

```
cargo test -p grove-core --test chat whole_recorded_session -- --ignored
```

## What it holds

Seven turns of Claude Code 2.1.318 through the sidecar (Agent SDK 0.3.319),
11.5 minutes in all, 379 KB. In an empty scratch repo the agent builds a
Markdown table of contents tool in Python with tests, then adds features and
fixes what its own review finds, committing as it goes.

| | Count |
|---|---|
| User messages | 7, one per turn |
| Text deltas | 2,576, about 37,000 characters. The longest run, a summary, is 8,400. |
| Tool calls and results | 82 each: 39 Edit, 32 Bash, 8 Read, 3 Write |
| Approvals and questions | None |

The replay sends each user message's turn only after the core sends a
`user_message`. A recorded user message waits as long as the user does, so a
fake session in the Performance run needs seven messages to play it whole. Any
text works, since the fake only checks the type.

## Record a new one

```
(cd sidecar/claude && npm ci)
python3 fixtures/chat-session/record.py
```

It needs Node, the sidecar's packages and your own `claude`, logged in. It
spends about 10 minutes of real usage on your login, so record only when the
protocol changes. It rewrites `session.jsonl`, then run `check.py`.

- The agent works in a scratch git repo under `/tmp`, deleted at the end
  (`--keep` keeps it). The repo has its own git identity.
- The settings file allows every tool and turns hooks off, so no approval
  stops the turn. A request that still comes is denied.
- The recorder sends the first prompt, and more prompts from its list while
  the session is under 10 minutes long.
- `claude` gets your shell's environment minus every `CLAUDE*` and
  `ANTHROPIC*` variable, so a host app such as Claude Code cannot change
  which login it uses.

Scrubbing:

- Paths: the scratch repo becomes `{{cwd}}`, your home `{{home}}`, the
  scratch folder `/tmp/scratch`, `/private/tmp/` becomes `/tmp/`, and your
  username and hostname become `user` and `host`.
- The session id becomes `{{session_id}}`.
- The account becomes `user@example.com` with no organization. The plan
  stays.
- Dropped: the `commands` list, which names your own skills and plugins, and
  any approval or question with the recorder's answer, which a replay could
  not play without a user.

It exits 1 if a turn failed or anything personal survived: the scratch folder, your home,
username, hostname, git name, account email or organization, the session id,
or anything `check.py` flags. Then it writes the result into the scratch
folder, never into the repo.

## Check

```
python3 fixtures/chat-session/check.py
```

It checks that the recording:

- has only lines replay can play: `user_message` from the core, and no
  approvals or questions from the sidecar;
- lasts at least 10 minutes;
- has at least 20 tool calls and results, a file change, a Bash call and a
  run of at least 2,000 characters of text;
- is under 2 MB;
- has nothing secret-looking: home or temp paths, token or key prefixes,
  UUIDs, emails other than `example.com`, `.local` hostnames;
- has a placeholder account.

It runs in CI's `ts` job.
