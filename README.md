# grove

A macOS desktop app for running several AI coding agents (Claude Code and Codex) in parallel, one git worktree per task, without the context switching.

You chat with your agents, see at a glance which ones need you, review their branches, and read code, all in one window. Each project gets a profile that sets the agent, layout, scripts and review flow, and agents can reshape the workspace too.

> **Status:** early and in active development. The core and the first UI slices exist.

## What makes it different

- **Fits each workflow.** A profile per kind of project sets the default agent and model, the layout, setup scripts, run sessions, start prompts and notifications. Agents can change it, and every change comes with Undo.
- **Attention queue.** One sidebar and one key to jump to the next agent that needs you. State comes from the agent's own signals: its protocol in chat mode, its hooks in terminal mode.
- **Branch review with comments back to the agent.** Review the whole branch against its merge base, leave line comments, and send them to the agent as one batch.
- **Agent-controlled panels.** Agents use `grove-mcp` (or the `grovecode` CLI) to spawn subagents and open log or browser panels you can watch and type into.
- **Not a wrapper.** Chat mode loads your own agent config, skills and MCP servers. Any session can switch to terminal mode to use the agent's real TUI. grove never stores or proxies agent credentials.

Full pitch and non-goals: [`docs/vision.md`](docs/vision.md).

## How it fits together

```
React UI (sidebar | chat area | panel area)  ── Tauri 2 shell (thin bridge)
                         │
                   grove-core (Rust, tokio)
   worktrees · sessions · diff · status · profiles · review
      │                    │                     │
 Claude sidecar      codex app-server      ~/.grovecode/grove.sock
 (Node, Agent SDK)   (JSON-RPC)             ▲              ▲
                                        grove-hook     grove-mcp
                                        (agent hooks)  (panels, subagents)
```

| Path | What it is |
|---|---|
| `crates/grove-core` | All the logic: worktrees (git CLI), sessions (PTY and chat), diffs (git2), status, events. UI-agnostic. |
| `crates/grove-hook` | Tiny binary that relays Claude Code and Codex hooks to the core. |
| `crates/grove-mcp` | MCP server and `grovecode` CLI so agents can open and control panels. |
| `sidecar/claude` | Node sidecar that drives Claude through the Claude Agent SDK. |
| `app/src-tauri` | Tauri shell. Maps commands to the Core API and forwards events. |
| `app/src` | React + TypeScript UI, xterm.js terminals, Monaco editor and diff. |
| `fixtures/` | Diff parity repo, fake agents, recorded sessions. |
| `docs/` | Vision, architecture, ADRs, acceptance tests, workflow. |

Details: [`docs/architecture.md`](docs/architecture.md). Decisions: [`docs/adr/`](docs/adr/). Vocabulary: [`GLOSSARY.md`](GLOSSARY.md).

## Getting started

### Requirements

- macOS on Apple Silicon
- Rust (the toolchain is pinned in `rust-toolchain.toml`; `rustup` picks it up)
- Node.js and npm
- [Bun](https://bun.sh), to build the Claude sidecar
- The `claude` and/or `codex` CLI, already logged in. grove uses your existing login.

### Run in development

In development the core runs `claude-sidecar` from your login shell's `PATH`. For chat mode with Claude, build it once before you start the app:

```bash
(cd sidecar/claude && npm ci && npm run build)
```

Then add the absolute path of `sidecar/claude/dist` to `PATH` in your shell profile (such as `~/.zshrc`). The core reads `PATH` when it starts, so restart the app after you change it.

Start the app from the repo root:

```bash
(cd app && npm install && npm run tauri dev)
```

### Build the app bundle

```bash
(cd app && npm install && npm run bundle)
```

This builds `grove-hook`, `grove-mcp` and the Claude sidecar, and packs them into the `.app`.

## Development

Run from the repo root.

| Gate | Command |
|---|---|
| Rust format | `cargo fmt --check` |
| Rust lint | `cargo clippy --all-targets -- -D warnings` |
| Rust tests | `cargo test` |
| TS types (in `app/`) | `npx tsc --noEmit` |
| TS lint (in `app/`) | `npx eslint .` |
| TS tests (in `app/`) | `npx vitest run` |

To run the gates on every push, enable the pre-push hook once in the main checkout:

```bash
git config core.hooksPath .githooks
```

How work is planned, built and reviewed: [`docs/workflow.md`](docs/workflow.md). Tests each milestone must pass: [`docs/acceptance.md`](docs/acceptance.md).

### Conventions

- One issue, one worktree, one PR.
- Branch: `<type>-<issue>-<slug>`, such as `ci-15-rust-and-fixture-checks`.
- PR title: `#<issue> <type>: <what it does>`. Type is one of `feat`, `fix`, `test`, `ci`, `docs`, `chore`.
- `main` is protected: changes land by PR, with the `rust` and `ts` checks green.

## Contributing

Issues and pull requests are welcome. Before you open a pull request, run every gate and make sure it passes.

From the repo root:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

In `app/` and again in `sidecar/claude/`, after `npm ci`:

```bash
npx tsc --noEmit
npx eslint .
npx vitest run
```

A change to behavior comes with a test in the same pull request. A new dependency needs the maintainer's OK first.

## License

MIT. See [`LICENSE`](LICENSE).
