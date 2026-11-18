# Architecture

## Overview
```
+------------------------- Tauri app --------------------------+
|  React UI: sidebar | chat area | panel area (files, diff,    |
|            terminal, browser, logs) | xterm.js | Monaco       |
|        ^ ipc::Channel (acked stream per session, events)     |
|        | invoke (commands)                                   |
|  src-tauri: thin bridge, no business logic                   |
+--------|-----------------------------------------------------+
         v
+--------------------- grove-core (tokio) ---------------------+
|  worktree   |  sessions          |  diff      |  status       |
|  git CLI    |  chat: protocol    |  git2 +    |  protocol;    |
|             |  terminal: PTY,    |  notify    |  hooks >      |
|             |  alacritty_term    |            |  heuristic    |
|  review comments | profiles | adapters (claude, codex) | events |
+-----|---------------------|-------------------^---------------+
      | stdio JSON lines    | stdio JSON-RPC    | Unix socket ~/.grovecode/grove.sock
 Claude sidecar        codex app-server     +---+-----------------+
 (Node, Agent SDK)                      grove-hook          grove-mcp / grovecode CLI
      |                                 (hooks)             (panels, subagents, layout)
 user's `claude`
```
- Chat mode (ADR 0013): the core drives Claude through the Claude sidecar and Codex through `codex app-server` (ADR 0015). Terminal mode runs the agent's TUI in a PTY, as shell and run sessions do.

## Crates
| Job | Crate | Notes |
|---|---|---|
| Runtime | `tokio` | Core owns its runtime. |
| PTY | `portable-pty` | Shell, run and command sessions, and agent sessions in terminal mode. One std reader thread and one std writer thread per session. See Output pipeline. |
| Claude transport | Claude sidecar: Node on `@anthropic-ai/claude-agent-sdk` | Chat mode for Claude. JSON lines over stdio. Runtime bundling picked during M4 (ADR 0015). |
| Codex transport | `codex app-server` | Chat mode for Codex. JSON-RPC over stdio, spoken from Rust. Types from `codex app-server generate-json-schema` for the pinned Codex version. |
| Profiles | a TOML crate (pick with the first profile issue) | `~/.grovecode/profiles/*.toml` (ADR 0016). |
| VT state | `alacritty_terminal` | Headless. Parses every byte of every PTY session: status heuristics, snapshots, and the only reply to terminal queries. xterm.js renders. With 8 sessions at 50 KB/s each it costs about 1% of one core more than reading alone, and nothing while idle (spike #7). |
| Worktrees | git CLI via `std::process` in `spawn_blocking` | Respects user hooks and config. New worktrees go in `~/.grovecode/worktrees/<repo>/<name>`. Existing ones from `git worktree list` can be adopted. |
| Diff | `git2` | Indent heuristic on; user `diff.*` config ignored. Runs in `spawn_blocking` with a `Repository` opened per call. Parity contract in ADR 0012. |
| Watching | `notify` + `notify-debouncer-full` + `ignore` | Debounce about 150 ms. Watches the worktree's files (skips `.git/`, `node_modules`, `target`) and its git dir (`HEAD`, `index`) plus the common dir's `refs/` and `packed-refs`, so commits and staging trigger `DiffChanged`. A worktree whose watcher cannot start (at start, on add, or after a failed Delete) stays tracked without live updates, and the core sends one `WatcherNotStarted` warning with the cause. `retry_watcher` starts it again. |
| IPC | `tokio::net::UnixListener`, `serde`, `serde_json` | Hook relay and MCP server both talk to this socket. |
| Errors | `thiserror` (core), `anyhow` (bins) | |
| Logging | `tracing` | |
| MCP | `rmcp` | stdio transport. |

## Core API (target shape)
```rust
impl Core {
    pub fn start(cfg: CoreConfig) -> Result<(Core, broadcast::Receiver<CoreEvent>)>;

    // worktrees
    pub async fn add_repo(&self, path: &Path) -> Result<()>; // adopts every worktree in `git worktree list`
    pub async fn create_worktree(&self, repo: &Path, req: NewWorktree) -> Result<AddedWorktree>; // the worktree and its setup's run session
    pub fn setup(&self, wt: &Path) -> Option<Setup>; // its setup's run session and Running, Succeeded or Failed
    pub async fn list_worktrees(&self) -> Vec<WorktreeInfo>;
    pub async fn remove_worktree(&self, id: WorktreeId, opts: RemoveOpts) -> Result<()>;
    pub async fn delete_check(&self, wt: &Path) -> Result<DeleteCheck>; // what Delete would lose, and whether the branch is merged
    pub async fn retry_watcher(&self, wt: &Path) -> Result<()>; // after WatcherNotStarted; does nothing if it has a watcher, no second warning on failure

    // sessions
    pub fn spawn(&self, wt: WorktreeId, spec: SpawnSpec) -> Result<SessionId>;
    pub fn spawn_shell(&self, wt: WorktreeId, cols: u16, rows: u16) -> Result<SessionId>; // the worktree's shell: the running one, or a new login shell
    pub fn sessions(&self, wt: WorktreeId) -> Result<Vec<SessionInfo>>; // agent, shell and run sessions, with any run session a restart brought back stopped
    pub fn write(&self, s: SessionId, bytes: &[u8]) -> Result<()>; // queued to the writer thread
    pub fn resize(&self, s: SessionId, cols: u16, rows: u16) -> Result<()>;
    pub fn bracketed_paste(&self, s: SessionId) -> Result<bool>; // NoTerminal in chat mode
    pub fn kill(&self, s: SessionId) -> Result<()>; // whole process group; state becomes Exited
    pub fn attach(&self, s: SessionId, sink: OutputSink) -> Result<Snapshot>;
    pub fn detach(&self, s: SessionId);
    pub fn ack(&self, s: SessionId, bytes: usize);
    pub fn focus(&self, s: SessionId); // Done -> Idle; remembers the last focused agent session
    pub fn spawn_run(&self, wt: &Path, spec: RunSpec) -> Result<SessionId>; // a saved run session, by name
    pub fn port(&self, s: SessionId) -> Option<u16>; // a running run session's declared port

    // agent sessions in chat mode
    pub fn spawn_agent(&self, wt: WorktreeId, spec: AgentSpec) -> Result<SessionId>; // agent, model, effort, mode, panel; puts it in agent panel spec.panel (counted as workspace returns them; a new shown panel after the last when None or taken), saves the workspace, sends WorkspaceChanged with by None
    pub async fn spawn_agent_after_setup(&self, wt: &Path, spec: AgentSpec) -> Result<SessionId>; // waits for setup; SetupFailed if it failed
    pub fn subscribe(&self, s: SessionId, sink: MessageSink) -> Result<Vec<AgentMessage>>; // history, then live
    pub fn send_message(&self, s: SessionId, text: &str) -> Result<()>;
    pub fn answer(&self, s: SessionId, request: RequestId, reply: Reply) -> Result<()>; // approval or question
    pub fn interrupt(&self, s: SessionId) -> Result<()>;
    pub fn set_model(&self, s: SessionId, model: &str, effort: Option<&str>) -> Result<()>; // from the next turn, saved with the session
    pub fn model_choice(&self, s: SessionId) -> Result<Option<ModelChoice>>; // what set_model saved; every launch in either mode applies it
    pub async fn models(&self, s: SessionId) -> Result<Vec<ModelInfo>>; // the agent's own list, with each model's efforts
    pub async fn set_mode(&self, s: SessionId, mode: Mode) -> Result<()>; // ends one process, resumes the conversation in the other

    // files
    pub async fn list_files(&self, wt: WorktreeId) -> Result<Vec<FileEntry>>; // respects .gitignore
    pub async fn read_file(&self, wt: WorktreeId, path: &Path) -> Result<Vec<u8>>;
    pub async fn write_file(&self, wt: WorktreeId, path: &Path, bytes: &[u8]) -> Result<()>;
    pub async fn reveal_in_finder(&self, wt: &Path) -> Result<()>; // a tracked worktree only, selected in its parent folder
    pub async fn open_link_in_default_browser(&self, link: &str) -> Result<()>; // http and https links only, in the user's default browser

    // profiles and workspaces
    pub fn profiles(&self) -> Result<Vec<Profile>>;
    pub async fn set_profile(&self, p: Profile, by: Option<SessionId>) -> Result<()>; // by: the agent session that changed it
    pub fn repo_settings(&self, repo: &Path) -> Result<RepoSettings>; // the chosen profile and its overrides
    pub async fn set_repo_settings(&self, repo: &Path, s: RepoSettings) -> Result<()>;
    pub fn profile_text(&self, name: &str) -> Result<ProfileText>; // the file and its path, comments included
    pub async fn set_profile_text(&self, name: &str, text: String, by: Option<SessionId>) -> Result<()>; // kept as typed; refuses a rename
    pub fn repo_overrides_text(&self, repo: &Path) -> Result<String>;
    pub async fn set_repo_overrides_text(&self, repo: &Path, text: &str) -> Result<()>; // keeps the profile; comments are not kept
    pub fn suggest_profile(&self, repo: &Path) -> Option<Suggestion>; // the profile and the marker file found
    pub fn workspace(&self, wt: WorktreeId) -> Workspace; // starts from the profile's layout
    pub fn set_workspace(&self, wt: WorktreeId, w: Workspace, by: Option<SessionId>) -> Result<()>; // by: the agent session that changed it

    // diff + review
    pub async fn diff(&self, wt: WorktreeId, base: DiffBase) -> Result<Vec<FileDiff>>;
    pub async fn file_versions(&self, wt: WorktreeId, path: &Path, base: DiffBase) -> Result<FileVersions>; // bytes
    pub async fn line_changes(&self, wt: WorktreeId, path: &Path) -> Result<Vec<LineChange>>; // lines changed since HEAD, for the editor's gutter
    pub async fn add_comment(&self, wt: &Path, c: NewReviewComment) -> Result<CommentId>; // reads the lines' text
    pub fn comments(&self, wt: &Path) -> Result<Vec<ReviewComment>>; // open and sent, oldest first
    pub fn resolved_comments(&self, wt: &Path) -> Result<Vec<ReviewComment>>;
    pub fn resolve_comment(&self, c: CommentId) -> Result<()>;
    pub fn send_review_batch(&self, wt: &Path, to: Option<SessionId>) -> Result<()>; // None: the most recently focused agent session

    // status
    pub fn agent_state(&self, s: SessionId) -> AgentState;
    pub fn attention_queue(&self) -> Vec<SessionId>;
}

// Small state events only. PTY output and agent messages never ride this bus.
pub enum CoreEvent {
    SessionExited { session: SessionId, code: Option<i32> },
    AgentState { session: SessionId, state: AgentState, source: StateSource },
    DiffChanged { worktree: WorktreeId, paths: Vec<PathBuf> },
    CommentsChanged { worktree: PathBuf },
    PanelRequested { parent: Option<SessionId>, panel: PanelSpec },
    PanelClosed { panel: SessionId }, // its agent session closed it, or its worktree was removed
    WorkspaceChanged { worktree: WorktreeId, by: Option<SessionId> }, // by Some: show the notice with Undo; None for the user, an undo, or spawn_agent placing a session
    ProfilesChanged { by: Option<SessionId> }, // by Some: show the notice with Undo
    RepoSettingsChanged { repo: PathBuf, by: Option<SessionId> }, // a repo's profile pick or overrides changed
    Setup { worktree: PathBuf, setup: Setup }, // a worktree's setup started or ended
    Account { agent: AgentKind, account: Option<AccountInfo> }, // plan or API key as each agent reports it
}
```
- `AgentMessage` covers user and agent text (streamed as deltas), turn starts and ends (an end carries its error, if any), tool calls and results, file changes, approval and question requests, review batches, messages that never reached the agent, notices and errors. Each subscribed agent panel gets its own `MessageSink`, batched like PTY output: at most one frame per 8 ms per session.
- Run sessions start through `spawn_run`. The core records a run session's port, read with `port`, for the future port router.
- `OutputSink` is a core type (a sender of `OutputFrame { seq, bytes }`), so the core never depends on Tauri (ADR 0002). src-tauri forwards each frame to that session's `ipc::Channel` as raw bytes.

## Output pipeline
This pipeline carries every PTY session: shell, run and command sessions, and agent sessions in terminal mode.
1. The reader thread feeds every chunk to the headless parser first, then to the session's output buffer, and numbers it. The parser never gets half a UTF-8 character. The last 1 to 3 bytes of a split one wait for the next chunk, because vte 0.15.0 drops a byte when it joins a character split across two calls. The parser applies synchronized updates (mode 2026) at once, since it never draws.
2. While a panel is attached, a batcher sends its output to that session's own `ipc::Channel`. Output that arrives 8 ms or more after the last frame goes out at once. Output that arrives sooner waits until 8 ms after that frame and goes out with everything read meanwhile. An idle session sends nothing and wakes nothing. Bytes stay bytes all the way to `term.write(Uint8Array)`; nothing decodes them per batch.
3. The UI calls `ack` after xterm.js has written a frame. When unacked bytes pass a high watermark, the reader stops reading, so the agent blocks on its own writes as in a real terminal. It resumes below a low watermark. Values: 128 KiB and 32 KiB. With 8 panels under `yes`, 512 and 128 KiB made Ctrl-C take 278 ms to show and froze frames for up to 274 ms. 128 and 32 KiB took 87 ms and 79 ms, for 4% less throughput.
4. A hidden or closed panel detaches. So does every panel while the page is hidden (`document.visibilityState`): the window hidden, minimized or covered, or the screen locked. They reattach when it is visible again. The core keeps reading at full speed and feeds only the headless parser, so background agents never wait on the UI. A hidden page stops animation frames and runs timers about once a second, so a panel left attached acks late: it held `yes` to 8 MB/s, against 156 MB/s detached.
5. `attach` takes the snapshot and subscribes under one lock: the snapshot covers output up to sequence N, and live frames start at N+1. Spike #7 checked this with 200 attaches under steady output and found no byte lost or repeated.
6. The UI debounces resizes and never sends a zero size (a hidden panel measures 0x0). The core resizes the headless parser and the PTY in one step.
7. Writes go through the writer thread, so a TUI that is not reading never blocks a tokio worker.
8. Spike #6 chose the flush rule in step 2. With 7 other panels replaying agent output, keystroke to painted echo had a p95 of 23 ms on a fixed 8 ms tick, 18 ms when every read went out at once, and 17 ms with the rule. Sending every read at once also sent 5,400 frames a second at 1 MB/s per panel and more than doubled CPU. One channel per session costs more than one shared channel only when many sessions send small frames at the same time: 35 to 85% CPU against 14 to 31% at 8 sessions times 125 frames a second. Recorded agent output sent 160 frames a second in all, and the two differed by 4 to 7 points.

## Snapshot and replay
Spike #7 decided this section.
- The headless parser answers terminal queries for every session, attached or not, the way xterm.js would: DA1 `CSI ?1;2c`, DA2 `CSI >0;276;0c`, device status, cursor position, mode reports (DECRQM), colors (OSC 4, 10 and 11, from the UI's theme), and the text area size in cells (`CSI 18t`). `alacritty_terminal` names itself in DA1 and DA2, so the core rewrites those two replies.
- xterm.js registers parser handlers that swallow every query it would answer itself: DA1, DA2, DSR, DECXCPR, DECRQM, DECRQSS, the `CSI t` reports, and OSC 4, 10, 11 and 12 queries. So each query gets exactly one reply. DECXCPR, DECRQSS, XTVERSION and the size in pixels get none.
- The kitty keyboard protocol stays off in the parser, because xterm.js 6.0 lacks it. `CSI ? u` gets no reply, so agents keep the legacy key encoding.
- A snapshot serializes the parser's grid. It holds the size, the screen, the last 1,000 lines of scrollback, the main screen behind the alt screen, the cursor (with a pending wrap, the saved cursor and the current colors), and the modes the TUI turned on: alt screen, cursor keys, keypad, mouse tracking and encoding, focus events, bracketed paste, cursor visibility and style, and keyboard flags (none while the kitty protocol is off). It ends with the bytes of an escape sequence or UTF-8 character the parser has read but not finished, so the first live frame completes it. It holds no queries, so replaying it triggers no replies. Replay restores all of it in the panel's own xterm.js, reset in place, so a reopened vim, Claude Code or Codex session keeps working keys. Before the snapshot the panel writes CAN and RIS, shows the cursor and restores the theme's colors, which RIS in xterm.js 6.0 leaves as they were. A new xterm.js would cost a new WebGL context, and WebKit keeps 16.
- Snapshots do not replay a byte ring. Replaying the last N bytes restores the screen only while the ring still holds everything since the last full repaint, and a long session breaks that. With a 16 KiB ring, vim came back blank after 1,500 cursor moves. A ring also replays old queries.
- A reopened panel shows only those last 1,000 lines of scrollback.
- A snapshot does not restore the scroll region, character sets, tab stops or the title, because `alacritty_terminal` keeps them private. A TUI that keeps a scroll region set can scroll wrongly in a reopened panel until it sets one again.

## Chat mode
ADR 0013 and 0015 decided this section. Details marked "M4" are settled by the M4 vertical slice.
- Claude: the core starts one Claude sidecar per agent session in chat mode. The sidecar runs the Agent SDK with `settingSources` user, project and local, the `claude_code` system prompt preset, the worktree as its folder, and the user's `claude` from the login shell `PATH`. It gets the same per-session `claude-settings.json` and `mcp.json` as terminal mode, so hooks and `grove-mcp` work in both modes. Core and sidecar exchange JSON lines over stdio.
- Codex: the core runs `codex app-server` with the same `-c` overrides as terminal mode and speaks JSON-RPC over stdio: `thread/start` or `thread/resume`, `turn/start` per message, `turn/interrupt`.
- Messages: each adapter maps its transport's events to `AgentMessage`. Text arrives as deltas and renders at most once per frame.
- Approvals and questions:
  - Claude: the SDK's `canUseTool` callback reaches the core as a request. `AskUserQuestion` comes the same way and becomes a question.
  - Codex: `item/commandExecution/requestApproval`, `item/fileChange/requestApproval` and `item/permissions/requestApproval` become approvals. `item/tool/requestUserInput` becomes a question.
  - The UI draws a card. `answer` sends allow once, allow for this session, or deny, or the chosen answer.
- Model and effort: set in the composer's picker; the profile gives the defaults. Claude uses `setModel` and `applyFlagSettings({ effortLevel })`. Codex takes them on the next `turn/start`. The models come from each agent. Effort is a plain string the agent defines, not a fixed list, since Codex has levels such as `ultra`. The agent is fixed after the first message.
- Slash commands: Claude's `supportedCommands()` fills the `/` menu. Anything chat mode cannot do, the user does in terminal mode.
- Messages not delivered: the core keeps each message sent until the agent starts a turn on it. If the process ends first, the conversation gets a `NotDelivered` entry for it, and a review batch's comments go back to `Open`.
- Switching mode: `set_mode` is refused while the session is `Working` or `NeedsInput`, or with `MessagePending` while a sent message has no turn started yet. A send that comes during the switch fails with `SwitchingMode`. It ends the current process and resumes the same conversation in the other mode with the resume id. Chat mode shows the history from the agent's own record: the turns in Codex's `thread/resume` reply, the same ones `thread/read` returns, and for Claude the sidecar's `history`, which it reads with the Agent SDK's `getSessionMessages`. `set_mode` returns once the history is in.
- Account: each transport reports who is logged in and on which plan, or that an API key is in use. grove shows it and never stores credentials.

Protocol events map to agent states (source `Protocol`) the same way for both agents:

| Protocol event | Agent state |
|---|---|
| A turn starts (the user sends a message, or a review batch) | `Working` |
| An approval request is pending | `NeedsInput` (Approval) |
| A question is pending | `NeedsInput` (Question) |
| Answering a request | `Working` |
| The turn ends | `Done`, or `Idle` when its panel has focus |
| The turn ends on an error | `Failed` |
| `interrupt` | `Idle` |
| The process exits cleanly, or grove stopped it | `Exited` |
| The process exits non-zero | `Failed` |

The last two rows come from the core's own wait on the sidecar or app-server process, not from a transport message. They still use source `Protocol`, since they describe a chat-mode session.

## Agent adapters
```rust
trait AgentAdapter {
    // Terminal mode: command plus integration flags. LaunchCtx has the session id, resume id, prompt,
    // and the profile's launch args for that agent, which go after grove's flags and the prompt.
    fn launch(&self, ctx: &LaunchCtx) -> SpawnSpec;
    // Chat mode: the sidecar or app-server to start, with the same integration.
    fn launch_chat(&self, ctx: &LaunchCtx) -> ChatSpec;
    fn parse_event(&self, event: &HookEvent) -> Option<AgentState>;
    fn parse_message(&self, message: &TransportMessage) -> Vec<AgentMessage>;
}
```
- The adapter only translates. In chat mode the core maps the `AgentMessage`s from `parse_message` to agent states with the protocol table in Chat mode and sends each change as `CoreEvent::AgentState` with source `Protocol`. In terminal mode `parse_event` gives the state, and the core sends it with source `Hook`.

| | Claude Code | Codex |
|---|---|---|
| Integration | Per-session files in `~/.grovecode/sessions/<id>/`: `claude-settings.json`, whose hooks run `~/.grovecode/bin/grove-hook --session <id> claude`, and `mcp.json`, which sets `GROVE_SESSION_ID`, `GROVE_SESSION_TOKEN` and `GROVE_SOCKET` in the server's `env`. Passed with `--settings` and `--mcp-config` on every launch, including `--resume`. Both add to the user's own config. | `GROVE_SESSION_ID`, `GROVE_SESSION_TOKEN` and `GROVE_SOCKET` in the environment of the Codex process: hooks get Codex's own environment (`shell_environment_policy` does not apply to them), and `env_vars` reads from it too. `-c` overrides on every launch and resume: `[hooks]` running `~/.grovecode/bin/grove-hook codex`, with the same entries every time (command, timeout, event order), because the one-time approval is stored per entry against a hash of that entry; `mcp_servers.grove` with `env_vars=["GROVE_SESSION_ID","GROVE_SESSION_TOKEN","GROVE_SOCKET"]`; `shell_environment_policy.set.GROVE_SESSION_ID`, so shell commands such as `grovecode` get the id even under a user policy like `inherit = "core"`. Plus `--no-daemon`: on the shared daemon, hooks and MCP servers get the daemon's environment instead of the session's. `notify` is never touched. |
| Resume id | Assigned by grove with `--session-id <uuid>` on first launch; updated by `SessionStart` (for example after `/clear`). | From `SessionStart`, refreshed on every one. In the TUI it fires with the first prompt, not at launch. |

- Nothing is written into worktrees or the user's global config (ADR 0010).
- Beyond these flags, grove adds only the launch args from the repo's profile, or the repo's own when it overrides them. A repo's launch args replace the profile's; they are not added to them. Empty by default.
- Model, effort and permission mode come from the profile when it sets them, otherwise from the user's own agent config. The picker changes model and effort from the next turn.
- A launch arg that would replace one of grove's own flags or overrides is refused:
  - Claude: `--settings`, `--session-id` or `--resume`.
  - Codex: a `-c` key that grove sets or that holds one, such as `hooks`.
  - Codex: a `shell_environment_policy.include_only` list that does not name `GROVE_SESSION_ID`. Codex applies it after `set`.
- Only agent sessions get a `GROVE_SESSION_ID`. Shell and run sessions do not.
- If no resume id was captured, restore starts a fresh conversation.
- The first prompt goes in argv, never typed into the PTY. Codex startup prompts such as "Hooks need review" are modal and eat keystrokes.
- Claude's `--mcp-config` takes several values, so grove passes `--mcp-config=<file>`. Claude ignores an invalid settings file without an error, so grove validates `claude-settings.json` after writing it.
- Codex hook approval (terminal mode only; chat mode answers approvals over app-server, ADR 0014): at launch, beside the agent, grove runs `codex app-server` with the same `-c` flags and calls `hooks/list`. It reports each hook as trusted, untrusted, or modified. If any of grove's hooks is not trusted, the session's agent states have source `Heuristic` until a hook fires. The core also sends a `CodexHooksNotTrusted` warning, so the UI can show how to approve the hooks in `/hooks`. A check that fails or gets no answer within 5 s sends nothing. "Hooks need review" on screen at launch means the same. grove never approves its own hooks, neither with `--dangerously-bypass-hook-trust` nor by passing `hooks.state` through `-c` (ADR 0010).
- Claude folder trust: in a folder Claude does not trust yet, no hook runs and no MCP server starts until the user accepts the trust dialog. grove detects the dialog on the headless screen and shows the session as `NeedsInput` (Approval, source `Heuristic`), not as hooks down. A linked worktree of a trusted repo inherits its trust.

Hook events map to agent states the same way for both agents:

| Hook events | Agent state |
|---|---|
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `Working` |
| `PreToolUse` for the question tool (Claude `AskUserQuestion`, Codex `request_user_input`) | `NeedsInput` (Question) |
| `PermissionRequest` for any other tool | `NeedsInput` (Approval) |
| `Stop` | `Done` |
| `StopFailure` (Claude only; keeps the `error` kind) | `Failed` |
| `Interrupt` (Codex only) | `Idle` |
| `SessionEnd`, a clean process exit, or grove stopped it | `Exited` |
| Process exits non-zero, or on a signal grove did not send | `Failed` |

- `Notification` is ignored: its permission prompt fires only after about 6 seconds and cannot tell an approval from a question.
- A `PermissionRequest` for the question tool counts as a question.
- A question asked in plain text ends with `Stop` and stays `Done`.
- When Codex rejects `request_user_input` (its default mode, unless the user turned that feature on), `PreToolUse` still fires and `Stop` follows about 50 ms later. The question then arrives as plain text.
- An event counts only when its agent `session_id` matches the one grove recorded for that session. `SubagentStop` never changes state.
- Codex failures and Claude interrupts fire no hook. The stuck-`Working` check below covers them.

## Status flow
In chat mode, agent state comes only from the protocol (see Chat mode). The steps below are terminal mode.
1. The agent runs a hook. `grove-hook` reads the JSON on stdin, keeps the fields grove needs, adds the session id (Claude: from its command line; Codex: from the environment), and sends it to the socket.
2. The adapter maps it to an `AgentState`.
3. Hook beats Heuristic: a heuristic may change a state only when no hook event has arrived since that state was set, and the next hook event always wins.
4. Leaving `NeedsInput`: no hook fires when the user answers an approval or a Claude question. A Codex question is the exception: answering it fires `PostToolUse` for `request_user_input`, which maps to `Working`. Otherwise, when the user writes to a `NeedsInput` session and output follows within 2 seconds, the state becomes `Working` (source `Heuristic`). The next hook confirms or corrects it.
5. Stuck `Working`: no hook event and no output for 10 seconds while `Working` triggers a screen check. Agent input prompt plus an error on screen: `Failed`. Prompt without an error: `Idle`. The 10 seconds is a starting value.
6. Heuristic fallback, only when hooks are not running (for example, Codex hooks not yet approved): output quiet for N seconds plus foreground process plus last screen lines (from the headless parser).
7. `focus` (the UI calls it when a panel gets focus) moves `Done` to `Idle` and records the worktree's most recently focused agent session.

## Hook relay contract (grove-hook)
- Lives at `~/.grovecode/bin/grove-hook`. grove copies it there from the app bundle at startup when it changed, so the path has no spaces and survives app updates.
- grove-hook finds the socket at `GROVE_SOCKET` from the environment, else `~/.grovecode/grove.sock`. It is not a flag because Codex asks to approve a hook again whenever its command changes.
- 200 ms total budget to connect and write. The hook `timeout` is set to 1 second as a backstop.
- Codex runs each hook through `$SHELL -lc`, so the user's login profile runs before `grove-hook` every time (25 to 30 ms measured). That counts against the 1 second timeout, not the 200 ms budget.
- Always exits 0 and never writes to stdout or stderr. Claude reads hook stdout as context or as a permission decision, and exit 2 blocks the tool call.
- If the socket is missing (grove not running), it exits 0 at once. Agents keep working without status.
- Hooks stay synchronous, so events from one agent arrive in order.
- The core accepts connections on a task that never blocks. At startup it takes `~/.grovecode/grove.lock` (one grove at a time), removes a stale socket file, then binds.

## Panel control flow (grove-mcp)
1. The agent launches `grove-mcp` as a child; it gets `GROVE_SESSION_ID`, `GROVE_SESSION_TOKEN` and `GROVE_SOCKET` from its MCP config (`env` for Claude, `env_vars` for Codex). The core names the binary in `CoreConfig.mcp_server`, and the app points that at the `grove-mcp` next to its executable.
   Each request carries the session id and token. The core refuses a wrong or missing token, so a session can act only as itself.
2. Tools: `subagent_spawn(agent, prompt, worktree?)`, `panel_open(kind, title, cmd)`, `panel_read(id, lines?)`, `panel_wait(id, state, timeout_seconds?)`, `panel_close(id)`, `workspace_get()`, `workspace_set(workspace)`, `profile_get()`, `profile_set(changes)`.
3. grove spawns subagents itself as real agent sessions. Each opens as an agent panel in chat mode beside its parent, and the user can watch it, type into it, or switch it to terminal mode. The core keeps each subagent's parent: `sessions` lists it and a restart brings it back. Subagents nest at most three levels below a session the user started. A deeper request is refused.
4. A log or command panel runs its command with the user's shell in the agent session's worktree, as a session whose id is the panel's id. A log panel shows output only; the user can type into a command panel. Only the agent session that opened a panel can read, wait on or close it. A session holds at most 8 panels at once, one whose command exited included, until it closes them. Closing a panel, or removing its worktree, kills its processes and sends PanelClosed. It does not come back after a restart.
5. Workspace and profile changes use only grove's built-in panels. A workspace change sends `WorkspaceChanged` with the agent session that made it. A profile change through `profile_set` sends `ProfilesChanged` with the agent session that made it, and the UI reloads. For each agent change the UI shows a notice with Undo, Keep and Save to profile (ADR 0016).
   `workspace_set` takes the whole workspace. It is refused if it drops the panel of an agent session running in the worktree or names a session that does not run there. `profile_set` takes the settings to change, in the profile file's schema; each one replaces the profile's. Both act only on the calling session's worktree and its repo's profile.
6. The `grovecode` CLI exposes the same commands for agents that prefer shell.

## Workspace and profiles
- The window has three regions: the sidebar, the chat area and the panel area. Each can be resized and collapsed.
- A workspace is one worktree's layout: which agent panels the chat area shows (several side by side, all in that worktree), which panels the panel area has open as tabs (Files, Diff, Browser, Logs, Profile), and whether a terminal panel sits at its bottom.
- A new worktree's workspace starts from its profile's layout. grove saves each workspace in its state, so it comes back after a restart.
- A profile is a TOML file in `~/.grovecode/profiles/` (ADR 0016) with:
  - the default agent, model, effort and permission mode
  - the layout
  - setup, cleanup and run sessions
  - launch args
  - start prompts
  - the review flow
  - which agent states notify
- Repo settings hold the chosen profile and the repo's overrides. `suggest_profile` names a profile whose marker files the repo has. The user picks.
- Agents edit profiles through `grove-mcp` (`profile_set`), the user in the Profile panel. `ProfilesChanged` tells the UI to reload. Its `by` names the agent session when an agent made the change, and is empty for the user's own edits and for edits to the file on disk.
- `set_repo_settings`, and so `set_repo_overrides_text`, sends one `RepoSettingsChanged` for the repo. Every Profile panel of that repo reloads its profile pick and overrides, and keeps unsaved edits with a notice. `by` is empty for the user.
- A profile file made invalid on disk can be repaired in the Profile panel. `profile_text` and `set_profile_text` find it by the name it had when it was last valid, or, if it has not been valid since grove started, by its file name. A valid profile of that name always wins, so a repair never writes over another profile's file.

## Editor
- The Files panel shows the worktree's tree (respecting `.gitignore`) and opens files in Monaco tabs. Edits save through `write_file`. Lines changed since `HEAD` get gutter marks.
- Language servers come after M7, one language first. The editor keeps the file model separate from the panel, so a language client can attach later without a rewrite.

## Diffs
- `Head`: the worktree against `HEAD`, staged and unstaged together, plus untracked files. Same as `git diff HEAD`.
- `MergeBase`: the worktree against the merge base of the base branch and `HEAD`, so committed and uncommitted work show together. Same as `git diff $(git merge-base <base> HEAD)`, using `origin/<base>` when it exists.
- git2 supplies the file list, status, paths, binary flag, hunk ranges (sidebar counts and comment anchors), and both versions of each file as bytes.
- Monaco draws the diff from those versions with `ignoreTrimWhitespace: false`, so whitespace-only changes show. A Monaco model keeps one line ending for the whole file and drops `\r` from each line, so a change of line endings alone shows no diff there (spike #8). grove replaces each `\r` with a visible `␍` in both versions before they reach Monaco. Binary and non-UTF-8 files show as binary, outside Monaco.
- What parity means, and when the git CLI is used instead of git2: ADR 0012.

## Review comment flow
1. User selects lines in the Monaco diff and adds a comment. It is anchored to the current version of the file: path, line range, and the text of those lines.
2. "Send batch" formats all open comments as one message (`path:line`, snippet, comment). ESC and other control characters except newline and tab are stripped from every part, so a snippet cannot end the paste early.
3. The batch goes to one agent session that is `Idle` or `Done`. The default target is the worktree's most recently focused agent session. Sent comments become `Sent`.
   - Chat mode: it goes as one message through `send_message`, and the chat shows it as a review batch card.
   - Terminal mode: the session must have bracketed paste on (mode 2004, read from the headless parser). The batch is wrapped in bracketed paste and followed by Enter.
4. On `DiffChanged`, grove searches for each comment's text and moves the anchor if found. If the text appears more than once, the copy closest to the old start line wins, the earlier one on a tie. Renames come from the diffs against HEAD and against the merge base, so a committed rename is followed too. If the text is gone, the file is deleted, or a rename git does not detect, the comment is marked "possibly addressed". The flag clears when the text comes back.
5. The user resolves comments, open or sent. Resolved comments are hidden but kept.

## Keyboard and WebKit
These apply to terminal panels, which include agent sessions in terminal mode.
- xterm.js uses `macOptionIsMeta: true`. Shift+Enter sends `ESC CR`, the newline Claude Code accepts without the kitty keyboard protocol, which xterm.js 6.0 lacks.
- An Option dead key (Option+E on the US layout) still starts a macOS accent. xterm.js sends `ESC e`, and then the next key arrives composed (`é`). The panel drops the composed text and sends that key plain.
- Cmd chords belong to the app (`attachCustomKeyEventHandler`). Everything else goes to the PTY.
- Cmd-V uses WebKit's own paste event, and xterm.js wraps the text in bracketed paste. No clipboard command is needed.
- WebKit keeps 16 live WebGL contexts and drops the one drawn least recently. A disposed context still counts until garbage collection, so disposing on hide and loading on show made an idle shown panel lose its context after about 70 hides.
- A panel loads the WebGL addon when first shown and keeps it while hidden, so showing a panel makes no new context.
- Before a panel makes a context, the shown panels redraw (`term.refresh`) and it waits one animation frame for that draw, so WebKit drops a hidden panel's context, never a shown one's.
- A panel that loses its context reloads the addon: at once on the canvas's `webglcontextlost` event if shown, otherwise when next shown. xterm.js's `onContextLoss` fires 3 s after the loss, too late to use. Past 16 shown panels, the extra ones use the DOM renderer.

## Threading and performance
- The core runs inside the Tauri process (ADR 0009). Closing the window keeps sessions running; Cmd-Q ends them.
- tokio runtime for core logic; std threads for PTY reads, writes, and waits.
- Target: 8 streaming sessions without UI stalls, in chat mode or terminal mode.
- The Claude sidecar and `codex app-server` run as child processes. Their stdio is read on std threads and forwarded through channels, like PTY output.
- Scrollback: 5,000 lines per xterm.js panel, 1,000 lines in the headless parser. Checked against the M4 performance run.
- State persisted to a JSON file under `~/.grovecode/` for MVP (worktrees, sessions to restore, comments), written atomically (temp file, then rename) with a schema version. Schema version 2 adds per-repo run sessions and launch args; once profiles land they move into the repo's overrides of its profile (repo settings). Version 5 adds each worktree's sessions to restore and review comments, and the session in each agent panel. Version 6 adds the parent of each subagent. Revisit SQLite if it grows.
- Changes that no call waits to save, such as a session ending or a comment moving, ask a background task to save. It writes on a blocking thread, off the tokio workers. Quitting saves once more before it ends the sessions and saves nothing after, so the sessions it ends still restore. After a crash, the last save restores.
- Names (ADR 0011): the home folder, the CLI command, and the app bundle id use `grovecode`. Everything else says grove.
- Login shell environment: at startup the core runs `$SHELL -ilc` once to print `env -0` between two marker lines, with a 5 second timeout, and uses that environment for every PTY. The shell starts from a clean base (`env -i` plus `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, and a minimal `PATH`), so variables from whatever launched grove (for example `CLAUDECODE` from a Claude Code terminal) never reach PTYs. grove then sets `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=grove`, and `LANG` when it is missing. If the read fails, grove uses the minimal environment and shows a warning. A Dock launch otherwise gets a minimal `PATH` and cannot find `claude` or `codex`.
- The socket's folder is readable only by the user, and the socket file is `0600`. Each agent session process gets its own token, which its panel requests must carry.

## Worktree lifecycle
- Setup and cleanup scripts (M5): from the repo's profile, never in the repo. A blank script counts as none.
  - Setup runs as a run session right after Add, only in worktrees grove creates. `spawn_agent_after_setup` starts an agent session once setup exits 0, and starts none when setup fails. `spawn_agent` does not wait. Setup's output stays attachable after it ends, until the worktree is removed.
  - Cleanup runs during Delete, after the checks and after the worktree's sessions end, right before `git worktree remove`. It runs with no terminal, and grove kills it after 5 minutes. If it fails, the delete stops, the worktree stays, and the error holds the script's last 20 lines of output. Forget never runs it.
- Add: the user types a branch name. A new branch is created from the base branch; an existing local or remote branch is checked out. Display name defaults to the branch; folder name replaces `/` with `-`.
- Base branch defaults to the repo's default branch (`origin/HEAD`, then `main`, then `master`). The merge base uses `origin/<base>` when it exists.
- Forget: grove stops tracking it; disk untouched. The main worktree can only be forgotten with its whole repo.
- Delete: kills its sessions, runs the cleanup script, then `git worktree remove`. With uncommitted changes or commits on no remote, the user must type the worktree name to confirm. The branch is kept; deleting it is offered only when it is merged into the base branch.

## Quit and restore
- Cmd-Q asks for confirmation when any agent session is `Working` or `NeedsInput`.
- grove stops a session by killing its whole process group, so tools the agent started do not keep the PTY open. Each child is waited on in its own thread, and exit comes from that wait, not from the reader reaching end of file. Sessions grove stops become `Exited`.
- On launch: agent sessions resume their conversation in the mode they were in (chat mode through the sidecar's `resume` or `thread/resume`; terminal mode with `claude --resume <id>` or `codex resume <id>`), with the same integration as a first launch. Workspaces come back as they were. Shell sessions start a fresh shell in the same worktree, run sessions show as stopped. No scrollback restore in MVP.
- `Core::start` restores before it returns. Each session keeps its session id, so its integration files and its panel need no change. New sessions get ids above the restored ones. A session that cannot start again is left out with a `SessionNotRestored` warning, and a panel whose session did not come back is dropped. Review comments come back with their states, and grove places them again at once, since files may have changed while it was closed.

## Notifications (M5)
- When grove is not the focused app, a session entering a state its profile lists sends a macOS notification. Clicking it focuses that session. The default list is `NeedsInput` and `Failed`; `Done` is off.
