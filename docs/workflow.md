# Workflow: validate, plan, build, review, improve

Tools: Matt Pocock's skills for planning, pstack for execution and review, and an orchestrator with role-based subagents on Claude Opus 5.5.

Skill names below may differ slightly between versions. Check your `/` menu and map them.

## Which skill stack does what
| Stage | Use | Why |
|---|---|---|
| Repo setup | `setup-matt-pocock-skills` | Points skills at the issue tracker, labels, and docs |
| Design grilling | `grill-with-docs` | Stress-tests the plan and updates GLOSSARY.md and ADRs as decisions land |
| Architecture attack | pstack interrogate / architect panel | Several reviewers try to break `architecture.md` |
| Spec | `to-spec` | MVP scope (M3 to M6) as one document |
| Issues | `to-tickets` | Tracer-bullet vertical slices, tagged HITL or AFK |
| Implementation | `implement` (Matt's), which drives `tdd` | One ticket per run, one failing test at a time, then `code-review` |
| Before risky refactors | pstack `blast-radius` | Shows what a Core API change touches |
| Two equal approaches | pstack arena | Build both, compare against `acceptance.md` |
| Cleanup | pstack `unslop` | Cuts AI filler from code and docs |
| Understanding code | pstack `how` / `why` | Explains code paths when I review |

Do not run both stacks' planners on the same task. Matt plans, pstack executes and reviews.

## Phase 1: Validate the problem (skipped)
Skipped on Oct 25.

## Phase 2: Validate the design (once, start of M3)
1. Run `setup-matt-pocock-skills`. Labels: `HITL`, `AFK`, `spike`, `dogfood`, `adr-needed`.
2. `grill-with-docs` on `vision.md` and `architecture.md` until no open questions remain for M3 to M5.
3. pstack interrogate on `architecture.md`. Ask it to attack: PTY batching, env forwarding to hooks and MCP, git2 diff parity, WebKit quirks. Each real risk becomes a `spike` issue.
4. `to-spec` for the MVP.
5. `to-tickets`. Every issue lists the acceptance tests it touches and is tagged:
   - `AFK`: an agent builds it. This is almost every ticket, `crates/` included.
   - `HITL`: I have to do it myself, such as a manual measurement.

## Phase 3: Build

### Roles
| Role | Model | Job |
|---|---|---|
| Orchestrator | Opus 5.5 | Holds roadmap and issues, writes nothing but plans and delegations |
| Planner | Opus 5.5 | Plan, edge cases, test list |
| Implementer | Opus 5.5 (Sonnet 5.5 to save usage) | Writes code test first in its own worktree |
| Explorer / test runner | Haiku 5.5 or Sonnet 5.5 | Reads code, runs gates, summarizes logs |
| Reviewers | CodeRabbit, Cursor's bots, the `reviewer` agent (Fable 5.2, fresh context), and me | Bots review the PR; the reviewer runs the gates, and reviews when CodeRabbit did not; I review the PR before merge |

If usage limits are not a concern, Opus 5.5 everywhere is fine. Treat cheaper routing as an experiment: if an implementer fails the gates twice, escalate it to Opus 5.5.

### Orchestrator rules
- Never edits code or docs directly.
- At most 3 implementers in parallel, each in its own worktree. I am the bottleneck for review.
- Stops and asks me before: any ADR conflict, new dependency, Core API change, or doc change.
- Runs `blast-radius` before any Core API change.
- Reads the implementer's Auto-fix reports, tells me what was fixed or skipped, and never merges.

### Build loop
1. Orchestrator picks an issue whose blockers are closed and writes the plan.
2. Implementer builds it in its own worktree, test first where testable, and commits there.
3. Gates pass (two failures: escalate model or re-plan).
4. `unslop` pass.
5. The implementer pushes its branch and opens the PR on its own, named by the rules in `CLAUDE.local.md`. Once the PR is open, before QA, the orchestrator runs the `reviewer` agent on it (`.claude/skills/reviewer/SKILL.md`):
   - Gates, on every PR while GitHub Actions is off: the six gates and CI's fixture and hook checks, on a clean checkout of the PR's head commit. A gate failure ends the run.
   - Review, only when CodeRabbit did not review the head commit: its notice for that commit is "Review limit reached", or it posted nothing 15 minutes after the push. The reviewer then runs Anthropic's `code-review` plugin for bugs and `CLAUDE.md` breaks, checks the diff against `CLAUDE.local.md` when it exists, and checks the change against its issue.
   - It posts from my GitHub login: a summary that names the commit reviewed, plus one thread per finding when it reviewed. When CodeRabbit reviewed, it posts only the gate results.
   - After the implementer's fixes to its threads, the orchestrator runs it once more as a recheck. The recheck replies on its own threads, resolves the fixed ones, and opens no new thread. Then it stops.
   - The gates run on macOS. CI ran on Ubuntu. That is the stricter check for the app and a weaker one for Linux build breaks.
   - GitHub Actions stopped on Nov 14, 2026, when the month's minutes ran out. `ci.yml` now runs only when started by hand.
   - CodeRabbit reviews the PR and every push. Cursor's Bugbot, Security Agent, and Approval Agent run on a weekly quota.
6. Right after opening the PR, the implementer session turns on the app's Auto-fix for it (`mcp__ccd_pr__set_monitor` with `auto_fix` and `address_comments`; I approve). Auto-fix wakes it on review comments and merge conflicts, and it handles them itself:
   - Review comments (the reviewer, CodeRabbit, Cursor, me): implement each finding by default. Check it against the code, fix it test first, and run the gates.
   - Skip a finding only if it is clearly wrong or out of scope, with the reason on the thread. Style-only nitpicks get a reply, not code.
   - Stop and ask before a Core API change, a new dependency, or a doc change.
   - Push and reply on every thread. Resolve the threads of CodeRabbit, Cursor and me. Leave the reviewer's threads open: its recheck resolves them. `main` requires every thread resolved before merge. List skipped findings in the report. At most two fix rounds.
   - A gate failure in the reviewer's summary: fix the root cause. No retries, sleeps, or disabled tests.
   - Merge conflict: sync with `main`. Never rebase or force-push.
   - Never use CodeRabbit's own autofix.
   - Report to the orchestrator after each event. After a fix round for the reviewer, ask the orchestrator for its recheck.
7. I review the PR. From M4 on, I do this inside grove.
8. Only I merge. No agent or orchestrator merges or turns on auto-merge, even when CI is green, the bots are clean, the PR is approved, or I said "merge in order".

### Reviewer checklist
- Does it do what the issue and plan say, nothing more?
- Edge cases from the plan covered by tests?
- Any ADR violated or GLOSSARY.md term misused?
- Core: no `unwrap()`, no blocking on tokio threads, errors typed.
- UI: no per-chunk events, no unbatched renders.
- Acceptance tests touched still pass?
- Anything that should become a lint rule or test?

## Phase 4: Verify
| When | Gates |
|---|---|
| Every commit | `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `tsc --noEmit`, `eslint` |
| Every PR | `cargo test` (includes diff parity), `vitest run` |
| Every milestone from M4 | Full `docs/acceptance.md`, including manual terminal tests and the 8-session performance run |

The pre-push hook in `.githooks/pre-push` runs the Rust and TS gates before every push and refuses the push when one fails. It lists the files the pushed commits change against `origin/main` and runs a gate group only when one of them is a path the group reads, by the lists in `.github/scripts/step_groups_to_run.py`. When git cannot list them, every group runs. The hook prints which groups ran and why. The hook and the reviewer's `run_gates.sh` take the same lock, so only one gate run goes at a time on this Mac. A waiting run says who holds the lock. The kernel frees it when its holder exits or dies. The Rust gates build in one folder for all hook runs, `~/.cache/grove-hook/target`, or in `GROVE_HOOK_TARGET_DIR` when it is set. It is not the reviewer's folder, so a hook never changes a build the reviewer is using. Every checkout gives the grove crates the same artifact names in that folder, and cargo judges them fresh by mtime. So when another checkout built there last, the hook runs `cargo clean --workspace` and rebuilds the grove crates from the pushing checkout. Dependencies stay built. Set the hook up once in the main worktree with `git config core.hooksPath .githooks`. Every worktree shares that setting, but git looks for the hook in each worktree's own checkout. A worktree on a branch without `.githooks/pre-push` pushes with no check until it syncs with `main`. The hook runs the gates on the working tree, not on the commits being pushed, so an uncommitted fix can let the hook pass while the pushed commit fails CI.

A ruleset protects `main`: changes land only by PR, every review thread must be resolved, the branch must be up to date with `main`, and no force push. The `rust` and `ts` CI checks were removed from the ruleset on Nov 15, 2026 while GitHub Actions is off, and go back in when Actions runs again. Until then the `reviewer` runs the same gates on the PR's head commit.

CI retries a failed Rust test twice. One that then passes is listed as flaky in the `rust` job summary and does not fail the run. Each flaky test gets its own issue with the cause. The retry is not the fix.

## Feedback loop
**Rule promotion ladder:**
1. A review finding seen twice becomes a rule in `CLAUDE.md`.
2. A rule that a machine can check becomes a clippy setting, ESLint rule, or test.
3. Rules that stop firing for a month get deleted.

**Knowledge gate:** agents never write to docs, `GLOSSARY.md`, or `CLAUDE.md` on their own. They propose changes inside a PR and I approve them.

**Per-PR metrics** (PR template): gate failures before green, review findings by category, retries, models used. These show whether the routing works.

**Weekly retro (30 min, Sunday):**
- Tally review findings and promote rules.
- Apply a scope cut early if behind.

## Dogfooding
From M4, run the implementers inside grove itself. Every friction point becomes an issue labeled `dogfood`. M7 is the dogfood week.
