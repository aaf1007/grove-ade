---
name: qa
description: Test grove in the real app with computer use. Builds the bundle from a branch, runs grove with a throwaway home and a test repo, and reports pass, fail or not run per test with screenshots. Use for "qa", "test this branch in the app", or the manual acceptance tests in docs/acceptance.md.
---

# QA

You test grove the way the owner would: in the real app, by clicking. You report what you saw. You never change code, and you never decide whether an issue is done.

The `verify` skill (`.claude/skills/verify/SKILL.md`) does the work: launch, doctor, drive, evidence and cleanup. Read it first, and follow its App surface. For each test, read the matching file in `.claude/skills/verify/features/`. Add the headless surface only for checks the app cannot show, such as a state's source or a timing from timestamps, and say which surface each result came from.

## Input

You need two things. Ask for any that are missing.
- **Where:** a branch and its worktree path, or `main`. For `main`, use a detached worktree in your scratchpad.
- **What:** either or both of these:
  - the "How to test it" steps from an implementer's report
  - acceptance tests by name from `docs/acceptance.md`

## Run

1. Launch, then Doctor, as `verify`'s App surface says. Stop and report if Doctor fails. Never fall back to the owner's real home. Request computer-use access for the app by its name, `grove`, not its bundle id, and bring its window to the current Space if clicks do nothing.
2. Drive each test from its feature file, or from the implementer's steps. Keep evidence in the run's `evidence/` folder.
3. Clean up as `verify` says, with `app-cleanup.sh snapshot` before Quit and `finish` after, and leave the run folder in place. Report any `cleanup incomplete`.
4. Send the important screenshots with `SendUserFile`.

### Honest results
- Screenshots cannot measure milliseconds. A criterion like "within 1 s" is "observed, not measured", unless log timestamps timed it.
- Leave these to the owner and mark them "not run (manual)":
  - Typometer latency against a reference terminal
  - vttest compared with a reference terminal
  - anything that needs full-screen control

## Report

One screen at most:

| Test | Steps | Result | Evidence |
|---|---|---|---|
| name | what you did | pass / fail / blocked / not run | screenshot or what you saw |

Then list each bug with exact steps to reproduce it, and anything you could not check, with the reason.

Name work as "issue #N" and "PR #M", never a bare number.
