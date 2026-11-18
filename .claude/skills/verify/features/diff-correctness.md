# Diff correctness

The Diff panel shows what changed in a worktree, the same way `git diff` would, against `HEAD` or against the branch's base. It updates on its own when a file is saved, staged or committed, by the user or by an agent.

## Sub-features

- `parity` (cargo test, headless): the structured diff matches `git diff HEAD` on `fixtures/diff-repo`. Automated in `cargo test -p grove-core diff`.
- `untracked` (headless; app not mapped yet): an untracked file shows as a whole-file addition, as `git diff --no-index /dev/null <file>`.
- `branch-base` (headless; app not mapped yet): the base diff matches `git diff $(git merge-base origin/main HEAD)`, uncommitted changes included.
- `commit-liveness` (headless; app not mapped yet): after `git add` or `git commit` the view updates within 500 ms.
- `liveness` (app, headless): the view updates within 500 ms of a file save.

## How to get to it (user POV)

- Select a worktree, then choose the **Diff** tab in the **Panels** tab list, or **Open the Diff panel** from the details card.
- The **Diff base** control switches between uncommitted changes and the branch base.
- **Changed files** lists the files. Selecting one shows its diff.

## Driving it with app_* and headless.py

Preconditions:

- App: Doctor passed and the test repo added through **Repo path** and **Add repo**. For parity and branch base, build `fixtures/diff-repo` into `<run>/home/diff` and add `<run>/home/diff/repo`.
- Headless: the baseline run. For parity and branch base, run `python3 fixtures/diff-repo/build.py <run>/home/diff` and `headless.py call <run> add_repo path=<run>/home/diff/repo`.

- **Open the panel, app.** Add `<run>/home/repo` through **Repo path** and **Add repo**, `app_click` **main (main worktree)**, then the **Diff** tab (`app_ax_find` title `Diff`, an `AXRadioButton`). The panel shows **Diff base** with **HEAD** picked and no files for a clean repo.
- **Liveness, app.** Save the before state with `screencapture`, then change a file from the shell: `echo changed >> <run>/home/repo/README.md`. About a second later, capture again. The changed-files row shows `README.md +1 -0`, and `git -C <run>/home/repo status --short` lists `README.md` as modified (`M`). A capture a second later cannot time the update, so report the 500 ms bound "observed, not measured" in the app; the headless recipe below can measure it.
- **Liveness, headless.** `echo changed >> <run>/home/repo/README.md`, then `headless.py wait <run> events.log 'DiffChanged \{ worktree: "<run>/home/repo"'`. `headless.py call <run> diff worktree=<run>/home/repo base=head` lists `README.md` as `Modified` with the line `+changed`. The 500 ms bound is the gap between the file's mtime (`stat -f %Fm`) and the `DiffChanged` timestamp, where the `DiffChanged` time is the `wall=` Unix time on `events.log`'s `ready` line plus that line's seconds since start. Report the gap in ms.
- **Commit liveness, headless.** `headless.py settle <run> events.log` so every `DiffChanged` from the save is in, then note `wc -l < <run>/evidence/events.log` as `L`, `git -C <run>/home/repo add README.md`, and `headless.py wait <run> events.log 'DiffChanged' --after L`. Settle and note `L` again, `git -C <run>/home/repo commit -qm next`, and wait the same way. After the commit, `diff base=head` returns `[]`.
- **Untracked, headless.** `settle`, note `wc -l < <run>/evidence/events.log` as `L`, `printf 'x\ny\n' > <run>/home/repo/new.txt`, `headless.py wait <run> events.log 'DiffChanged' --after L`, then `diff base=head`. `new.txt` is `Untracked` with lines `+x` and `+y`. Compare with `git -C <run>/home/repo diff --no-index /dev/null new.txt`.
- **Branch base, headless.** `headless.py call <run> diff worktree=<run>/home/diff/repo base=merge_base`, then compare each file's status, paths and hunk ranges with `git -C <run>/home/diff/repo diff $(git -C <run>/home/diff/repo merge-base origin/main HEAD)` run with the environment in `fixtures/diff-repo/README.md`.
- **Parity.** `cargo test -p grove-core diff` passes. Headless adds nothing to it except a manual spot check with `diff base=head` on `<run>/home/diff/repo`.
- **Proof.** App: `screencapture -x -o -l <window_id> <run>/evidence/diff-liveness.png` showing **Changed files** and the added line. Headless: the `DiffChanged` line and the `diff` reply.

## Gotchas

- The test repo from `headless.py start` has only `main`, so `merge_base` there equals `head`. Use the diff fixture for branch base.
- Headless `diff` prints lines with their trailing newline inside the string. Compare text, not byte counts.
- One save can produce more than one `DiffChanged`. Wait with `--after` for the first after the change, then read the diff.
- In the app, the Diff panel only refreshes while its worktree is selected. Select it before changing files.
- On 2026-11-16, on `main`, the app's diff body under the changed-files row stayed blank in background captures, even after clicking the file; only the overview ruler showed the added line. Cause unknown: it may be Monaco not painting while the window is in the background. Report what this run's capture shows: if the hunk text is visible, report it seen; if it is blank, report it "not seen" in the app. A headless `diff` checks the core's hunk, never what the app drew.
- Git commands for the comparison must run with `GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1`, or the owner's git config changes the expected output.
