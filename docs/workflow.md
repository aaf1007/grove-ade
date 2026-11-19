# Workflow: validate, plan, build, review, improve

Tools: Matt Pocock's skills for planning, pstack for execution and review, and an orchestrator with role-based subagents on Claude Opus 5.5.

Skill names below may differ slightly between versions. Check your `/` menu and map them.

## Which skill stack does what
| Stage | Use | Why |
|---|---|---|
| Repo setup | `setup-matt-pocock-skills` | Points skills at the issue tracker, labels, and docs |
| Design grilling | `grill-with-docs` | Stress-tests the plan and updates GLOSSARY.md and ADRs as decisions land |
| Architecture attack | pstack interrogate / architect panel | Several reviewers try to break `architecture.md` |
| Spec | `to-spec` | MVP scope as one document |
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
2. `grill-with-docs` on `vision.md` and `architecture.md` until no open questions remain for the MVP.
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
| Reviewers | CodeRabbit, Cursor's bots, the `reviewer` agent (Fable 5.2, fresh context), and me | Bots review the PR; the reviewer reports the CI checks, and reviews when CodeRabbit did not; I review the PR before merge |

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
5. The implementer pushes its branch and opens the PR on its own, named `#<issue> <type>: <what it does>` on a branch `<type>-<issue>-<slug>` (see the README). Once the PR is open, before QA, the orchestrator runs the `reviewer` agent on it (`.claude/skills/reviewer/SKILL.md`):
   - CI checks, on every PR: the reviewer reads the `rust` and `ts` checks on the PR's head commit, waits while they run, and reports a failure with the failing step. A failed check ends the run.
   - Review, only when CodeRabbit did not review the head commit: its notice for that commit is "Review limit reached", or it posted nothing 15 minutes after the push. The reviewer then runs Anthropic's `code-review` plugin for bugs and `CLAUDE.md` breaks, checks the diff against the owner's private rules in `CLAUDE.local.md` when that file exists, and checks the change against its issue.
   - It posts from my GitHub login: a summary that names the commit reviewed, plus one thread per finding when it reviewed. When CodeRabbit reviewed, it posts only the check results.
   - After the implementer's fixes to its threads, the orchestrator runs it once more as a recheck. The recheck replies on its own threads, resolves the fixed ones, and opens no new thread. Then it stops.
   - CodeRabbit reviews the PR and every push. Cursor's Bugbot, Security Agent, and Approval Agent run on a weekly quota.
6. Right after opening the PR, the implementer session turns on the app's Auto-fix for it (`mcp__ccd_pr__set_monitor` with `auto_fix` and `address_comments`; I approve). Auto-fix wakes it on review comments and merge conflicts, and it handles them itself:
   - Review comments (the reviewer, CodeRabbit, Cursor, me): implement each finding by default. Check it against the code, fix it test first, and run the gates.
   - Skip a finding only if it is clearly wrong or out of scope, with the reason on the thread. Style-only nitpicks get a reply, not code.
   - Stop and ask before a Core API change, a new dependency, or a doc change.
   - Push and reply on every thread. Resolve the threads of CodeRabbit, Cursor and me. Leave the reviewer's threads open: its recheck resolves them. `main` requires every thread resolved before merge. List skipped findings in the report. At most two fix rounds.
   - A failed check in the reviewer's summary: fix the root cause. No retries, sleeps, or disabled tests.
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
| Every milestone | Full `docs/acceptance.md`, including manual terminal tests and the 8-session performance run |

A ruleset protects `main`: changes land only by PR, every review thread must be resolved, the branch must be up to date with `main`, no force push, and the `rust` and `ts` CI checks must pass. CI runs on every PR and every push to `main`.

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
