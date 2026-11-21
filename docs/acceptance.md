# Acceptance tests

Every milestone from M4 on must pass these, from the milestone a section names. Record results in the PR.

## Terminal correctness
From M4 these pass for shell sessions. Claude Code TUI, Codex TUI and the Claude Code part of Replay pass from M5, when terminal mode ships for agent sessions.

| Test | Pass |
|---|---|
| vim | Splits, visual mode, colors correct. No artifacts after resizing the window 10 times. |
| htop | 60 s of updates, mouse-click sorting works, correct after resize. |
| Claude Code TUI | In terminal mode: multi-line prompt with Shift+Enter, arrow keys in permission prompts, shift+tab mode switching, `/` menu, resize mid-stream, 200-line bracketed paste, Ctrl-C. No garbled lines. |
| Codex TUI | In terminal mode: the same checks as Claude Code where they apply. |
| vttest | Menus 1, 2, and 8 look the same as in a reference terminal. |
| Unicode | Truecolor test script, CJK, and emoji widths render correctly. |
| Dock launch | Build the .app with `npm run bundle` in `app/`, then start grove from the Dock, not a terminal. A new agent session finds and starts `claude`. |
| Replay | Close and reopen a panel for a running vim and a Claude Code session in terminal mode. No junk at the prompt, no stray replies to old terminal queries, and arrow keys still work. |
| Hidden output | Hide a panel while its session streams for 5 minutes. The agent keeps running at full speed, and the panel is correct when shown again. |
| Hidden window | Minimize or cover the window for 5 minutes while sessions stream. The agents keep running at full speed, and every panel is correct when the window is shown again. |

## Chat mode
Own login, Own config, Approval round trip, Question and Interrupt pass for Claude from M4, except Own config's `grove-mcp` check, which passes from M6. Model switch, Mode switch, Resume and Two panels, and every test for Codex, pass from M5.

| Test | Pass |
|---|---|
| Own login | With `claude` and `codex` logged in through their own CLIs and no API key set, a chat-mode session of each starts, and grove shows the account and plan. |
| Own config | A chat-mode Claude session in a worktree follows the repo's CLAUDE.md, lists the user's skills and slash commands in the `/` menu, and can call `grove-mcp`. |
| Approval round trip | A command approval shows as a card and as `NeedsInput` (Approval) within 1 s. Allow once runs it; Deny stops it; the state follows each. |
| Question | Claude's `AskUserQuestion` and Codex's `request_user_input` show as a card and as `NeedsInput` (Question). The chosen answer reaches the agent. For Codex, the test setup turns `request_user_input` on; with it off, Codex asks in plain text and the turn ends `Done`. |
| Interrupt | Interrupting mid-turn stops the agent within 2 s and leaves the session `Idle`. |
| Model switch | Changing the model and effort mid-session applies from the next turn, which the agent's own record confirms. |
| Mode switch | Switch an agent session to terminal mode and back three times. Every message is still there, in order, and the conversation goes on. |
| Resume | Quit grove during an `Idle` chat session, reopen it, and send a message. The agent remembers the conversation. |
| Two panels | Two agent sessions in one worktree, side by side, stream at the same time without mixing messages. |

## Diff correctness
| Test | Pass |
|---|---|
| Parity | On `fixtures/diff-repo` (21 scripted changes: add, delete, rename, binary, CRLF, whitespace-only, 5k-line file, a staged change, an indent heuristic slide), the core's structured diff matches `git diff HEAD` run with user config disabled (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`): per file the status, paths, binary flag, and every hunk's ranges and lines (ADR 0012). Automated in `cargo test`. |
| Untracked | Untracked files match `git diff --no-index /dev/null <file>` in the same structured form. |
| Branch base | `MergeBase(main)` matches `git diff $(git merge-base main HEAD)`, uncommitted changes included. |
| Commit liveness | After an agent runs `git commit` or `git add`, the diff view updates within 500 ms. |
| Liveness | Diff view updates within 500 ms of a file save. |

## Performance (8 sessions)
Setup: 2 real agent sessions in chat mode, 3 fake chat-mode sessions replaying a recorded protocol stream, and 3 terminal panels replaying a recorded agent session (`script` recording piped through `pv -qL 50k`).

| Metric | Pass |
|---|---|
| Keystroke latency | In a focused terminal panel, Typometer p95 within 2x of a reference terminal measured the same way, and grove's own keydown-to-painted-echo p95 at most 30 ms. |
| Memory | Total footprint (Activity Monitor's Memory column) of grove and its WebKit helpers at most 800 MB, with the window shown for the whole run. Less than 10% growth over 30 min. |
| CPU | Idle under 2%. |
| Stalls | No UI freeze longer than 100 ms while all 8 stream. |
| Composer | Typing in a focused chat composer while all 8 stream: keydown to painted character p95 at most 30 ms. |

## Status and review (from M5)
| Test | Pass |
|---|---|
| NeedsInput | A Claude Code permission request shows as NeedsInput within 1 s: via protocol in chat mode, via hook in terminal mode. |
| Leaving NeedsInput | Approve a command that runs for 30 s. The session leaves NeedsInput within 2 s of approving. |
| Codex status | A Codex approval request shows as NeedsInput within 1 s: via protocol in chat mode, and via hook in terminal mode once grove's hooks are approved. |
| Hook relay down | Quit grove while an agent works. The agent keeps working with no hook errors and no added delay. |
| Attention jump | One key focuses the head of the attention queue. |
| Notifications | With grove's window unfocused, an agent session that asks for approval shows a macOS notification naming its worktree. Clicking it brings grove forward with that session's agent panel focused, in another worktree too. With the window focused, none shows. A profile that lists `Done` gets a notification when a turn ends; the default does not. |
| Review batch | 3 comments on 2 files arrive as one message with correct `path:line`: as a card in chat mode, and in the agent's TUI in terminal mode. A snippet containing `ESC[201~` arrives as plain text in both. |
| Restart | Quitting and reopening grove restores worktrees, sessions to relaunch in their mode, each agent session's workspace, and open comments. Two sessions in one place come back with their own layouts. |
| Profile | A new worktree in a repo with `Cargo.toml` suggests the Rust crate profile. Its setup runs, and its layout, agent and model are what the profile says. |

## Agent-shaped workspace (from M6)
| Test | Pass |
|---|---|
| Layout change | An agent opens the Logs panel through `grove-mcp`. Only its own session's workspace changes: another session in the same place keeps its layout. The notice names the agent session; Undo restores the workspace exactly. |
| Profile change | An agent changes its profile's default model through `grove-mcp`. The Profile panel shows the change, and the next new worktree uses it. |
| Subagent panel | An agent spawns a subagent. It opens as an agent panel beside its parent in chat mode, and the user can type into it, for Claude Code and for Codex. |
