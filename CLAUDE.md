# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

# Global Coding Standards

1. **YAGNI**: Don't build it until you need it
2. **DRY**: Extract patterns after second duplication, not before
3. **Fail Fast**: Explicit errors beat silent failures
4. **Simple First**: Write the obvious solution, optimize only if needed
5. **Delete Aggressively**: Less code = fewer bugs
6. **Semantic Naming**: Always name variables, parameters, and API endpoints with verbose, self-documenting names that optimize for comprehension by both humans and LLMs, not brevity (e.g., `wait_until_obs_is_saved=true` vs `wait=true`)

# grove

Desktop app that kills context switching when running several AI coding agents (Claude Code, Codex) in parallel, in any project folder, each session in its own git worktree by default (ADR 0017), in a workspace each user and their agents shape per project. The project and repo are both named grove.

## Read first
- `GLOSSARY.md`: domain vocabulary. Use these terms exactly in code, docs, and chat.
- `docs/vision.md`: the problem, what makes grove different, non-goals
- `docs/architecture.md`: modules, crates, Core API, events, data flow
- `docs/adr/`: decisions already made. Do not reopen one without writing a new ADR.
- `docs/acceptance.md`: tests each milestone must pass
- `docs/workflow.md`: how we plan, build, and review

## Stack
- Rust core: `crates/grove-core` (UI-agnostic, tokio)
- Tauri 2 shell + React + TypeScript UI
- Chat mode: the Claude sidecar (`sidecar/claude`, Node on the Claude Agent SDK) and `codex app-server` (ADR 0015)
- xterm.js (WebGL addon) for terminals and terminal mode, Monaco for the editor and the diff
- `grove-hook`: tiny binary that relays Claude Code and Codex hooks to the core
- `grove-mcp`: MCP server (plus CLI) so agents can open and control panels
- macOS first (Apple Silicon)

## Architecture in brief
- `grove-core` owns all logic and its own tokio runtime. `src-tauri` only maps commands to the Core API and forwards `CoreEvent`s (ADR 0002).
- Each agent session has a `GROVE_SESSION_ID`. Claude hooks get it on the `grove-hook` command line; Codex hooks get it from the environment; both agents' MCP servers get it from their MCP config. It travels with every message to `~/.grovecode/grove.sock` and is how the core ties a hook or panel request to a session.
- An agent session runs in chat mode (driven over its protocol) or terminal mode (its real TUI in a PTY), one mode at a time (ADR 0013).
- PTY output and agent messages never ride the `CoreEvent` bus: each attached panel has its own acked stream, and a hidden panel never slows its agent.
- Agent state: `Protocol` in chat mode (ADR 0014); Hook > Heuristic in terminal mode (ADR 0005, 0010). Hooks and MCP config are passed at launch in both modes, never written into worktrees. Heuristics read the headless `alacritty_terminal` screen; xterm.js only renders.
- grove never handles agent credentials. Agents use the login the user made with their own CLI (ADR 0015).
- Profiles are TOML in `~/.grovecode/profiles/`. Agents change the workspace and profile through `grove-mcp`, and each change shows a notice with Undo (ADR 0016).
- Per-agent code lives behind the `AgentAdapter` trait. Only Claude Code and Codex until after M7 (ADR 0007).
- Subagents are real agent sessions that grove spawns, not the agent's own subagents (ADR 0006).
- MVP state is a JSON file under `~/.grovecode/`. `Run` sessions record their port for the M8 port router.

## Planned layout
```
grove/
  CLAUDE.md
  GLOSSARY.md
  docs/
  crates/
    grove-core/     # worktrees, sessions (PTY), diff, status, events
    grove-hook/     # hook relay -> Unix socket
    grove-mcp/      # MCP server + `grovecode` CLI for panel control
  sidecar/
    claude/        # Node: Claude Agent SDK <-> core over stdio
  app/
    src-tauri/     # thin bridge: commands + event channels
    src/           # React UI
  fixtures/        # diff parity repo, recorded agent sessions
  evals/           # bug replay and live funnel for the review stages, with their results
```

## Code review rules
Each rule here can be checked against a diff. The Engineering rules below apply as well.
- A change to behavior in `crates/`, `app/src/` or `sidecar/` adds or changes a test in the same PR.
- Logic goes in `grove-core`. `app/src-tauri` only maps commands to the Core API and forwards events (ADR 0002).
- grove never writes hooks or MCP config into a worktree, and never reads or stores agent credentials.

## Engineering rules
- Rust gates before saying "done": `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- TS gates: `tsc --noEmit`, `eslint`, `vitest run`.
- Errors: `thiserror` in `grove-core`, `anyhow` only in binaries. No `unwrap()` in core outside tests.
- Never block a tokio worker. PTY readers and the stdio readers of the Claude sidecar and `codex app-server` run on std threads and forward through channels.
- PTY output and agent messages reach the UI through a batched `tauri::ipc::Channel`, at most one frame per 8 ms per session: output that arrives 8 ms or more after the last frame goes out at once, and sooner output waits until 8 ms after that frame. Never one event per chunk.
- Worktree add/remove: git CLI. Diffs: git2, guarded by the diff parity test.
- One issue, one worktree, one PR. Keep slices vertical and small.
- Never cut: acceptance tests, diff parity, agent state from the agent's own signals (protocol in chat mode, hooks in terminal mode, with heuristics only as the terminal-mode fallback).
- Writing style for docs and comments: plain and short, no em dashes.

## Rule enforcement
`correct` keeps this table in step with the rules in this file. Proposed for the owner's approval. A rule can have more than one enforcer. CodeRabbit and the reviewer only advise.

| Rule | Enforced by |
|---|---|
| A behavior change adds or changes a test | The reviewer. Nothing automatic |
| Logic in `grove-core`, `src-tauri` only maps (ADR 0002) | CodeRabbit path instructions in `.coderabbit.yaml`, and the reviewer |
| No hooks, MCP config or credentials in a worktree | The reviewer. Nothing automatic |
| Rust gates: `fmt`, `clippy`, `cargo test` | CI `rust` check |
| TS gates: `tsc`, `eslint`, `vitest` | CI `ts` check |
| `thiserror` in core, `anyhow` only in binaries | CodeRabbit path instructions. No lint |
| No `unwrap()` in core outside tests | CodeRabbit path instructions, and the reviewer. No lint |
| Never block a tokio worker | CodeRabbit path instructions, and the reviewer. No test |
| 8 ms output batching | Core side: `crates/grove-core/tests/sessions.rs`, `chat.rs` and `config.rs`. App side, `tauri::ipc::Channel` and no event per chunk: CodeRabbit path instructions only |
| Git CLI for worktrees, git2 for diffs | CodeRabbit path instructions. Diff results are checked by the parity tests below |
| Diff parity | `crates/grove-core/tests/diff.rs` in the `rust` check. `fixtures/diff-repo/check.py` in the `ts` job checks the fixture builds the same each time |
| One issue, one worktree, one PR | The owner's review. Nothing automatic |
| Never cut: acceptance tests, diff parity, agent state from its own signals | Diff parity as above. The rest: the owner's review |
| Docs and comments: plain and short, no em dashes | CodeRabbit path instructions for `*.md`, and the `unslop` pass |

## Commands
Run from the repo root. `rust-toolchain.toml` pins the toolchain.
- Format: `cargo fmt --check`
- Lint: `cargo clippy --all-targets -- -D warnings`
- Test: `cargo test`
- One test: `cargo test -p grove-core <name>`. It runs every test whose path contains `<name>`.
- App bundle: `npm run bundle` from `app/` builds the .app with `grove-hook` inside.
