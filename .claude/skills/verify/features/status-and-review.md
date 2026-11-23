# Status and review

grove shows what each agent session is doing (Working, NeedsInput, Done, Idle, Failed, Exited) from the agent's own signals, keeps an attention queue of sessions waiting on the user, notifies when a session needs approval, sends review comments to an agent as one batch, restores everything after a restart, and suggests a profile for a new worktree.

## Sub-features

- `needs-input` (headless; app not mapped yet): a Claude permission request shows as NeedsInput within 1 s, by protocol in chat mode and by hook in terminal mode.
- `leaving-needs-input` (headless; app not mapped yet): after approving a 30 s command the session leaves NeedsInput within 2 s.
- `codex-status` (headless; app not mapped yet): a Codex approval shows as NeedsInput within 1 s, by protocol in chat mode and by hook in terminal mode.
- `hook-relay-down` (headless; app not mapped yet): quitting grove while an agent works causes no hook errors and no delay.
- `attention-jump` (headless for the queue; app not mapped yet): one key focuses the head of the attention queue.
- `notifications` (app; headless for the event): an approval with the window unfocused shows a macOS notification naming the worktree.
- `review-batch` (app, headless): 3 comments on 2 files arrive as one message with correct `path:line`, and a snippet with `ESC[201~` arrives as plain text.
- `restart` (app, headless): quitting and reopening restores worktrees, sessions in their mode, each agent session's workspace, and open comments.
- `profile` (headless; app not mapped yet): a new worktree in a repo with `Cargo.toml` suggests the Rust crate profile, and its setup, layout, agent and model follow it.

## How to get to it (user POV)

- The sidebar shows each worktree's session state. The attention button in the title bar (named "Nothing waiting ⌘J" when the queue is empty) or ⌘J jumps to the session waiting longest.
- In the Diff panel, select lines to add a **Comment**. Choose **Send review batch** in **Review batch**.
- **New worktree** opens the dialog that suggests a profile. The **Profile** tab shows it.
- Quit with `app_menu` `["grove", "Quit grove"]` and reopen with the `open -n --env HOME=…` command.

## Driving it with app_* and headless.py

Preconditions:

- App: Doctor passed, the test repo added, real agents logged in.
- Headless: the baseline run with default scenarios.

- **NeedsInput, chat, headless.** As in chat-mode.md's approval recipe. `events.log` has `NeedsInput { kind: Approval }, source: Protocol` and a `Notify` line naming `<run>/home/repo`.
- **NeedsInput, terminal, headless.** `spawn_agent … agent=claude mode=terminal`, `write session:=N text=$'\r'`. `events.log` has `NeedsInput { kind: Approval }, source: Hook`. The hook's `t` in `fake-claude.jsonl` and `events.log` count from different starts, so they share no clock. Report the 1 s bound "observed, not measured".
- **Codex status, headless.** The same with `agent=codex`: `mode=chat` gives source `Protocol`, `mode=terminal` gives source `Hook`.
- **Leaving NeedsInput, headless.** Use a scenario that sleeps 30 s after the answer (`{"sleep": 30}`) before `turn_end`. After `answer`, `events.log` shows `Working` within 2 s of the answer's `Answered` line.
- **Hook relay down, headless.** Start a terminal Claude session with a scenario that runs hooks for a while after a `wait_input`. Run `headless.py stop <run>` mid-run. `fake-claude.jsonl` hook lines keep `code` 0 and their `t` gaps do not grow.
- **Attention queue, headless.** With two sessions in NeedsInput, `headless.py call <run> attention` lists the older one first. The jump itself needs two waiting sessions in the app's own run, since the headless run is a separate grove. No App recipe for that yet: report the jump "not run (no recipe yet)". In the app, the button is the attention button in the title bar (`app_ax_find` title `⌘J`).
- **Notifications, app.** Keep grove unfocused (the background tools never focus it), ask an agent for a command that needs approval, and check Notification Center with the owner. Headless proves only the `Notify` event.
- **Review batch, headless.** Finish one turn on chat session `N` first. Make a two-line file, then `add_comment worktree=<run>/home/repo path=notes.txt start_line:=1 end_line:=2 body="rename this"`, a second comment on `notes.txt`, and one on `README.md`. `send_review_batch worktree=<run>/home/repo session:=N`. `session-N.log` gets one `ReviewBatch { comments: [ … ] }` with all three, and `comments` lists each as `state: Sent`. For the escape check, put `\u001b[201~` in a commented line and check it arrives as text in the fake's `stdin`.
- **Review batch, app.** In the Diff panel, select lines, add a **Comment**, repeat on a second file, then **Send review batch**. The agent panel shows one review card. This proves only that one batch arrived as one card. The three comments, their `path:line` and the `ESC[201~` snippet are "not run" on App: the headless recipe checks them in the fake's `stdin`.
- **Restart, headless.** After the steps above, add one comment and do not send it: `add_comment worktree=<run>/home/repo path=README.md start_line:=1 end_line:=1 body="keep me open"`, and check `comments` lists it as `state: Open`. Save the replies of `worktrees`, `workspace session:=N`, `sessions worktree=<run>/home/repo` and `comments worktree=<run>/home/repo`. Then `headless.py restart <run>` and call them again: each reply matches, `keep me open` is still `Open`, and the sent ones are still `Sent`. Sessions keep their ids and modes.
- **Restart, app.** `app_menu` `["grove", "Quit grove"]`, check `pgrep -lx grove` is empty, reopen with the same `open -n --env HOME=…` command, note the new pid, and `app_screenshot`: the same worktrees, panels and sessions.
- **Profile, headless.** Commit a minimal crate so the new worktree has it: `printf '[package]\nname = "verify"\nversion = "0.1.0"\nedition = "2021"\n' > <run>/home/repo/Cargo.toml`, `mkdir -p <run>/home/repo/src && touch <run>/home/repo/src/lib.rs`, `git -C <run>/home/repo add -A && git -C <run>/home/repo commit -qm crate`. Then `headless.py call <run> suggest_profile repo=<run>/home/repo` returns `Some(Suggestion { profile: "Rust crate", marker: "Cargo.toml" })`. Accept it with `use_profile repo=<run>/home/repo "name=Rust crate"`, then `create_worktree repo=<run>/home/repo branch=verify-rust`. `<new path>` holds `Cargo.toml`. `events.log` gets `Setup { … state: Running }` for the profile's `cargo fetch`, and `starting_workspace worktree=<new path>` shows the profile's panels (`[Files, Diff]`, terminal on). A session spawned there with no `agent` runs the profile's agent: `workspace session:=<its id>` shows `AgentPanel { agent: Claude, … }`. For the model, `spawn_agent worktree=<new path> agent=claude mode=chat`, send a message, and look in `fake-claude-sidecar.jsonl` for the profile's model (`opus`) in that pid's `argv` or a `set_model` `stdin` line.
- **Proof.** The state and event lines with their timestamps, the `call` replies before and after restart, and screenshots for the app steps.

## Gotchas

- Headless sessions get no login-shell PATH (the runner sets `login_shell = None`), so profile setup finds no `cargo` and ends `Failed { code: Some(127) }`. Headless proves the profile is picked and its setup starts. Report setup success "not run" headless.
- On 2026-11-16 the profile's model reached no new session: the fake got no `--model` and no `set_model` line. Neither the core's `spawn_agent` nor the app's model picker reads the profile's model; the picker sends the agent's own default. Report the model part "fail" with that evidence until grove applies it.
- `send_review_batch` to a session that has not had a state yet fails with "cannot take a review batch before its first state". Finish one turn first.
- `Notify` comes once per entry into a state, after the `AgentState` line.
- Headless never focuses a session, so `Done` never turns into `Idle` there.
- Without a marker file `suggest_profile` returns `None`. grove's built-in profiles are `Next.js app` (`next.config.*`) and `Rust crate` (`Cargo.toml`).
- After `restart`, restored sessions are not followed until `follow session:=N`, so their new messages are missing from `session-N.log` until then.
- Comments sit on lines of the current file. Commit or keep the file as it was when you added them, or they show as possibly addressed.
