# 0014: Agent state comes from the protocol in chat mode

- Status: Accepted (Nov 2026). Supersedes part of 0005 and 0010 (the state source for chat mode).
- Context: ADRs 0005 and 0010 take agent state from hooks, with heuristics as the fallback, because a TUI reports nothing else. In chat mode grove drives the agent through its protocol, which reports a turn starting and ending, each approval request and each question as it happens.
- Decision: A new state source, `Protocol`. In chat mode every agent state comes from the protocol stream, plus the core's own wait on the agent process for exits. Both use source `Protocol`. In terminal mode Hook beats Heuristic, as before. The rule that is never cut becomes: agent state comes from the agent's own signals (protocol or hooks), never from guessing alone.
- Consequences: grove still passes its hooks and MCP config at launch in both modes (ADR 0010), so switching modes needs no new integration and `grove-mcp` keeps its session id. Codex hook approval matters only in terminal mode, because chat mode answers approvals over app-server.
- Revisit if: a protocol stops reporting approvals or turn ends.
