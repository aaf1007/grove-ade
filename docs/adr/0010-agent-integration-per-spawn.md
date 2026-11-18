# 0010: Agent integration passed at launch; hooks for both agents

- Status: Accepted (Oct 2026). Supersedes part of 0005. Superseded in part by 0014 (the state source in chat mode).
- Context: ADR 0005 had adapters install hooks into the worktree and used Codex `notify`. Files in a worktree can get committed, and Claude Code reads `.claude/settings.local.json` from the main checkout, not the worktree. Codex now has hooks, including `PermissionRequest`, and `notify` only reports turn completion. A `-c notify=...` override would also replace the user's own notifier.
- Decision: Adapters pass hooks and MCP config on every launch: Claude Code `--settings` and `--mcp-config`, Codex `-c` overrides with `env_vars=["GROVE_SESSION_ID"]` on the MCP server. Nothing is written into worktrees or global config. Both agents report status through hooks, and `Notify` is dropped as a state source: Hook beats Heuristic.
- Consequences: Codex hooks need a one-time approval in `/hooks`; until then grove falls back to the heuristic and says so. No `--dangerously-bypass-hook-trust`.
- Revisit if: either CLI drops these flags, or Codex hook approval stops persisting across launches.
