# Performance

With eight sessions streaming at once, grove stays responsive: typing echoes fast, memory stays bounded, idle CPU is low, and the UI never freezes. The setup is 2 real agents in chat mode, 3 fake chat sessions replaying a recorded stream, and 3 terminal panels replaying a recorded session.

## Sub-features

- `keystroke-latency` (manual): Typometer p95 within 2x of a reference terminal, and grove's own keydown-to-echo p95 at most 30 ms. "not run (manual)".
- `memory` (app): grove and its WebKit helpers at most 800 MB, under 10% growth over 30 min, window shown.
- `cpu` (app): idle under 2%.
- `stalls` (manual): no UI freeze over 100 ms while all 8 stream. Needs an in-app trace; "not run (manual)" until one exists.
- `composer` (manual): chat composer keydown-to-paint p95 at most 30 ms. "not run (manual)".
- `core-throughput` (headless): eight sessions on the fakes stream at once and every message arrives in order. A core check, not a UI number.

## How to get to it (user POV)

- Start sessions in a worktree as usual, then watch Activity Monitor's Memory and CPU columns for grove and its WebKit helpers.

## Driving it with app_* and headless.py

Preconditions:

- App: Doctor passed. The fake replay sessions need the bundle's sidecar swapped for the fake, which the app cannot do from outside. Use real sessions and report the setup you actually ran.
- Headless: the baseline run, with `<run>/scenarios/claude-sidecar.json` set to replay the recording at speed: `{"steps": [{"send": {"type": "account", "account": {"email": "verify@example.com", "organization": null, "plan": "max", "api_key_source": null}}}, {"replay": "<repo>/fixtures/chat-session/session.jsonl", "speed": 10}, {"wait_message": "never"}]}` with `<repo>` the absolute repo root.

- **Memory and CPU, app.** With the sessions running, sample the pid you noted: `ps -o rss=,%cpu= -p "$(cat <run>/evidence/app.pid)"`, and the WebKit helpers it started: `pgrep -P "$(cat <run>/evidence/app.pid)" | xargs ps -o pid=,rss=,%cpu=,command= -p`. Sample every minute into `<run>/evidence/perf.txt`. Sum RSS for the total.
- **Core throughput, headless.** Spawn eight Claude chat sessions on `<run>/home/repo`, each with `spawn_agent … mode=chat`. Each replay waits for the recording's first user message, so send each one the text the recording starts with (the first `core` line of `fixtures/chat-session/session.jsonl`). `wait` for `TurnEnded` in each `session-<id>.log`. Each log holds the full reply, in recording order.
- **Proof.** App: `perf.txt` with timestamps. Headless: the eight session logs and their `TurnEnded` timestamps.

## Gotchas

- RSS from `ps` is not Activity Monitor's Memory column. Say which one you report.
- WebKit helper processes are children of grove, not of `open`. Find them by the noted pid's children, never by name.
- Screenshots and `ps` cannot measure latency or stalls. Leave those to the owner.
- The replay at speed 10 still takes the recording's length divided by 10. Set the `wait` timeout to match.
