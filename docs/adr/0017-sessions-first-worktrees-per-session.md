# 0017: Sessions first, a worktree is a choice per session

- Status: Proposed (Nov 2026)
- Context: grove made the worktree its unit of work. The user adds a git repo, makes a worktree, then starts sessions in it, and a plain folder is refused. Other agent apps (T3 Code, Codex, Claude Code desktop) lead with the session instead: any folder is a project, the user starts a session in it, and picks where it runs. The developer wants grove to start sessions the same way, while keeping worktrees as the default for parallel work. Research for issue #59 compared the three apps.
- Decision:
  - A project is any folder the user adds. A git project gets worktrees, branches, the diff and review. A plain folder gets local sessions only, and an Initialize git action that runs `git init`.
  - The main action is New session. It asks where the session runs: a new worktree, the local checkout on a new branch (branch out), the local checkout on its current branch, or an existing worktree. The default is a new worktree; the project's profile can change it.
  - A new worktree is created when the session's first message is sent. Its branch gets a generated name the user can rename.
  - Where a session runs is locked after its first message. Hand off moves a session and its uncommitted changes between the local checkout and a worktree, both ways.
  - Several sessions may share the local checkout. When its branch changed since a session last ran, that session shows a banner with Restore and Dismiss. Branch out says so first when the checkout has uncommitted changes.
  - The sidebar lists projects, then their sessions. Each row shows the agent state, the branch and whether the session runs in a worktree. The attention queue spans all projects.
  - The workspace (agent panels, panel area tabs, terminal) belongs to an agent session, not a worktree. It starts from the profile's layout.
  - The Diff panel opens on uncommitted changes (`Head`). The whole-branch view (`MergeBase`) stays one click away and still drives branch review.
  - Deleting the last session on a worktree asks whether to remove the worktree too, and warns when it has uncommitted changes. No automatic cleanup yet.
- Consequences:
  - Every session still runs in exactly one checkout: the local checkout (for a git project, the main worktree) or one worktree. The core keeps tracking worktrees; what changes is that sessions, not worktrees, are what the user starts and sees.
  - The Core API changes: adding a plain folder, starting a session with a place, branch out, hand off, and workspaces keyed by session. `docs/architecture.md` changes with each slice that changes it.
  - The saved state moves from repos and worktrees to projects and sessions, so grove needs a state migration.
  - Hand off and branch out run git operations that can fail on conflicts; both must say why and leave the checkout as it was.
- Revisit if: during dogfooding most sessions run local and worktrees go unused, or several sessions in one checkout keep breaking each other.
