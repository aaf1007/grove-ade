# Evals

Two evals tell whether a review stage catches bugs the cheaper stages miss. The Sunday retro reads both tables before it runs `correct`. Both need Python 3.9 or later and `gh`, and nothing outside the standard library. They only read from GitHub.

## Bug replay

`replay/` puts past bugs back and runs one stage at a time over them. A case is a fix commit in `replay/cases.json`. For each case:

- The stage reviews a fresh repo with two commits: the branch's base on `main`, and the branch just before the fix. The issue text is its input.
- The fix's test is the answer key. The stage never sees it or the fix commit. Trees that hold them, for the proof and the gates, are deleted as soon as that step ends, and the stage may not read the ones other cases are using, nor any other case's folder in the run, since an earlier case's fix is often in a later case's tree. A stage whose transcript names another case's folder, or whose tools return a line the fix adds, is marked leaked. The stage may run `git diff main...HEAD` but no other `git diff`, since `git diff` with two paths prints any file on disk. A judge model reads the fix and the stage's findings and says whether a finding names the same bug.
- The gates count as caught when a gate fails on the branch just before the fix and passes on the fix commit. A gate fails only when it fails three runs in a row, because some older timing tests fail under load. Gate commands from parallel cases take turns.

A case must prove itself: its test fails with the fix left out and passes with it. Add the case to `replay/cases.json`, then check it. An unknown sha is an error:

```
python3 evals/replay/replay.py proof --cases <sha>
```

A full run writes `results/<date>.md`, with a comparison to the last run, and `results/<date>.json`. Totals leave out leaked stages, stages that errored, and cases whose proof failed:

```
python3 evals/replay/replay.py run --stage gates --stage code-review --stage interrogate=<sha>,<sha> --jobs 3
```

`--stage NAME=SHA,...` runs that stage on those cases only, and a sha that matches none of the selected cases is an error. Stages: `gates`, `code-review`, `interrogate`, `thermos`, and `verify-gauntlet`, which says "not available" until issue #265 lands. Model stages need the `claude` CLI logged in, with the `code-review` and `pstack` plugins installed and its `--restricted` mode (tested with 2.1.327). A stage session reads no user, project or local settings file, so no allow rule of yours applies to it (managed settings still do), and its file tools stay inside the stage repo and the plugins' folders. Its commands run in Claude Code's sandbox, which may not write to the stage repo or any case's folder, nor read another case's folder, the grove checkouts, the build cache or Claude's session logs. Scratch folders go in the temp folder and are removed after each case. Compiled Rust and `node_modules` are kept in `~/.cache/grove-evals`.

## Live funnel

`funnel/` reads merged pull requests and `bug` or `dogfood` issues and tags each real defect with the stage that caught it:

```
python3 evals/funnel/funnel.py --since 2026-10-16 --until 2026-11-16
```

It writes `results/funnel-<until>.md`: a weekly table of defects per stage, what reached the owner, fix rounds per pull request and cost where the template gives it. The module docstring says where each stage's count comes from. Limits:

- A review thread counts when its first reply says it was fixed or real. Threads with an unclear reply are listed, not counted.
- CodeRabbit's outside-diff and nitpick comments live in its review body, not in threads, and are not counted.
- A pull request with more than one page of threads, reviews, commits or comments is listed as not counted.
- Implementers post from the owner's login. A thread that opens by saying what changed is taken as theirs, not the owner's.
- Stages that run before the pull request (interrogate, thermos, qa, verify, gauntlet) show up only through the "Caught by" column of `.github/pull_request_template.md`. The funnel reads only `bug` rows for those five stages, so a finding is never counted twice.

## Tests

```
python3 -m unittest discover evals/replay
python3 -m unittest discover evals/funnel
```
