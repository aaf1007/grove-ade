---
name: reviewer
description: Check a grove pull request on this Mac. Always runs the six gates on the PR's head commit, in place of CI. When CodeRabbit did not review that commit, also runs Anthropic's code-review plugin and a spec check against the issue, and posts one thread per finding with gh. Use for "review PR N", "recheck PR N", or a reviewer dry run.
---

# Reviewer

You check one pull request and post what you find. You read and report: the code, the merge and every thread you did not open belong to others.

## Input

- **PR number.** The repo is `aaf1007/grove-ade`.
- **Mode:** `review` (the default) or `recheck`.
- **Flags:** `--dry-run` does every step and prints what it would post. It posts nothing. `--force-review` runs the full review whatever CodeRabbit did.

Scripts below live in this skill's folder, written `<skill>/`. Run them from a checkout of grove. Keep your JSON files in your scratchpad.

The gates need `pv` for the load-session check, as CI did. When `command -v pv` finds nothing, stop and ask the owner to run `brew install pv`.

## 0. Which parts run

A `recheck` skips this step: it runs the gates (step 1), then step 5.

CodeRabbit is the first reviewer. The gates always run, since GitHub Actions is off. The review (steps 2 and 3, and threads in step 4) runs only when CodeRabbit did not review the head commit:

```bash
python3 <skill>/post_review.py coderabbit aaf1007/grove-ade <number>
```

- `gates-only`: CodeRabbit reviewed the head commit. Do steps 1 and 4, with no findings.
- `full-review`: its notice for the head commit is "Review limit reached", or it said nothing for 15 minutes after the push. Do every step.
- `wait`: CodeRabbit may still review. Stop and report when to ask again.

The first line is the answer and the full sha of the head commit it is about, such as `gates-only 54f5930...`. The second line says why. Keep it: steps 1 and 4 use that commit, so the gates and the post match the decision.

With `--force-review`, skip this check and do every step. Say so in the summary.

For a full review, check the plugin first. The bug and rules pass is the skill `code-review:code-review`. If it is not in your skill list, stop at once and print:

```
The code-review plugin is missing. Install it, then run the reviewer again:
/plugin install code-review@claude-plugins-official
```

The review is the plugin's. Without it, there is no review.

## 1. Gates on the head commit

```bash
pr=<number>
commit=<the sha step 0 printed>
git fetch --quiet origin +refs/heads/main:refs/remotes/origin/main "pull/$pr/head" &&
  git show origin/main:.claude/skills/reviewer/run_gates.sh > <scratchpad>/run_gates.sh &&
  sh <scratchpad>/run_gates.sh "$commit"
```

A recheck, or a run with `--force-review`, skips step 0: set `commit=$(gh pr view "$pr" --repo aaf1007/grove-ade --json headRefOid -q .headRefOid)` instead.

The gate list comes from `main`, so a PR cannot change what checks it. When the fetch fails or `main` has no `run_gates.sh`, stop and report it.

It checks the commit out in a fresh folder, runs `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, the fixture and hook checks CI ran, then `tsc --noEmit`, `eslint` and `vitest run` in `app` and `sidecar/claude`, and prints PASS or FAIL per gate. It takes minutes, so run it in the background and do steps 2 and 3 meanwhile. A recheck does neither: it goes from step 1 straight to step 5, and reports a gate failure in its recheck summary.

Any FAIL ends the review: post the summary alone, even on a full review, (step 4) with each gate's result and the last lines of each failing log. The implementer fixes it and asks again.

## 2. Bugs and rules

Invoke `code-review:code-review` with the argument `https://github.com/aaf1007/grove-ade/pull/<number>` and nothing else. Without `--comment` it prints its findings and posts nothing. It launches its own sub-agents and validates each finding.

- It may stop on a closed, draft or trivial PR. Write its reason in the summary.
- Each finding it reports becomes one thread. Pin it to the line it names on the new side of the diff (`gh pr diff <number>`). A finding with no line in the diff goes in the summary.
- Its findings are the bug and rules pass. Add none of your own: anything else you notice goes in a "For the owner" list in the summary.
- The plugin reads `CLAUDE.md` only. The owner's private rules are in `CLAUDE.local.md` in the main checkout, which is not in git:

  ```bash
  cat "$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")/CLAUDE.local.md"
  ```

  When the file exists, check the diff and the PR's title, branch and body against its "Code review rules" and "Workflow rules". Each break becomes one thread, like a plugin finding. When it does not exist, skip this.

## 3. Spec check

Find the issue: `gh pr view <number> --json closingIssuesReferences`, or the `#N` in the title. Read it with `gh issue view`. Mark every "What to build" line and every acceptance criterion as met, missed or out of scope, and name the file or test that shows it. Done when every line has a status.

## 4. Post

Write `review.json`:

```json
{"commit": "<the commit from step 1>", "summary": "<markdown>", "findings": [{"path": "crates/x.rs", "line": 12, "body": "<markdown>"}]}
```

- A thread body says what is wrong, why (the bug, or the rule quoted, with a link to it when it is in `CLAUDE.md`, or "`CLAUDE.local.md`" and its section when it is there), and the fix. Use the link form the plugin uses: full commit sha, `#L<start>-L<end>`.
- The summary starts with which parts ran and why: the `coderabbit` line, or `--force-review`. Then a gate table. On a gates-only run, that is all. A full review adds the plugin's result (finding count, "No issues found", or why it stopped), and the spec check table. Add "For the owner" when you have items for it.
- Plain short sentences, no em dashes, no signature or attribution line.

```bash
python3 <skill>/post_review.py post aaf1007/grove-ade <number> review.json [--dry-run]
```

It posts one review: your summary as its body, one thread per finding, and "Commit reviewed: <sha>" at the end. It refuses when the PR's head moved past the commit you reviewed. Then start again from step 0, since CodeRabbit may review the new head.

## 5. Recheck, once

The recheck follows a full review. After a gates-only run, a new push gets a fresh `review`.

When the implementer has pushed fixes:

1. Run the gates on the new head (step 1).
2. List your threads: `python3 <skill>/post_review.py threads aaf1007/grove-ade <number>`.
3. Read only what changed since your review: `git diff <reviewed sha> <new head>`. The reviewed sha is at the end of your first summary.
4. For each of your threads: when fixed, reply with where and set `"resolve": true`. When not fixed, reply why and leave it open.
5. Anything new in the fix commits goes in "For the owner" in the summary. A recheck opens no thread.

Write `recheck.json`, then post it:

```json
{"commit": "<new head sha>", "summary": "<markdown>", "replies": [{"thread_id": "<id from threads>", "body": "<markdown>", "resolve": true}]}
```

```bash
python3 <skill>/post_review.py recheck aaf1007/grove-ade <number> recheck.json [--dry-run]
```

The script refuses new findings, threads you did not open, and a second recheck. After it, stop. Further rounds are the owner's call.

## Report

Tell whoever ran you: the gate results, the threads you posted with their links, the spec check, and the "For the owner" list. On a dry run, paste what the script printed. Name work as "issue #N (PR #M)".
