# Chat mode

An agent session in chat mode shows the agent's conversation as messages in an agent panel. The user sends messages, answers approval and question cards, interrupts a turn, changes the model, switches the session to terminal mode and back, and picks the conversation up again after grove restarts.

## Sub-features

- `own-login` (app only): a Claude and a Codex session start with the user's own login, and grove shows the account and plan.
- `account-report` (headless): grove reports the account and plan the agent gives it. The fakes give a fixed `verify@example.com`, so this proves the reporting path, not a real login.
- `own-config` (app only): the session follows the repo's CLAUDE.md, lists the user's skills and slash commands in the `/` menu, and can call `grove-mcp`. Headless fakes have no CLAUDE.md or `/` menu.
- `approval-round-trip` (app, headless): an approval shows as a card and `NeedsInput` (Approval); Allow once runs it, Deny stops it.
- `question` (headless; app not mapped yet): a question shows as a card and `NeedsInput` (Question); the answer reaches the agent.
- `interrupt` (headless; app not mapped yet): interrupting mid-turn stops the agent within 2 s and leaves the session `Idle`.
- `model-switch` (headless; app not mapped yet): a new model and effort apply from the next turn.
- `mode-switch` (headless; app not mapped yet): three switches to terminal mode and back keep every message, in order.
- `resume` (headless; app not mapped yet): after quitting grove during an `Idle` session and reopening it, the agent remembers the conversation.
- `two-panels` (headless; app not mapped yet): two agent sessions in one worktree stream at once without mixing messages.

## How to get to it (user POV)

- Select the worktree in the sidebar, such as **main (main worktree)**, then choose **Start Claude Code** under it or **Start session** in the empty chat area.
- Choose **Start another agent session beside this one** on an agent panel's header for a second session.
- Pick the agent and model from **Agent and model** under the composer.
- Use the **Chat**/**Terminal** switch in the agent panel's header to change mode.
- Type in the **Message** composer and choose **Send**. Choose **Interrupt** (title "Interrupt the turn") while a turn runs.

## Driving it with app_* and headless.py

Preconditions:

- App: Doctor passed, the test repo added through **Repo path** and **Add repo**, real `claude` and `codex` logged in through their own CLIs.
- Headless: the baseline run, default scenarios. The Claude fake reports `verify@example.com`, plan `max`. The Codex fake reports `verify@example.com`, plan `plus`.

- **Own login, app.** Start a Claude session. `app_click` **Start Claude Code**, then `app_screenshot`. The account and plan show under the composer. Do the same with Codex picked in **Agent and model**.
- **Account report, headless.** Run `headless.py call <run> spawn_agent worktree=<run>/home/repo agent=claude mode=chat`, then `headless.py wait <run> events.log 'Account \{ agent: Claude'`. The line names `verify@example.com` and `max`. Repeat with `agent=codex` and wait for `Account \{ agent: Codex`.
- **Approval, app.** Send "create an empty file called qa.txt". `app_type` into **Message**, `app_click` **Send**. An approval card appears and the sidebar shows the session as needing input. Choose Allow once. `git -C <run>/home/repo status --short` shows `?? qa.txt`.
- **Approval, headless.** Send a message to session `N`: `headless.py call <run> send session:=N text="reply with the word ok"`. `headless.py wait <run> events.log 'NeedsInput \{ kind: Approval \}'` matches. Answer: `headless.py call <run> answer session:=N request=toolu_verify reply=allow_once`. `session-N.log` gets `Answered { … AllowOnce }` then `TurnEnded { outcome: Completed }`, `events.log` gets `Working` then `Done`, and `fake-claude-sidecar.jsonl` has a `stdin` line with `"reply":"allow_once"`. Repeat in a new session with `reply=deny` and check the fake got `"reply":"deny"`.
- **Approval timing, headless.** Compare the `User { … }` timestamp in `session-N.log` with the `NeedsInput` timestamp in `events.log`. The gap is under 1 s.
- **Question, headless.** Before spawning, write `<run>/scenarios/claude-sidecar.json` with a `question_request` step (shape in `sidecar/claude/PROTOCOL.md`) then `{"wait_message": "answer"}` and `{"send": {"type": "turn_end"}}`. Spawn and send. `wait` for `NeedsInput \{ kind: Question \}`. Answer with `reply:='[["vitest"]]'`. The fake's log has `"answers":[["vitest"]]`. For Codex, use an app-server scenario that sends `item/tool/requestUserInput`.
- **Interrupt, headless.** Use a scenario whose turn waits: `turn_start`, then `{"wait_message": "interrupt"}`, then `{"send": {"type": "turn_end", "interrupted": true}}`. Send, wait for `Working`, then `headless.py call <run> interrupt session:=N`. `session-N.log` gets `TurnEnded { outcome: Interrupted }` and `events.log` gets `Idle`, both within 2 s of the call.
- **Model switch, headless.** `headless.py call <run> set_model session:=N model=opus effort=high`, then send a message. The fake's log has a `set_model` stdin line, and a scenario that sends `{"type": "text_delta", "text": "{{model}} {{effort}}"}` shows `opus high` in `session-N.log`.
- **Mode switch, headless.** The scenario must add `record` steps so the conversation can be read back (see `fixtures/fake-agents/README.md`, Record). After a turn ends, run `headless.py call <run> set_mode session:=N mode=terminal`, then `mode=chat`, three times. `session-N.log` lists every message again after each switch back, in order.
- **Resume, headless.** With the session `Idle` or `Done`, `headless.py restart <run>`, then `headless.py call <run> sessions worktree=<run>/home/repo` lists session `N` running. `headless.py call <run> follow session:=N` and send a message. The fake was started with `--resume` (its log's `argv`), and `session-N.log` starts with the earlier messages.
- **Two panels, headless.** Spawn two Claude sessions in `<run>/home/repo`, send each a different text, and check each `session-<id>.log` holds only its own `User` and replies.
- **Proof.** App: `screencapture -x -o -l <window_id> <run>/evidence/chat-<step>.png` after each card and each answer. Headless: the `call` replies, the matching lines in `events.log` and `session-N.log`, and the fake's log.

## Gotchas

- Session ids count from 0 per run, and log or command panels an agent opens take ids too. Use the id `spawn_agent` returned.
- The default sidecar scenario waits for `never` after its one turn, so a second message gets no reply. Write a scenario per test.
- A scenario is read when the fake starts. Edit `<run>/scenarios/…` before `spawn_agent`, not after.
- `Done` means the turn ended and the user has not focused the session. Headless never focuses, so it stays `Done`, not `Idle`, after a completed turn. Interrupt leaves `Idle`.
- `set_mode` is refused while a session is `Working` or `NeedsInput`, and when a sent message has no turn started yet.
- In the app, `own-config` needs the throwaway home's links to `~/.claude/skills`, `commands` and `CLAUDE.md` from Launch step 3. Missing links make the `/` menu look empty, which is a setup failure, not a grove bug.
- The real agents are slow and cost money. Keep prompts tiny, and run the many-path checks headless.
