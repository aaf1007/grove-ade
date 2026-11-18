# 0001: Tauri (React + TS) over GPUI

- Status: Accepted (Oct 2026)
- Context: Needed a UI stack for terminals, a diff viewer, and a sidebar. GPUI is native Rust and fastest, but has thin docs and no ready-made terminal view or diff editor under a license grove can use. Tauri can use xterm.js and Monaco, both mature.
- Decision: Tauri 2 with React + TypeScript. Rust lives in the core.
- Consequences: Much faster path to a usable app. More memory than native. Rust learning comes from the core, not the UI. Must test in WebKit, since that is Tauri's webview on macOS.
- Revisit if: the M4 performance tests fail and cannot be fixed by batching or renderer changes.
