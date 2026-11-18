# Load session

A recorded Claude Code session with heavy TUI output, for the Performance
tests in `docs/acceptance.md`. Those run six replays next to two real agent
sessions.

| File | What it is |
|---|---|
| `session.ansi` | Raw terminal output of one Claude Code turn at 120 x 40, recorded with `script`. |
| `replay` | Plays it in a loop forever through `pv -qL 50k`. |
| `record.py` | Records a new `session.ansi`. |
| `load_api.py` | The fake model the recorded CLI talks to. |
| `check.py` | Checks the recording and the replay. |

## Replay in a grove shell session

It needs pv: `brew install pv`.

1. Open a shell session in any worktree of the grove repo.
2. From the worktree root, run `fixtures/load-session/replay`.

It streams at 50 KB/s until you press Ctrl-C. One pass takes about 4 s, so
30 minutes is about 480 loops. `replay 100k` sets another pv rate. If the
terminal looks wrong after Ctrl-C, run `reset`.

## What it holds

One turn of Claude Code 2.1.311 in a scratch repo. The agent writes a rate
limiter and its tests, runs them and sees two fail, fixes the bug with two
edits, runs them again, writes a README and ends with a long markdown
summary. Text streams at about 70 tokens/s. The output has truecolor, cursor
movement, the alternate screen, box drawing, highlighted code, colored diffs,
tables and non-ASCII text.

The recorder removes terminal queries (DA1, XTVERSION, kitty keyboard flags,
OSC 7501) and the modes that make the terminal send input (mouse tracking,
focus events, color scheme reports). So during a replay the terminal never
answers into the shell, and Ctrl-C still stops it. The session id in the exit
message is `{{session_id}}`.

## Record a new one

```
python3 fixtures/load-session/record.py "$(command -v claude)"
```

It needs macOS (it calls BSD `script`), takes about a minute and rewrites
`session.ansi`. It exits 1 if the CLI never finished the turn or if your home
path, username, hostname, the scratch folder or the dummy key survived
scrubbing. Then run `check.py`.

It stays isolated the way #17's recorder does (see
[../fake-agents/claude-code/README.md](../fake-agents/claude-code/README.md)):

- The CLI runs with a scratch HOME and a scratch git repo under the system
  temp folder, deleted at the end (`--keep` keeps it). It never reads or
  writes `~/.claude` or `~/.claude.json`.
- `ANTHROPIC_BASE_URL` points at `load_api.py` on localhost, with a dummy key.
  No account, no model, no usage.
- `--permission-mode acceptEdits --allowedTools=Bash` keeps approval prompts
  from stopping the turn. Tools only run in the scratch repo, and every Bash
  command comes from `load_api.py`.

To change what the agent does, edit `PLAN` in `load_api.py`. Each step is the
text the model streams and, except for the last, one tool call.

## Check

```
python3 fixtures/load-session/check.py
```

It checks that the recording:

- has colors, cursor movement and non-ASCII text;
- is under 2 MB;
- has nothing secret-looking: home or temp paths, token prefixes, UUIDs,
  emails, `.local` hostnames, also with escapes removed;
- has no terminal queries or input modes.

It also checks that the replay loops past the end of the recording, and that
it streams at about 50 KB/s by default. It takes about 3 s, needs pv, and runs
in CI's `ts` job.
