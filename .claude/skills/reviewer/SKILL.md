---
name: reviewer
description: Check a grove pull request on this Mac. Always reads the PR's `rust` and `ts` CI checks on its head commit and reports them. When CodeRabbit did not review that commit, also runs Anthropic's code-review plugin and a spec check against the issue, and posts one thread per finding with gh. Use for "review PR N", "recheck PR N", or a reviewer dry run.
---

# Reviewer

You check one pull request and post what you find. You read and report: the code, the merge and every thread you did not open belong to others.

## Input

- **PR number.** The repo is `aaf1007/grove-ade`.
- **Mode:** `review` (the default) or `recheck`.
- **Flags:** `--dry-run` does every step and prints what it would post. It posts nothing. `--force-review` runs the full review whatever CodeRabbit did.

Scripts below live in this skill's folder, written `<skill>/`. Run them from a checkout of grove. Keep your JSON files in your scratchpad.

## 0. Which parts run

A `recheck` skips this step: it reads the CI checks (step 1), then does step 5.

CodeRabbit is the first reviewer. The CI checks are always read and reported. The review (steps 2 and 3, and threads in step 4) runs only when CodeRabbit did not review the head commit:

```bash
python3 <skill>/post_review.py coderabbit aaf1007/grove-ade <number>
```

- `gates-only`: CodeRabbit reviewed the head commit. Do steps 1 and 4, with no findings.
- `full-review`: its notice for the head commit is "Review limit reached", or it said nothing for 15 minutes after the push. Do every step.
- `wait`: CodeRabbit may still review. Read the CI checks (step 1), report them, and say when to ask again. Do no other step.

The first line is the answer and the full sha of the head commit it is about, such as `gates-only 54f5930...`. The second line says why. Keep it: steps 1 and 4 use that commit, so the checks and the post match the decision.

With `--force-review`, skip this check and do every step. Say so in the summary.

For a full review, check the plugin first. The bug and rules pass is the skill `code-review:code-review`. If it is not in your skill list, stop at once and print:

```
The code-review plugin is missing. Install it, then run the reviewer again:
/plugin install code-review@claude-plugins-official
```

The review is the plugin's. Without it, there is no review.

## 1. CI checks on the head commit

The `rust` and `ts` jobs of `.github/workflows/ci.yml` run on every PR. A PR run skips the gate steps whose paths it did not change, so a pass can mean a step was skipped. The step `Find the step groups this run needs` in each job's log says which groups ran. Read the result for the head commit:

```bash
pr=<number>
commit=<the sha step 0 printed>
head=$(gh pr view "$pr" --repo aaf1007/grove-ade --json headRefOid -q .headRefOid)
if [ "$head" != "$commit" ]; then echo "Head moved to $head."; else
  gh pr checks "$pr" --repo aaf1007/grove-ade --json name,state,bucket,link \
    -q '.[] | select(.name == "rust" or .name == "ts") | [.name, .bucket, .link] | @tsv' | grep . ||
    echo "No rust or ts check: CI did not run on the PR."; fi
```

A recheck, or a run with `--force-review`, skips step 0: set `commit=$(gh pr view "$pr" --repo aaf1007/grove-ade --json headRefOid -q .headRefOid)` instead.

When the head is no longer `$commit`, start again from step 0.

- `pass` for both: report them as passed.
- `pending`: the checks are still running. On a full review, do steps 2 and 3 meanwhile. Then read them again. The jobs time out after 15 and 10 minutes, so when one is still pending 20 minutes after the push, report that and stop.
- `cancel` with no failed step: report the cancellation with the job link.
- `fail`, or `cancel` after a step failed: find the failing step. The link ends in `/actions/runs/<run>/job/<job>`:

  ```bash
  gh run view <run> --repo aaf1007/grove-ade --json jobs \
    -q '.jobs[] | .name as $job | .steps[] | select(.conclusion == "failure") | "\($job): \(.name)"'
  # A failed job:
  gh run view --job <job> --repo aaf1007/grove-ade --log-failed | tail -n 40
  # A cancelled job: --log-failed skips it, so keep the failing step's lines of the full log.
  gh run view --job <job> --repo aaf1007/grove-ade --log | awk -F '\t' -v step="<failing step>" '$2 == step' | tail -n 40
  ```

- `rust` or `ts` missing from the list: CI did not run on the PR. Report it and stop.

A PR that changes `.github/` changes its own checks. Say so in "For the owner".

Any failed check ends the review: post the summary alone, even on a full review, (step 4) with each check's result, the failing step and the last lines of its log. The implementer fixes it and asks again. A recheck goes from step 1 straight to step 5, and reports a failed check in its recheck summary.

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
- The summary starts with which parts ran and why: the `coderabbit` line, or `--force-review`. Then a table of the `rust` and `ts` checks with their links. On a gates-only run, that is all. A full review adds the plugin's result (finding count, "No issues found", or why it stopped), and the spec check table. Add "For the owner" when you have items for it.
- Plain short sentences, no em dashes, no signature or attribution line.

```bash
python3 <skill>/post_review.py post aaf1007/grove-ade <number> review.json [--dry-run]
```

It posts one review: your summary as its body, one thread per finding, and "Commit reviewed: <sha>" at the end. It refuses when the PR's head moved past the commit you reviewed. Then start again from step 0, since CodeRabbit may review the new head.

## 5. Recheck, once

The recheck follows a full review. After a gates-only run, a new push gets a fresh `review`.

When the implementer has pushed fixes:

1. Read the CI checks on the new head (step 1).
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

Tell whoever ran you: the CI check results, the threads you posted with their links, the spec check, and the "For the owner" list. On a dry run, paste what the script printed. Name work as "issue #N (PR #M)".
