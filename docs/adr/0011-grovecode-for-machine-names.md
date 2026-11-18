# 0011: `grovecode` for names shared with other tools

- Status: Accepted (Oct 2026)
- Context: The product is called grove. Another worktree tool, which also watches Claude Code agents, already uses `~/.grove/`, and a third tool reportedly installs a `grove` command.
- Decision: Anything another tool could also claim on the machine uses `grovecode`: the home folder `~/.grovecode/`, the CLI command `grovecode`, and the app bundle id. The display name, docs, crate names, and `GROVE_SESSION_ID` stay grove.
- Consequences: Two names to keep straight. Renaming the folder later means migrating user state.
- Revisit if: grove is ever published, at which point the whole name gets a proper check.
