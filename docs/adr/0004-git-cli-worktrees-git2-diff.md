# 0004: git CLI for worktree changes, git2 for diffs

- Status: Accepted. Superseded in part by 0012 (what the parity test compares).
- Context: Worktree creation must respect the user's git config and hooks. Diffs must match `git diff`.
- Decision: `git worktree add/remove` through the CLI. Diffs through `git2` with the indent heuristic, guarded by an automated parity test against `git diff --no-color`. Fall back to parsing CLI output for any case git2 cannot match.
- Consequences: Requires git installed (fine on macOS). Parity fixture repo must be maintained.
- Revisit if: parity keeps failing on edge cases, or `gix` becomes a good learning stretch for read paths.
