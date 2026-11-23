# Glossary: grove vocabulary

Use these terms exactly. If a new concept appears, add it here in the same PR.

| Term | Meaning |
|---|---|
| Project | A folder the user adds to grove. It is a repo or a plain folder. A plain folder has local sessions only. |
| Repo | A project that is a git repository. Only a repo has worktrees, branches, the diff and review. |
| Project settings | What grove saves for a project in its own state, never in the project: the profile it uses and its overrides. |
| Profile | A reusable setup for a kind of project, saved as TOML in `~/.grovecode/profiles/`: default agent, model, effort and permission mode, layout, setup, cleanup and run sessions, launch args, start prompts, review flow and notifications. A project picks one; grove suggests one from marker files such as `Cargo.toml`. Agents can change it through `grove-mcp`. |
| Worktree | A git worktree grove tracks, either created by grove or adopted from the repo. A folder plus a branch, with a display name and an optional issue link. _Avoid_: ticket. |
| Local checkout | A project's own folder. For a repo it is the main worktree. |
| Main worktree | The repo's original checkout. Grove treats it as a worktree but never removes it. |
| Nested worktree | A worktree whose first subagent came from an agent session in another worktree of the same repo. The sidebar shows it under that worktree, one level deep, and it stays there after both sessions end. _Avoid_: linked worktree, which is git's word for any worktree but the main one. |
| Forget | Stop tracking a worktree without touching it on disk. |
| Delete | Remove a worktree from disk and stop tracking it. |
| Base branch | The branch a worktree, or a branch made by branch out, is compared against for a whole-branch diff. Set when the branch is made; can be changed. |
| Session | Something grove runs in one project, with one session id for its whole life. It runs in exactly one place: the local checkout or one worktree. A shell or run session is one process in a PTY. |
| Place | Where an agent session runs: a new worktree (the default), the local checkout on a new branch (branch out), the local checkout on its current branch, or an existing worktree. Picked in New session and locked after the first message. |
| Branch out | Start an agent session in the local checkout on a new branch instead of a worktree. |
| Hand off | Move an agent session and its uncommitted changes between the local checkout and a worktree, either way. |
| Agent session | A session running Claude Code or Codex: one conversation, shown in chat mode or terminal mode. Switching mode ends its process and resumes the conversation in a new one under the same session id. _Avoid_: thread. |
| Chat mode | An agent session driven through its protocol (the Claude sidecar or `codex app-server`), with grove drawing the messages, tool calls, approvals and questions. The default. |
| Terminal mode | An agent session running as the agent's real TUI in a PTY. It resumes the same conversation as chat mode. A conversation runs in one mode at a time. |
| Turn | One message to an agent session (from the user, or a review batch) and the agent's work until it stops. |
| Claude sidecar | The Node program that runs the Claude Agent SDK for one agent session in chat mode and talks to the core over stdio. |
| Shell session | A plain interactive shell in an agent session's place. |
| Run session | A named command from the profile (dev server, tests), started in one place. It may record a port. |
| Launch args | Extra args a profile sets for each agent. A repo's own launch args replace the profile's. They go after grove's own flags and the prompt. |
| Setup script | Commands from the profile that grove runs in a new worktree before agents start. |
| Cleanup script | Commands from the profile that grove runs in a worktree before deleting it. |
| Panel | Any visible view in grove: an agent session, a terminal, files, diff, browser, logs, profile. _Avoid_: pane, surface. |
| Agent panel | A panel showing one agent session. Several can sit side by side in the chat area. |
| Chat area | The middle region of the window, holding the selected agent session's panel and the panels of the subagent sessions it spawned. |
| Panel area | The right region of the window: panels as tabs, with an optional terminal panel at its bottom. |
| Editor | Monaco tabs in the Files panel for reading and editing a worktree's files. Its file model is kept apart from the panel, so a language server can attach later. |
| Workspace | One agent session's layout: its agent panels, the panels open in the panel area and whether the terminal is open. Starts from the profile's layout and is saved with the session. A subagent session shows in the workspace of its top parent. |
| Subagent panel | An agent panel holding an agent session that another agent session spawned through `grove-mcp`. Its parent is recorded. |
| Log panel | A panel in the panel area that shows the output of a command an agent session ran through grove-mcp. The user cannot type into it. Only that agent session can read, wait on or close it. |
| Command panel | A panel in the panel area that runs a command an agent session started through grove-mcp, as a terminal the user can type into. Only that agent session can read, wait on or close it. |
| Snapshot | A PTY session's terminal when a panel attaches: size, screen, scrollback, cursor and modes, as bytes the panel replays before live output. |
| Agent state | One of `Working`, `NeedsInput`, `Failed`, `Done`, `Idle`, `Exited`. `NeedsInput` has a kind: `Approval` or `Question`. `Failed`: the turn or the process ended on an error. `Done`: the turn ended and the user has not looked at the panel since. `Idle`: nothing is waiting on the user. `Exited`: the process ended cleanly or grove stopped it. |
| Usage reading | An agent's plan limits as its chat-mode transport last read them: limit windows, each with a name, a percent used and a reset time, or no plan limits for an API key account. One per agent. grove never calls a provider itself. |
| Context reading | A chat-mode agent session's total tokens and how full its context window is, reported after each turn. |
| State source | Where an agent state came from: `Protocol` in chat mode; `Hook` or `Heuristic` in terminal mode, where Hook beats Heuristic. |
| Attention queue | Agent sessions waiting on the user, ranked Approval, Question, `Failed`, `Done`, oldest first within each. One key jumps to the head. |
| Diff base | What a diff compares against: `Head` (all uncommitted changes, staged or not, plus untracked files) or `MergeBase` (everything since the branch left its base branch, committed or not). The Diff panel opens on `Head`. |
| Review comment | A note the user attaches to a file and line range in a diff. It is `Open`, `Sent`, or `Resolved`, and can be flagged possibly addressed. |
| Review batch | All open review comments for a place, sent as one message to one agent session in that place: a chat message in chat mode, a bracketed paste in terminal mode. |
| Adapter | Per-agent code (Claude Code, Codex) for launching in each mode, the hook and MCP integration passed at launch, and parsing protocol and hook events. |
| Core event | A message the core broadcasts to the UI (`AgentState`, `DiffChanged`, `CommentsChanged`, `WorkspaceChanged`, ...). |
| Hook relay | `grove-hook`: forwards agent hook payloads to the core socket with the session id. |
| Port router | Post-MVP: proxies one fixed localhost port to the active session's dev server. |
