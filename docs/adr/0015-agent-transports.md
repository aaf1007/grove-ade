# 0015: Claude through an Agent SDK sidecar, Codex through app-server

- Status: Accepted (Nov 2026)
- Context: Chat mode (ADR 0013) needs a programmatic way to drive each agent. Claude has the Claude Agent SDK, which is TypeScript and documented, and `claude -p` with stream-json, whose stdin control messages are not a public protocol. Codex has `codex app-server`, a documented JSON-RPC protocol over stdio that Codex's own editor extension uses. Other agent UIs already use the SDK for Claude and app-server for Codex.
- Decision:
  - Claude: a small Node program, the Claude sidecar (`sidecar/claude/`), runs the Agent SDK with `settingSources` set to user, project and local, and talks to the core over stdio as JSON lines. It points the SDK at the user's installed `claude` binary.
  - Codex: the core runs `codex app-server` and speaks its JSON-RPC directly from Rust. grove already calls it for the hook check.
  - Auth: grove never handles credentials. Both agents use the login the user made with their own CLI (`claude auth login`, `codex login`), or an API key the user sets in their own shell environment. grove shows the account and plan each agent reports.
- Consequences:
  - grove ships a Node runtime for the sidecar. How it is bundled (a single binary, or Node beside the app) is picked during M4, as a dependency to approve.
  - Plugins from a marketplace load only by folder path through the SDK.
  - Anthropic's docs do not allow third-party products to offer claude.ai login or route subscription credentials for their users. They do allow a user to sign in to the unmodified Claude Code binary with their own subscription. They do not settle a personal tool that drives the user's own logged-in `claude` through the SDK. Before grove ships to anyone else, ask Anthropic.
- Revisit if: Anthropic publishes a documented stream-json protocol, or the sidecar's runtime costs more than it saves.
