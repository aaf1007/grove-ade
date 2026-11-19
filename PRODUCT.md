# Product

<!-- impeccable:product-schema 1 -->

## Platform

web

## Users

The primary user is the project's author: one developer on macOS (Apple Silicon) running 3 to 8 Claude Code or Codex agent sessions at once, each in its own git worktree. During the day they answer agents, notice which agents are blocked, review each branch's diff, read code and run commands, and they switch tasks often.

Other developers come later, once grove is open source. Until the dogfood week, the primary user's needs win. Where supporting other users is cheap, grove stays open to them: profiles are files, nothing is tied to one machine, and grove holds no credentials.

## Product Purpose

grove removes context switching when several AI coding agents run in parallel. It replaces terminal multiplexer panes plus a separate editor window per worktree with one window. Agents that are blocked don't sit idle unnoticed.

Success: the primary user is happy to do daily work in grove. This is the "usable first" bar, a good UI plus the core features, and it comes before dogfooding. The product need behind it is still a hypothesis, because validation was skipped.

## Positioning

"The agent IDE that fits how you work." It is a control room for coding agents, shaped per project by the user and by their agents.

- **Profiles fit each workflow.** Each kind of project, such as a Rust crate or a Next.js app, gets a profile. It sets the default agent and model, the layout, setup scripts and run sessions, start prompts, the review flow and notifications. Agents can change the profile and the layout through `grove-mcp`, and each change shows a notice with Undo. This is the headline.
- **Attention queue.** One sidebar, and one key that jumps to the next agent session that needs the user. State comes from the agent's own signals: its protocol in chat mode, and its hooks in terminal mode.
- **Branch review with comments sent back to the agent.** The user reviews the whole branch against its merge base, leaves line comments, and sends them to the agent as one batch.
- **Agent-controlled panels.** Agents open subagents, log panels and browser panels that the user can watch and type into. This works the same way for Claude Code and Codex.

## Operating Context

- A desktop app for macOS: a Tauri 2 shell around a React UI, with a Rust core.
- The window has three regions. The sidebar lists repos, then their worktrees, each with its branch, change counts, age and agent state. The chat area holds one or more agent panels side by side. The panel area holds tabs for Files and the editor, the diff, a browser and logs, with a terminal at the bottom.
- Agent sessions run in chat mode by default and can switch to terminal mode, which shows the agent's real TUI.
- grove uses the user's own agent CLIs, logins, config, skills and MCP servers.
- Use `GLOSSARY.md` terms exactly in the UI and docs. For example, say "agent session", not "thread", and "panel", not "pane".

## Capabilities and Constraints

- Agents are limited to Claude Code and Codex until after dogfooding.
- grove never stores, reads or proxies agent credentials.
- grove never hides the agent: any agent session can switch to terminal mode.
- grove is not a full code editor. The MVP editor is Monaco with a file tree. Language servers come after the dogfood milestone.
- No cloud, no multi-user and no team features.
- The shared-port dev server router is stopped.
- Decided for later: sandboxed widget panels, which come after profiles.
- Wanted later, not planned yet: built-in VMs or sandboxes, and an agent memory system.

## Brand Commitments

- The name is "grove", always lowercase. "grove-ade" is only the repo name.
- Voice in docs, UI copy and comments: plain and short, with no em dashes.

## Evidence on Hand

- `docs/vision.md`, `docs/architecture.md`, `GLOSSARY.md` and `docs/adr/` describe the product and its decisions.
- An approved prototype of the whole window is at `research/prototype/grove-ade-prototype.html`. It is local and git-excluded. It is the target for the current UI work and uses the existing theme tokens.
- There are no users, testimonials, metrics or benchmarks yet. Don't invent any.

## Product Principles

1. **Attention goes to the agent that needs it.** Make a blocked agent impossible to miss, and never make the user hunt for it.
2. **The user and their agents shape the workspace.** Layouts and flows come from profiles, not one fixed screen for everyone. Every change an agent makes can be undone.
3. **The real agent, never a wrapper.** Respect the user's own agent setup, and keep terminal mode one step away.
4. **Usable before broad.** Finish the core loop for one developer before widening scope.
5. **Local and trustworthy.** grove runs on the user's machine, holds no credentials, and writes nothing into worktrees.

## Accessibility & Inclusion

- Keyboard-first. Every core action works from the keyboard, including the jump to the head of the attention queue, switching panels and agent sessions, and review.
- Text meets WCAG AA contrast in both the light and dark themes.
