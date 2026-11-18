# Agent-shaped workspace

Agents shape the workspace through `grove-mcp` or its shell twin `grovecode`: open log and command panels, change the layout, change their profile, and spawn subagents that open as agent panels beside their parent. Each change shows a notice naming the agent session, with Undo.

## Sub-features

- `layout-change` (app, headless): an agent opens the Logs panel through `grove-mcp`; the notice names the agent session; Undo restores the workspace exactly.
- `profile-change` (headless; app not mapped yet): an agent changes its profile's default model; the Profile panel shows it and the next new worktree uses it.
- `subagent-panel` (headless; app not mapped yet): an agent spawns a subagent, which opens as an agent panel beside its parent in chat mode, and the user can type into it, for Claude Code and Codex.
- `panel-commands` (headless): `grovecode panel open|read|wait|close`, `workspace get|set` and `profile get|set` work from an agent session's shell and fail cleanly outside one.

## How to get to it (user POV)

- Ask the agent in a session to "open a log panel running `echo ok`", "show the Logs panel", or "spawn a Codex subagent to …". The agent calls `grove-mcp`.
- Or, in an agent session's shell, run `grovecode workspace get` and the other commands its `--help` lists.
- The notice appears in the workspace with **Undo**. The **Profile** tab shows the profile.

## Driving it with app_* and headless.py

Preconditions:

- App: Doctor passed, the test repo added, a real Claude session in chat mode.
- Headless: the baseline run and one chat Claude session `N` that has started (the fake logged its `env`): `spawn_agent worktree=<run>/home/repo agent=claude mode=chat`, then `wait <run> events.log Account`.

- **Workspace read, headless.** `headless.py grovecode <run> N workspace get`. Exit 0 and JSON with `chat`, `panels`, `selected` and `terminal`. `grovecode.log` records it.
- **Panel open, headless.** `headless.py grovecode <run> N panel open log tests "echo verify-ok"` prints `Opened panel M.` `events.log` gets `PanelRequested { parent: Some(SessionId(N)), panel: Log { session: SessionId(M), … title: "tests" } }` and `SessionExited { session: SessionId(M), code: Some(0) }`. `session-M.log` holds `verify-ok`. `grovecode <run> N panel read M` prints it too.
- **Layout change and Undo, headless.** Save the layout from `workspace get`, then change it with `grovecode <run> N workspace set '<json>'` adding `"logs"` to `panels`. `events.log` gets `WorkspaceChanged { …, by: Some(SessionId(N)) }`. `headless.py call <run> undo_workspace worktree=<run>/home/repo`, then `workspace` matches the saved layout exactly.
- **Profile change, headless.** The repo must use a profile first. Commit a minimal crate: `printf '[package]\nname = "verify"\nversion = "0.1.0"\nedition = "2021"\n' > <run>/home/repo/Cargo.toml`, `mkdir -p <run>/home/repo/src && touch <run>/home/repo/src/lib.rs`, `git -C <run>/home/repo add -A && git -C <run>/home/repo commit -qm crate`, then `headless.py call <run> use_profile repo=<run>/home/repo "name=Rust crate"`. `grovecode <run> N profile get` prints `Rust crate` with model `opus`. `grovecode <run> N profile set '{"agent": {"model": "sonnet"}}'` prints `Changed profile Rust crate.` `events.log` gets `ProfilesChanged { by: Some(SessionId(N)) }` and `headless.py call <run> profiles` shows `model: Some("sonnet")` for it. Then `create_worktree repo=<run>/home/repo branch=verify-next` and spawn a Claude chat session there: the fake's argv or a `set_model` stdin line should carry `sonnet`. See the model gotcha in status-and-review.md: on 2026-11-16 neither appears, so this part fails.
- **Subagent, headless.** Run it for both agents. Claude: `grovecode <run> N subagent spawn claude "reply with ok"` prints `Started subagent session S in <run>/home/repo.` `events.log` gets `PanelRequested { parent: Some(SessionId(N)), panel: Agent { session: SessionId(S), … } }`. `call <run> follow session:=S`; `session-S.log` gets `User { text: "reply with ok" }`. Answer its approval (`answer session:=S request=toolu_verify reply=allow_once`), wait for `TurnEnded` in `session-S.log`, then `call <run> send session:=S text="hi from verify"`: `fake-claude-sidecar.jsonl` gets a `stdin` line with `hi from verify`. Codex: the same with `subagent spawn codex`, except its approval is `request=call_verify` (the default `codex-app-server.json`), and the `send` reaches the fake Codex app-server's `stdin` in `fake-codex.jsonl`.
- **Through grove-mcp itself, headless.** Put `{"mcp_call": "grove", "tool": "panel_open", "arguments": {"kind": "log", "title": "tests", "cmd": "echo ok"}}` in `<run>/scenarios/claude.json` before a `wait_input`, spawn `mode=terminal`, and check the fake's log has the `mcp_call` reply and `events.log` the `PanelRequested`.
- **Outside a session.** `target/debug/grovecode workspace get` with no `GROVE_SESSION_ID` exits 1 and says it works only in an agent session's shell.
- **Layout change, app.** Ask the Claude session "use grove to open the Logs panel". `app_screenshot` shows the Logs tab and a notice naming the session. `app_click` **Undo**: the tabs are as before.
- **Proof.** Headless: `grovecode.log`, the `PanelRequested`, `WorkspaceChanged` and `ProfilesChanged` lines, and the `workspace` replies before and after. App: screenshots of the notice and after Undo.

## Gotchas

- `headless.py grovecode` reads the session's token from the fake's log, so the session's fake must have started. Wait for its first event before calling it.
- Panel ids share the session id counter. The id `panel open` prints is the session to read in `session-M.log`.
- `workspace set` takes the whole workspace as `workspace get` prints it. A partial object is refused.
- A repo uses no profile until one is chosen, and `profile get` or `profile set` then fails with "uses no profile". Run `use_profile` first. grove ships two built-in profiles, `Next.js app` and `Rust crate`.
- In the app, a real agent may answer in text instead of calling the tool. Ask it plainly to use grove's tool, and report what it did.
