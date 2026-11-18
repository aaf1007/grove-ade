# 0006: MCP server plus CLI for agent-controlled panels

- Status: Accepted
- Context: Want agents to open subagent, log, and preview panels the user can watch. Both Claude Code and Codex support MCP servers and can run shell commands.
- Decision: One `grove-mcp` stdio server and a `grovecode` CLI with the same commands, both talking to the core socket. grove spawns subagents itself as real agent sessions.
- Consequences: Agent-agnostic. Does not depend on each agent's internal subagent features.
- Revisit if: MCP env forwarding blocks session identification in Codex and no workaround exists.
