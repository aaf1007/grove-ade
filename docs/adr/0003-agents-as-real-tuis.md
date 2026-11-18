# 0003: Agents run as their real TUIs in PTYs

- Status: Superseded by 0013 (Nov 2026)
- Context: Some tools wrap agents in their own chat UI. That hides features and breaks when agents update.
- Decision: Claude Code and Codex run as their normal CLIs inside PTYs rendered by xterm.js. grove adds status, review, and panels around them.
- Consequences: Terminal correctness becomes a hard requirement (see `acceptance.md`). New agent features work on day one.
- Revisit if: an agent drops its TUI.
