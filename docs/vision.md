# Vision

## Problem
When I work several tasks in parallel with AI coding agents, I bounce between terminal multiplexer panes and separate editor windows per git worktree to answer agents, review diffs, read code, and run commands. Each switch costs focus, and blocked agents sit idle because I don't notice them.

Every developer also works differently, and the same developer works differently per project, language and framework. Today's agent tools give everyone the same screen and the same flow.

Validation was skipped, so this is still a hypothesis.

## Who it's for
Me first: one developer on macOS running 3 to 8 agent sessions across worktrees. Other developers later. Until the M7 dogfood week my needs win; where keeping grove open to other users is cheap (profiles as files, nothing tied to my machine, no credentials in grove), it stays open. No cloud, no team features.

## What grove is
The agent IDE that fits how you work. A control room for coding agents: you chat with Claude and Codex, watch several of them at once, review their work and read code, in a workspace you and your agents shape per project.

The window has three regions:
- **Sidebar:** repos, then their worktrees, each with branch, change counts, age and agent state.
- **Chat area:** one or more agent sessions side by side, in chat mode by default. A picker in the composer sets the agent, model and effort.
- **Panel area:** tabs for files and the editor, the diff, a browser and logs, with a terminal at the bottom.

## What makes it different
1. **It fits each workflow.** A profile per kind of project (a Rust crate, a Next.js app) sets the default agent and model, the layout, setup scripts and run sessions, start prompts, the review flow and notifications. Agents can change the profile and the layout too, and grove shows each change with Undo.
2. **Attention queue.** One sidebar and one key that jumps to the head of the attention queue. State comes from the agent's protocol in chat mode, and from its hooks in terminal mode, with a heuristic only as a fallback when hooks are not running.
3. **Branch review with comments back to the agent.** Review the whole branch against its merge base, leave line comments, send them as one batch to the agent, and flag comments as possibly addressed when those lines change.
4. **Agent-controlled panels.** Agents call `grove-mcp` (or the `grovecode` CLI) to spawn subagents and open log or browser panels I can watch and type into. Works the same for Claude Code and Codex.
5. **Same-port dev server router (post-MVP).** Every worktree runs its own dev server; `localhost:5173` always points at the active one.

## Non-goals
- Not an IDE to replace a full code editor for heavy editing. The MVP editor is Monaco with a file tree. Language servers come after M7, one language first.
- Not a wrapper that hides the agent. Chat mode loads the user's own agent config, skills and MCP servers, and any agent session can switch to terminal mode to use the agent's real TUI.
- Not cloud or multi-user.
- No agents beyond Claude Code and Codex until after dogfooding.
- grove never stores or proxies agent credentials. Agents use the login the user made with their own CLI.
