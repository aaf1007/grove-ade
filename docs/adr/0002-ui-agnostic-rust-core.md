# 0002: UI-agnostic Rust core

- Status: Accepted
- Context: Want the app logic testable without a UI and want the option to change UI later.
- Decision: All logic (worktrees, sessions, diff, status, comments, adapters) lives in `grove-core`. `src-tauri` is a thin bridge with no business logic.
- Consequences: Core is fully testable with `cargo test`. The UI only calls the Core API and listens to `CoreEvent`.
- Revisit if: never, unless the product changes shape.
