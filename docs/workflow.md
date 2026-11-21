# Workflow: validate, plan, build, review, improve

Tools: Matt Pocock's skills for planning, pstack for execution and review, and an orchestrator with role-based subagents. The Roles table gives the model for each role.

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
| PR that touches PTY or acked streams, the socket protocol, agent state, or the Core API | pstack `interrogate` | Several models review the diff adversarially. The orchestrator runs it after the PR is open, before QA, and posts the "Act on" items on the PR as review comments for the implementer to fix. The rest are dropped |
| PR when CodeRabbit hit its limit | pstack `thermos` | Optional second pass beside the reviewer. Not by default, for cost. The orchestrator runs it only when the owner asks. Its findings go to the owner first, and the owner says which the implementer fixes |
| Before opening the PR | `verify` | The implementer runs its own "How to test it" steps through `verify`, on the headless surface. A failed step is fixed before the PR opens. Anything it cannot reach is listed in the PR as not run |
| PR, before merge-ready | `qa` on `verify` | The orchestrator gives the branch to the `qa` agent. QA in the real app. A fail goes back to the implementer with repro steps, per step 7 of the Build loop |
| Milestone close, and risky PRs | pstack `swarm` on `verify` | Break-it gauntlet: one worker per feature file, told to break it. The orchestrator runs it. Each proven defect becomes an issue, and nothing else is reported |
| Weekly, before the Sunday retro | pstack `maintain-verification-skill` | Keeps the feature map true to the app. The orchestrator runs it. At most one PR of fixes, which the owner approves. Product bugs become issues |
| Sunday retro | pstack `correct` | Runs the rule promotion ladder. The orchestrator runs it. Rule changes are proposed in a PR for the owner's approval, never written directly |

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
| Role | Model | Notes |
|---|---|---|
| Orchestrator | The model the owner starts the session on | This doc names no model |
| Implementer | Opus 5.5, high | The default. A linked session with fresh context in its own worktree |
| Implementer, hard issue | Fable 5.2 high, or Opus 5.5 xhigh | Core API, concurrency, PTY. Also any issue that failed the gates twice |
| Implementer, small issue | Sonnet 5.5, medium | Docs, renames, one-file fixes. The orchestrator names the tier in the brief |
| Reviewer | Sonnet 5.5, high | A different model from the implementer. CodeRabbit, Cursor's bots and I also review the PR. See the reviewer bullet in the Build loop |
| DevOps | Sonnet 5.5, high | Moves to Opus 5.5 when the cause is still unclear after one round |
| QA | Sonnet 5.5, medium | See the QA step in the Build loop |
| Lookups: search, log summaries, name checks | Haiku 5.5 | |

### Orchestrator rules
- Never edits code or docs directly.
- Before relaying an implementer's report, checks the PR, its diff and its checks.
- At most 3 implementers in parallel, each in its own worktree. I am the bottleneck for review.
- Stops and asks me before: any ADR conflict, new dependency, Core API change, or doc change.
- Runs `blast-radius` before any Core API change.
- Reads the implementer's Auto-fix reports, tells me what was fixed or skipped, and never merges.

### Build loop
1. Orchestrator picks an issue whose blockers are closed and writes the plan.
2. Implementer builds it in its own worktree, test first where testable, and commits there.
3. Gates pass (two failures: escalate model or re-plan).
4. `unslop` pass.
5. Before the PR, the implementer proves its change through `verify` on the headless surface, using the "How to test it" steps it will put in the PR. Step 7 allows one app run at a time, so the app surface is left to QA. The implementer then pushes its branch and opens the PR on its own, named `#<issue> <type>: <what it does>` on a branch `<type>-<issue>-<slug>` (see the README). Once the PR is open, before QA, the orchestrator runs the `reviewer` agent on it (`.claude/skills/reviewer/SKILL.md`):
   - CI checks, on every PR: the reviewer reads the `rust` and `ts` checks on the PR's head commit, waits while they run, and reports a failure with the failing step. A failed check ends the run.
   - Review, only when CodeRabbit did not review the head commit: its notice for that commit is "Review limit reached", or it posted nothing 15 minutes after the push. The reviewer then runs Anthropic's `code-review` plugin for bugs and `CLAUDE.md` breaks, checks the diff against the owner's private rules in `CLAUDE.local.md` when that file exists, and checks the change against its issue.
   - It posts from my GitHub login: a summary that names the commit reviewed, plus one thread per finding when it reviewed. When CodeRabbit reviewed, it posts only the check results.
   - After the implementer's fixes to its threads, the orchestrator runs it once more as a recheck. The recheck replies on its own threads, resolves the fixed ones, and opens no new thread. Then it stops.
   - CodeRabbit reviews the PR and every push. Cursor's Bugbot, Security Agent, and Approval Agent run on a weekly quota.
   - Full or light: the first commit of a PR that CodeRabbit did not review gets the full review above. A later fix round gets a light review: one agent reads only the diff since the last reviewed commit, checks it against the open threads and the issue, and posts threads for what it finds. It starts no plugin agents.
   - `interrogate`: on a PR that touches PTY or acked streams, the socket protocol, agent state, or the Core API, the orchestrator also runs pstack `interrogate`, before QA. It posts the "Act on" items on the PR as review comments. Step 6 covers them like any other review comment, with the same two-round cap.
   - Gauntlet on a PR: the orchestrator runs `swarm` on a PR only when the owner asks for it on that PR, after QA passes. Otherwise it runs at milestone close (Phase 4).
6. Right after opening the PR, the implementer session turns on the app's Auto-fix for it (`mcp__ccd_pr__set_monitor` with `auto_fix` and `address_comments`; I approve). Auto-fix wakes it on review comments and merge conflicts, and it handles them itself:
   - Review comments (the reviewer, `interrogate`, CodeRabbit, Cursor, me): implement each finding by default. Check it against the code, fix it test first, and run the gates.
   - Skip a finding only if it is clearly wrong or out of scope, with the reason on the thread. Style-only nitpicks get a reply, not code.
   - Stop and ask before a Core API change, a new dependency, or a doc change.
   - Push and reply on every thread. Resolve the threads of CodeRabbit, Cursor and me. Leave the reviewer's threads open: its recheck resolves them. `main` requires every thread resolved before merge. List skipped findings in the report. At most two fix rounds.
   - A failed check in the reviewer's summary: fix the root cause. No retries, sleeps, or disabled tests.
   - Merge conflict: sync with `main`. Never rebase or force-push.
   - Never use CodeRabbit's own autofix.
   - Report to the orchestrator after each event. After a fix round for the reviewer, ask the orchestrator for its recheck.
7. QA, after the PR is open and CI is green, when the issue changes something visible in the app. Core-only, CI and docs issues skip it, and the report says so.
   - The orchestrator gives the `qa` agent the branch, the worktree path, the "How to test it" steps from the implementer's report, and the acceptance tests the issue names.
   - Headless first: every step the fake agents can script runs on `verify`'s headless surface, with no screenshots and no real agents. The app surface is only for what a user sees: layout, focus, keys, panels, notices.
   - One app run at a time, because only one grove can run on the machine. Each QA run starts with a fresh context.
   - Screenshots only at each pass or fail point, zoomed to the region being checked.
   - A fail goes back to that issue's implementer session with the repro steps. The implementer fixes it test first, then QA re-runs only the failed tests. After two rounds it goes to the owner.
8. DevOps: implementers fix CI failures their own diff caused. The `devops` agent diagnoses and fixes a failure on `main`, one that hits several PRs, or one that looks like the runner, a cache, apt, or a flaky test. It fixes in its own worktree, one issue and one PR per fix, by this loop.
9. I review the PR. From M4 on, I do this inside grove.
10. Only I merge. No agent or orchestrator merges or turns on auto-merge, even when CI is green, the bots are clean, the PR is approved, or I said "merge in order".

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
| Every milestone | The gauntlet: the orchestrator runs pstack `swarm` on `verify`, next to the `acceptance.md` run. Each proven defect becomes an issue |

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
- The orchestrator runs pstack `maintain-verification-skill` first. It keeps the feature map true to the app, opens at most one PR of fixes, and files product bugs as issues.
- Read the eval tables in `evals/`.
- The orchestrator runs pstack `correct`. It keeps the rule enforcement table in `CLAUDE.md` in step and proposes rule changes. The ladder above is grove's, so `correct` does not replace it. It proposes changes in a PR for my approval.
- Tally review findings and promote rules.
- Apply a scope cut early if behind.

## Dogfooding
From M4, run the implementers inside grove itself. Every friction point becomes an issue labeled `dogfood`. M7 is the dogfood week.
