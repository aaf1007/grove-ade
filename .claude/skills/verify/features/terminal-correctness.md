# Terminal correctness

Terminal panels run a shell or an agent's own TUI (terminal mode) in a PTY, rendered by xterm.js. Full-screen programs, keys, paste, resize and Unicode must look and behave as in a native terminal, and output keeps flowing while a panel or the window is hidden.

## Sub-features

- `vim` (app only): splits, visual mode and colors correct, no artifacts after 10 window resizes.
- `htop` (app, not mapped yet): 60 s of updates, mouse-click sorting, correct after resize.
- `claude-code-tui` (headless partly; app not mapped yet): terminal mode takes Shift+Enter, arrows in permission prompts, shift+tab, `/`, resize mid-stream, a 200-line bracketed paste, Ctrl-C. Headless reaches the PTY bytes and hook states, not the rendering.
- `codex-tui` (headless partly; app not mapped yet): the same checks for Codex.
- `vttest` (manual): menus 1, 2 and 8 match a reference terminal. "not run (manual)".
- `unicode` (app, not mapped yet): truecolor, CJK and emoji widths render correctly.
- `dock-launch` (app, not mapped yet): grove started from the bundle finds and starts `claude`. The `open` command stands in for the Dock.
- `replay` (headless partly; app not mapped yet): closing and reopening a panel leaves no junk and arrows still work. Headless can reattach with `follow` and read the snapshot bytes, not judge the screen.
- `hidden-output` (headless partly; app not mapped yet): a hidden panel's agent keeps full speed, and the panel is right when shown.
- `hidden-window` (app, not mapped yet): the same with the window minimized or covered.

## How to get to it (user POV)

- Turn on the **Terminal** toggle in the title bar (tooltip "Terminal at the bottom"), or choose **Open shell** at the bottom of the panel area. The terminal is headed **Shell**.
- Choose **Terminal** in an agent panel header's **Chat**/**Terminal** switch to see the agent's own TUI.
- Hide a panel with the button titled "Hide this panel. The agent session keeps running."

## Driving it with app_* and headless.py

Preconditions:

- App: Doctor passed, the test repo added. Real `claude` and `codex` logged in for the TUI tests. `vim` and `htop` on the login-shell PATH.
- Headless: the baseline run, default `claude.json` and `codex.json` scenarios.

- **Shell, app.** `app_click` **Open shell**, `app_click` inside the terminal, then `app_type` with `target: "focused"`: `vim README.md` and `app_key` `return`. `app_screenshot` shows vim with the file. Type `:vsplit`, `app_key` `return`, `app_screenshot`: two splits.
- **Resize, app.** There is no resize tool for the background app. Use `app_menu` window zoom items if the menu has them, else report "not run" with the reason.
- **Claude Code TUI, headless.** `headless.py call <run> spawn_agent worktree=<run>/home/repo agent=claude mode=terminal`. `session-N.log` gets the idle prompt's bytes. `headless.py call <run> write session:=N text=$'\r'`, then `wait <run> events.log 'SessionId\(N\).*NeedsInput'` with source `Hook`. `write … text=1`, then `wait … 'SessionId\(N\).*Done'`. `fake-claude.jsonl` shows every hook ran with `code` 0.
- **Bracketed paste, headless.** Write a scenario with `{"mode": "bracketed_paste", "on": true}` and `{"wait_input": "\u001b[201~"}`. Write `"\u001b[200~"` + 200 lines + `"\u001b[201~"` with `write`. The fake's `stdin` lines hold all 200 lines between the markers.
- **Codex TUI, headless.** The same with `agent=codex`, Enter approves. Every hook in `fake-codex.jsonl` has `code` 0.
- **Hidden output, headless.** Headless has no panel to hide. The headless runner acks every frame, so it proves only that output keeps flowing with a reader attached. Report the hidden part "not run" headless.
- **Proof.** App: a `screencapture` per check. Headless: `session-N.log` bytes, the state lines in `events.log`, and the fake's log.

## Gotchas

- Terminal panels are a canvas with no accessible text. Click the panel, then type with `target: "focused"`. Read results from screenshots.
- Ctrl-C goes as a byte, `write … text=$'\x03'`, and the default `claude.json` waits for two of them.
- `htop` sorting needs mouse clicks on the header row by coordinate. Take a fresh screenshot first.
- Typometer and vttest comparisons need a reference terminal side by side and full-screen control. Mark them "not run (manual)".
- Hidden window and 60 s or 5 min runs are long. Note the start and end times you observed.
