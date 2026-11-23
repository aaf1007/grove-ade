---
name: verify
description: Drive grove, the macOS desktop app for parallel AI coding agents, and prove a feature works. Two surfaces. App: the real .app bundle with a throwaway HOME, driven by the background app_* computer-use tools, one at a time. Headless: grove-core with the fake agents in fixtures/fake-agents, driven by headless.py and the grovecode CLI, many runs side by side. Use it to verify a change, run the acceptance tests in docs/acceptance.md, or try to break a feature. The qa skill and swarm workers build on it.
---

# verify

grove is a Tauri app over a Rust core (`crates/grove-core`). It runs AI coding agents (Claude Code, Codex) in git worktrees, in chat mode (driven over a protocol) or terminal mode (the agent's TUI in a PTY). Read `GLOSSARY.md` for the terms.

This skill launches grove, checks it is worth driving, drives one feature as a user would, keeps the evidence, and cleans up. Each feature has a recipe in [features/](features/README.md). Read the feature file before you drive it.

## Pick a surface

| | App | Headless |
|---|---|---|
| What runs | the built `grove.app` from `npm run bundle` | grove-core in `crates/grove-core/examples/headless.rs`, no window |
| Agents | real `claude` and `codex`, logged in through their own CLIs | the fakes in `fixtures/fake-agents`, playing a scenario |
| Driven by | background `app_*` computer-use tools | `.claude/skills/verify/headless.py` and `grovecode` |
| Proves | what the user sees and clicks | core behavior: states, messages, diffs, sockets, hooks, `grove-mcp` |
| Side by side | never. Serial only: two copies share the bundle id `com.grovecode.desktop` | yes. Each run has its own HOME and sockets |

Use the app for anything a user sees: rendering, focus, keys, panels, notices. Use headless to try many paths fast, to break things, and for anything the fakes can script. A feature file says which surface reaches each sub-feature. A sub-feature one surface cannot reach is reported "not run" for that surface, never "pass" through the other.

## Rules for both surfaces

- **Never touch the owner's real `~/.grovecode`.** Every run gets `HOME=/tmp/grove-verify.XXXXXX/home`. grove keeps all its state in `$HOME/.grovecode`. The folder lives in `/tmp` because macOS caps a socket path at 104 bytes.
- **Never stop a grove you did not start.** Stop only the pid your run noted, by pid. Never `pkill`, `killall`, or kill by name.
- **App surface: if a grove app is already running, stop and ask the owner to quit it.** It may be theirs. Headless runs never need that: they use no bundle id and no shared socket.
- **Never change code** while verifying. Report what you saw.
- **Never type credentials.** Approve only harmless commands inside the test repo, and deny everything else. Never change anything outside the run's folder.
- **Never add the owner's real repos.** Use the test repo in the run's home.
- A run's folder holds `home/` (grove's state and the test repo) and `evidence/` (the proof). Cleanup stops processes and leaves both, so the owner can look.

## App surface

### Launch

1. Build from the worktree under test. For `main`, use a detached worktree in your scratchpad: `git worktree add --detach <scratchpad>/main origin/main`. Never use `tauri dev` (computer use cannot capture its window) or `/Applications/grove.app` (an old build with the same bundle id).

   ```bash
   cd <worktree>/app && PATH="$HOME/.bun/bin:$PATH" npm run bundle
   ```

   The app lands at `<worktree>/target/release/bundle/macos/grove.app`. A first build takes several minutes. Run it in the background.

2. Make the run folder. Each Bash call starts a new shell, so write the printed path into every later command in place of `<run>`.

   ```bash
   run=$(mktemp -d /tmp/grove-verify.XXXXXX) && mkdir -p "$run/home/.claude" "$run/home/.codex" "$run/evidence" && echo "$run"
   ```

3. Give the throwaway home what real agents need, and nothing that they write back:

   ```bash
   home=<run>/home
   # Claude: a copy of its config file, and links to only what the Own config test needs.
   [ -e "$HOME/.claude.json" ] && cp "$HOME/.claude.json" "$home/"
   for name in settings.json CLAUDE.md skills agents commands plugins; do
     [ -e "$HOME/.claude/$name" ] && ln -s "$HOME/.claude/$name" "$home/.claude/$name"
   done
   # Codex: its login and config. Its sessions stay in the throwaway home.
   for name in auth.json config.toml; do
     [ -e "$HOME/.codex/$name" ] && ln -s "$HOME/.codex/$name" "$home/.codex/$name"
   done
   # Git identity and the login-shell PATH.
   for name in .gitconfig .zshrc .zprofile; do
     [ -e "$HOME/$name" ] && ln -s "$HOME/$name" "$home/$name"
   done
   # The test repo.
   git init -q "$home/repo" && cd "$home/repo" && echo verify > README.md && git add . && git -c user.name=verify -c user.email=verify@example.com commit -qm start
   ```

   For diff features, add `fixtures/diff-repo` too: `python3 fixtures/diff-repo/build.py <run>/home/diff` prints the repo path, `<run>/home/diff/repo` on branch `feature`.

4. Check nothing else runs, then launch:

   ```bash
   pgrep -lx grove
   ```

   Any output means a grove is running. Stop and ask the owner to quit it. Do not kill it.

   ```bash
   open -n --env HOME="<run>/home" <worktree>/target/release/bundle/macos/grove.app
   ```

   Every relaunch, as in Restart or Resume, uses this same `open -n --env HOME=…` command. Never a plain `open`, a Dock click or Finder: grove would start on the owner's real home.

5. Note the pid of the process you started. It is the only grove you may stop. Do it again after every relaunch.

   ```bash
   bin="<worktree>/target/release/bundle/macos/grove.app/Contents/MacOS/grove"
   for p in $(pgrep -x grove); do [ "$(ps -o command= -p "$p")" = "$bin" ] && echo "$p"; done | tee <run>/evidence/app.pid
   [ "$(wc -l < <run>/evidence/app.pid)" -eq 1 ] || echo "STOP: expected one grove, got: $(cat <run>/evidence/app.pid)"
   ps -o lstart= -p "$(cat <run>/evidence/app.pid)" > <run>/evidence/app.started
   ps -o command= -p "$(cat <run>/evidence/app.pid)" > <run>/evidence/app.command
   ```

   Match the exact command, not a prefix: grove starts helper processes from the same binary, also named `grove`, whose command starts with the same path plus arguments. A prefix match (`pgrep -f`) gets both and breaks every `ps -p` after it. The start time and command tell this grove apart from a later process that reuses its pid.

Ready means: `request_access` for the app by its name, `grove`, is granted (the owner approves once per session), and `app_screenshot` shows the window with the **Repo path** field and **Add repo**, and the title bar reads "No worktree selected" on a fresh home.

- Request access by the name `grove`, never by the bundle id. On 2026-11-17 a request for `com.grovecode.desktop` was denied at once with no dialog shown to the owner, three times; the same request by name showed the dialog.
- The window can open on another Space. If `element_index` clicks do nothing, call `app_bring_to_current_space` for `grove` first.

### Doctor

Run when you start and whenever anything looks off. All must hold:

```bash
ps -o command= -p "$(cat <run>/evidence/app.pid)"    # exactly the bundle's grove binary under <worktree>, no arguments
ls <run>/home/.grovecode/grove.sock                  # the app's socket is in the throwaway home
```

Then `app_screenshot` of `grove`. The window must be this build: check a change the branch makes, or the title bar. On the first run with real agents, start a Claude session in the test repo and check the composer reports the account. If it does not, the throwaway home is broken. Stop and report it. Never fall back to the real home.

### Drive

- Only the background tools: `app_screenshot`, `app_click`, `app_type`, `app_key`, `app_menu`, `app_ax_find`, `app_batch`. Never take full-screen control.
- Prefer accessible names over coordinates. Find them with `app_ax_find`. The names the window exposes, as of this writing: in the title bar the toggles **Sidebar**, **Terminal** and **Panel area**, and the buttons **Appearance**, **Details** and the attention button (named "Nothing waiting ⌘J" when the queue is empty); in the sidebar the **Repo path** field, **Add repo**, **New worktree in repo**, one button per worktree such as **main (main worktree)**, **Actions for main**, and **Start Claude Code**; in an empty chat area **Start session**; in the panel area the tab radio buttons **Files** and **Diff** with **Close Files** and **Close Diff**, the **Diff base** choice (**HEAD**, **Merge base**), **Send review batch (N)**, and **Open shell** at the bottom. In a session: the **Message** composer (placeholder "Ask Claude Code anything") with **Send** and **Interrupt**, and the **Chat**/**Terminal** switch in the agent panel's header. Quit is `app_menu` path `["grove", "Quit grove"]`.
- Text fields are inside the web view, so `app_type` with an `element_index` is refused. `app_click` the field by coordinate from the latest `app_screenshot`, then `app_type` with `target: "focused"`, then check with a screenshot.
- Some buttons, like a file chip in the Diff panel, have no accessible name. Click those by coordinate from a fresh screenshot.
- Terminal panels are a canvas. Click the panel first, then type with `target: "focused"`.
- After each step that should change the screen, take an `app_screenshot` and check the change.
- "Dock launch": the `open` command stands in for a Dock click. Record it as an approximation.
- Real agents: keep prompts tiny, like "reply with the word ok" or "create an empty file called qa.txt".

### Evidence

- After each step that proves something, save the window to a file. Get the window id from `app_list_windows` for `grove`, then:

  ```bash
  screencapture -x -o -l <window_id> <run>/evidence/<feature>-<step>.png
  ```

  If `screencapture` fails for lack of Screen Recording permission, say so in the report and describe each `app_screenshot` you took instead.
- Prove side effects, not just the screen: files in `<run>/home/repo` (`git -C <run>/home/repo status --short`), and grove's own state in `<run>/home/.grovecode/`.
- Send the important images to the owner with `SendUserFile`.

### Cleanup

`app-cleanup.sh` stops what this run's grove started and nothing else. It never kills by name or path match.

1. Save the app's process tree while every process in it is still the app's: its agent sessions, their children and their background jobs, each with its start time.

   ```bash
   .claude/skills/verify/app-cleanup.sh snapshot <run>
   ```

   Take it again after starting sessions and right before Quit. Each snapshot adds to the saved list and keeps earlier entries, so a process saved once stays covered after it leaves grove's tree.
2. Quit through grove's own Quit: `app_menu` path `["grove", "Quit grove"]`. That runs grove's exit path, which stops its agent sessions. A signal skips it and can leave agents running.

   On 2026-11-17, on a loaded Mac (load about 25), the standard quit Apple event (`osascript -e 'tell application id "com.grovecode.desktop" to quit'`) did not make grove exit within 10 s, so `finish` force-quit it. Later that day, with a live Claude session working, the Quit menu item ended grove in about 1 s. Use the menu item. Report a forced quit when `finish` prints `did not quit`.
3. Wait up to 10 s for the pid to go (`ps -p "$(cat <run>/evidence/app.pid)"` stops listing it), then:

   ```bash
   .claude/skills/verify/app-cleanup.sh finish <run>
   ```

   It force-quits the app only if it is still this run's (same pid, start time and command), then stops saved processes that are still the same process: SIGTERM, then SIGKILL after 5 s, then up to 5 s for each to leave the process table (a dead process waiting to be reaped counts as gone). It prints `attributable cleanup done` and exits 0 when every saved process is gone and nothing unattributed names or works in the run, even after a forced quit. Report that as "attributable processes gone; full coverage not verifiable", never as "all clean". Otherwise it prints `cleanup incomplete` and exits 1. Report these lines: `did not quit` (a forced quit), `outlived the app` (Quit left processes behind), `STILL RUNNING`, and `NOT ATTRIBUTED`. `NOT ATTRIBUTED` is a process outside the saved list whose command names `<run>/home` or whose working folder is under it, such as an agent's background job that left the tree before any snapshot. It is reported, never killed: a path match does not prove the run owns it. It cannot see a process that left grove's tree before every snapshot and neither names nor works in `<run>/home`, and says so.

4. Check it is gone: `pgrep -lx grove` prints nothing.
5. `app_release` to drop the window hold.
6. Leave `<run>` in place. The evidence stays in `<run>/evidence/`.

## Headless surface

`headless.py` is a standard-library Python script. Run it from the repo root as `.claude/skills/verify/headless.py`. It needs `cargo` and `python3` (3.9 or later).

### Launch

```bash
.claude/skills/verify/headless.py start
```

It builds `grove-core`'s `headless` example, `grove-mcp`, `grovecode` and `grove-hook` into `target/debug/`, makes `/tmp/grove-verify.XXXXXX` with `home/`, `bin/` (wrappers that point grove at the fakes), `scenarios/` (copied from `.claude/skills/verify/scenarios/`) and `evidence/`, makes the test repo `home/repo` with one commit, starts the core with `HOME=<run>/home`, adds the repo, and prints the run folder. Ready means it printed the folder. It fails with the reason otherwise.

To change what a fake agent does, edit `<run>/scenarios/<agent>.json` before you spawn the session. The format is in `fixtures/fake-agents/README.md`. The defaults:

| File | Fake | Plays |
|---|---|---|
| `claude-sidecar.json` | Claude in chat mode | account `verify@example.com`, then on the first message: text "ok", an approval for `touch verified.txt`, the turn ends after the answer |
| `claude.json` | Claude in terminal mode | idle prompt, then on Enter: an approved Bash call with every hook, Stop on `1` |
| `codex.json` | Codex in terminal mode | the same with Codex hooks, approval on Enter |
| `codex-app-server.json` | Codex in chat mode | on the first turn: an approval for `touch verified.txt`, the turn ends after the answer |
| `codex-account.json` | Codex in chat mode | the account `account/read` returns |

### Doctor

```bash
.claude/skills/verify/headless.py doctor <run>
```

Prints `OK …` and exits 0 when the core this run started is alive, its HOME is the run's and not the owner's, `grove.sock` is under `<run>/home/.grovecode/`, the test repo is tracked, and the binaries exist. Otherwise prints each `FAIL` and exits 1.

### Drive

One request at a time to the run's core, with `key=text` for strings and `key:=json` for JSON:

```bash
.claude/skills/verify/headless.py call <run> spawn_agent worktree=<run>/home/repo agent=claude mode=chat
.claude/skills/verify/headless.py call <run> send session:=0 text="reply with the word ok"
.claude/skills/verify/headless.py wait <run> session-0.log 'User'     # the log exists before anyone counts its lines
.claude/skills/verify/headless.py wait <run> events.log 'NeedsInput'
.claude/skills/verify/headless.py settle <run> session-0.log
L=$(wc -l < <run>/evidence/session-0.log)
.claude/skills/verify/headless.py call <run> answer session:=0 request=toolu_verify reply=allow_once
.claude/skills/verify/headless.py wait <run> session-0.log 'TurnEnded' --after "$L"
```

`call` prints `{"ok": …}`, or `{"error": …}` and exits 1. Session ids count from 0 in each run, and panels an agent opens take ids too. Read the id from the `spawn_agent` reply. The commands:

| Command | Arguments | Does |
|---|---|---|
| `ping` | | the core's HOME and pid |
| `add_repo` | `path` | tracks a repo |
| `forget_repo` | `path` | ends the sessions of a repo and stops tracking it, with the disk left alone |
| `worktrees` | | the tracked worktrees |
| `create_worktree` | `repo`, `branch` | a new worktree on a new branch, prints its path |
| `spawn_agent` | `worktree`, `mode=chat\|terminal`, optional `agent=claude\|codex` (without it, the first agent the profile's layout names), optional `prompt` | starts an agent session on the fake, with a workspace of its own, prints its id |
| `spawn_shell` | `worktree` | a shell session |
| `follow` | `session:=N` | logs a session the core restored after `restart` to `session-<id>.log` |
| `send` | `session:=N`, `text` | a chat-mode message |
| `answer` | `session:=N`, `request`, `reply=allow_once\|allow_session\|deny` or `reply:='[["label"]]'` | answers an approval or a question |
| `interrupt` | `session:=N` | interrupts the turn |
| `ask_side_question` | `session:=N`, `question` | asks a chat-mode session a side question, prints its id, and logs the answer to `side-question-<session>-<id>.log`, with `closed` once the core drops it |
| `end_side_question` | `session:=N`, `side_question:=N` | ends that side question and its fork |
| `set_model` | `session:=N`, `model`, optional `effort` | changes model and effort |
| `set_mode` | `session:=N`, `mode` | switches chat and terminal mode |
| `write` | `session:=N`, `text` | bytes to a PTY, such as `text=$'\r'` for Enter |
| `kill` | `session:=N` | ends a session |
| `diff` | `worktree`, `base=head\|merge_base` | the structured diff, lines as `+`, `-` or space then text |
| `workspace` | `session:=N` | the workspace the agent session shows in: its own, or its top parent's for a subagent |
| `starting_workspace` | `worktree` | the layout a new agent session in the worktree starts from |
| `undo_workspace` | `session:=N` | undoes the last change an agent made to that workspace |
| `add_comment` | `worktree`, `path` (relative), `start_line:=N`, `end_line:=N`, `body` | a review comment |
| `comments` | `worktree` | the open review comments and their state |
| `send_review_batch` | `worktree`, optional `session:=N` | sends the open comments as one message |
| `profiles` | | every profile |
| `suggest_profile` | `repo` | the profile grove suggests for the repo |
| `use_profile` | `repo`, `name` | makes the repo use that profile, as accepting a suggestion does |
| `attention` | | the attention queue |
| `sessions` | `worktree` | the sessions grove would restore |
| `quit` | | stops the core |

Wait for a line in an evidence file instead of sleeping. It prints the first match, or exits 1 after the timeout (10 s by default). It reads the file from the top, so a line an earlier step wrote would match at once. The first example above shows the pattern: before an action, `settle` the file (no new line for 0.5 s, past the core's 150 ms watcher debounce), note its line count, and pass it as `--after`, so only lines written after the count are read. That is not the same as lines the action caused.

`settle` makes a late line from an earlier step unlikely, not impossible: the core writes its logs on its own threads. Where you can, make the pattern name something only this action produces, such as the request id you answered or the text you sent, or use a scenario whose order rules out an earlier match. A session id tells sessions apart, not turns: two turns of one session share it. Without such a marker, report a timing or ordering result as "observed", not proven.

Act as an agent through its shell. This runs `target/debug/grovecode` with the `GROVE_SESSION_ID`, `GROVE_SESSION_TOKEN` and `GROVE_SOCKET` grove gave that session, read from the fake's own log:

```bash
.claude/skills/verify/headless.py grovecode <run> 0 workspace get
.claude/skills/verify/headless.py grovecode <run> 0 panel open log tests "echo verify-ok"
```

An agent can also reach `grove-mcp` itself: put an `mcp_call` step in the fake `claude` or `codex` scenario.

Quit and reopen grove on the same HOME, as the Restart and Resume tests do. Sessions the core restores keep their ids. Follow each again before you drive it:

```bash
.claude/skills/verify/headless.py restart <run>
.claude/skills/verify/headless.py call <run> sessions worktree=<run>/home/repo
.claude/skills/verify/headless.py call <run> follow session:=0
```

### Evidence

Everything lands in `<run>/evidence/` and stays after cleanup:

| File | Holds |
|---|---|
| `events.log` | every `CoreEvent`, with seconds since start: agent states and their source, diffs changed, workspace and profile changes, panels, accounts |
| `session-<id>.log` | a chat session's messages, or a PTY session's raw output |
| `side-question-<session>-<id>.log` | a side question's answer, which never reaches `session-<id>.log` |
| `fake-<agent>.jsonl` | what the fake agent got: argv, env, every stdin line, hook runs and their exit codes |
| `grovecode.log` | each `grovecode` call, its exit code and output |
| `driver.out` | the core's stderr |

Proof is the action and the result: the `call` you made, the event or message it caused, and the side effect the agent saw in its fake log or the file in `<run>/home/repo`. Timing claims come from the timestamps in `events.log` and `session-<id>.log`.

### Cleanup

```bash
.claude/skills/verify/headless.py stop <run>
```

It asks the core to quit, which ends every session it started, and kills the pid in `<run>/driver.pid` only if it is still this run's core. It never touches another process. `<run>/home` and `<run>/evidence` stay.

## Report

Per feature, one line per sub-feature: surface, what you did, pass / fail / blocked / not run, and the evidence path. A sub-feature not run says why. Screenshots cannot measure milliseconds: a criterion like "within 1 s" is "observed, not measured" unless timestamps in an evidence file timed it.

## Keeping the map true

The feature map drifts as grove changes. Run `/maintain-verification-skill` to bring it back in line with the app.
