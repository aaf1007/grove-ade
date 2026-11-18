# 0009: Core runs inside the app process

- Status: Accepted (Oct 2026)
- Context: Sessions could live in a background daemon so they survive the UI quitting or crashing. That needs a client/server protocol, a daemon lifecycle, and upgrade handling.
- Decision: `grove-core` runs inside the Tauri process. Quitting grove (Cmd-Q) ends every PTY; closing the window does not quit. On the next launch grove restores worktrees and relaunches sessions.
- Consequences: Much less to build for MVP. A UI crash kills running agents, so restore must be good. The Unix socket lives only while the app runs.
- Revisit if: losing sessions on quit or crash causes real lost work during dogfooding.
