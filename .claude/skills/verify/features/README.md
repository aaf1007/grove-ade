# grove verification map

This folder is the maintained source for verifying what a grove user can do. Read this index, then the matching feature file, before you drive grove. One file per section of `docs/acceptance.md`. The acceptance table says what passes; these files say how to reach it and what proves it.

## Baseline preconditions

- **App:** a bundle built from the worktree under test, launched with `open -n --env HOME=<run>/home …` as `../SKILL.md` says, the pid noted, Doctor passed, and no other grove running.
- **Headless:** `headless.py start` printed a run folder and `headless.py doctor <run>` printed `OK`. The test repo is `<run>/home/repo` with one commit on `main`, already tracked.
- Never drive a grove this run did not start, and never the owner's real `~/.grovecode`.

## Driving conventions

- Start every recipe from the baseline unless its preconditions say otherwise.
- App: background `app_*` tools only, accessible names over coordinates, a fresh `app_screenshot` after each step that should change the window.
- Headless: every action is a `headless.py call`, `wait` or `grovecode` command. Wait for a line in an evidence file, never a fixed sleep.
- `wait` reads the whole file, so an older match ends it at once. Before an action that should add a line, run `headless.py settle <run> <file>` so late lines from earlier steps are in, then note `wc -l < <run>/evidence/<file>` and pass it as `--after`. Every "wait for …" and "note `L`" in these files means this.
- Commands are literal. In headless commands `<run>` is the folder `start` printed, and `N` is the id a `spawn_agent` reply gave.
- A recipe that changes the test repo leaves it changed. Use a new run for the next feature that needs a clean repo.

## Proof and skip reporting

- Capture the action and the resulting state: a screenshot file, or the `call` reply plus the event or message line it caused.
- Check side effects too: the file in the repo, the fake agent's own log of what it received, grove's state under `<run>/home/.grovecode/`.
- A timing criterion is "observed, not measured" unless timestamps in `events.log` or `session-<id>.log` time it.
- A sub-feature a surface cannot reach is "not run" on that surface, with the reason. Never report it as verified through the other surface.
- A surface tagged "not mapped yet" has no recipe in the file. Report the sub-feature "not run (no recipe yet)" on that surface, or write the recipe first and prove it.
- Typometer latency, vttest against a reference terminal, and anything that needs full-screen control are "not run (manual)". The owner runs them.

## Feature entry contract

Each feature file starts with an H1 and one paragraph on the user-visible behavior, then exactly four H2s in this order:

1. `Sub-features`: short IDs, one line each, matching the acceptance test names, with the surfaces that reach each.
2. `How to get to it (user POV)`: every user entry point.
3. `Driving it with app_* and headless.py`: `Preconditions:`, then labeled bullets pairing each action with the exact tool call or command and the observable result.
4. `Gotchas`: traps that waste or invalidate a run.

## Features

- [Terminal correctness](./terminal-correctness.md): terminal panels and terminal mode render and take keys correctly, hidden or not.
- [Chat mode](./chat-mode.md): agent sessions in chat mode: login, approvals, questions, interrupt, model and mode switch, resume.
- [Diff correctness](./diff-correctness.md): the Diff panel matches git and updates live.
- [Performance](./performance.md): eight sessions streaming without slowing typing or the machine.
- [Status and review](./status-and-review.md): agent states, the attention queue, notifications, review batches, restart and profiles.
- [Agent-shaped workspace](./agent-shaped-workspace.md): agents change the layout and profile through `grove-mcp`, and spawn subagents.
