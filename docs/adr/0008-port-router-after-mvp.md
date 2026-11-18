# 0008: Same-port dev server router after MVP

- Status: Accepted
- Context: The router is a strong differentiator but not needed to prove the core value.
- Decision: Build it in M8. Until then, `Run` sessions record their port so the router can plug in without core changes.
- Consequences: During MVP, each worktree's dev server uses its own port.
- Revisit if: the baseline log shows `run_cmd` switches dominate.
