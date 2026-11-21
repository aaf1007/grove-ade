# Triage Labels

The skills speak in terms of five canonical triage roles. This file maps those roles to the actual label strings used in this repo's issue tracker.

| Label in mattpocock/skills | Label in our tracker | Meaning                                  |
| -------------------------- | -------------------- | ---------------------------------------- |
| `needs-triage`             | `needs-triage`       | Maintainer needs to evaluate this issue  |
| `needs-info`               | `needs-info`         | Waiting on reporter for more information |
| `ready-for-agent`          | `AFK`                | Fully specified, ready for an AFK agent  |
| `ready-for-human`          | `HITL`               | Requires human implementation            |
| `wontfix`                  | `wontfix`            | Will not be actioned                     |

When a skill mentions a role (e.g. "apply the AFK-ready triage label"), use the corresponding label string from this table.

Agents write all code, `crates/` included, so issues are `AFK` unless the developer has to do the work, such as a manual measurement.

Other labels from `docs/workflow.md`: `spike`, `dogfood`, `adr-needed`.

## Component labels

Every new issue gets at least one `comp:*` label for the part of grove it touches. Use two when it touches two. A spec that spans all of grove gets none.

| Label             | Covers                                                    |
| ----------------- | --------------------------------------------------------- |
| `comp:sessions`   | Agent sessions, worktrees, handoff, restore               |
| `comp:chat`       | Chat mode: transcript, composer, approvals, details card  |
| `comp:sidebar`    | Sidebar and project list                                  |
| `comp:shortcuts`  | Keyboard shortcuts and command palette                    |
| `comp:diff`       | Diff panel                                                |
| `comp:editor`     | Files panel and Monaco editor                             |
| `comp:terminal`   | Terminal mode and terminal panel                          |
| `comp:profiles`   | Profiles and project overrides                            |
| `comp:subagents`  | Subagent sessions and their panels                        |
| `comp:agent-io`   | Hooks, MCP, and agent adapters                            |
| `comp:design`     | Design tokens, motion, visual polish                      |
| `comp:infra`      | CI, tests, performance, workflow                          |

Edit the "Label in our tracker" column to match whatever vocabulary you actually use.
