# Diff fixture repo

`build.py` makes a git repo with every case the diff acceptance tests need: Parity, Untracked and Branch base in `docs/acceptance.md`, plus the extra cases ADR 0012 calls out. The repo itself is not committed. Build it when you need it.

## Build

```sh
python3 fixtures/diff-repo/build.py <dir>
```

`<dir>` must be missing or empty. The script writes:

- `<dir>/repo`: the fixture, on branch `feature`.
- `<dir>/origin.git`: its bare remote.

It prints the repo path. It needs git and Python 3.9 or later, nothing else. It ignores user and system git config and uses a fixed author, committer, dates and content, so every build has the same commit ids.

## Check

```sh
python3 fixtures/diff-repo/check.py
```

It builds the repo twice, checks both builds against the pinned commit ids, and checks that git shows every case below. It runs on macOS and Ubuntu, outside `cargo test`.

## Using it

Spike #8 and the parity test (#27) build the repo into a temp folder, then compare grove's diff with git run in `<dir>/repo` with this environment:

```sh
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 GIT_ATTR_NOSYSTEM=1 XDG_CONFIG_HOME=/dev/null
```

The first two are the ones ADR 0012 names. They do not cover the system attributes file or the user's `~/.config/git/attributes` and `~/.config/git/ignore`; the last two do.

| Grove diff | Compare with |
|---|---|
| `Head` | `git diff HEAD` |
| Each untracked file | `git diff --no-index /dev/null <file>` (exit status 1 means it differs) |
| `MergeBase(main)` | `git diff $(git merge-base origin/main HEAD)` |

`git ls-files --others --exclude-standard` lists the untracked files.

## Commits

```
A ── B ── C          origin/main (and origin/HEAD)
│    └── D ── E      feature, HEAD
main
```

- A: base files. Local `main` stays here, behind `origin/main`.
- B: adds `upstream.txt`. `feature` branches here.
- C: adds `upstream-later.txt`. Only on `origin/main`.
- D: adds `feature.txt`.
- E: edits `feature.txt` and `big.py`.

What the branch base cases test:

- `git merge-base origin/main HEAD` is B. Its diff shows `feature.txt` as added, the committed `big.py` edit, and the 21 uncommitted changes.
- `git merge-base main HEAD` is A, so that diff also adds `upstream.txt`. A core that ignores `origin/main` shows it.
- `git diff origin/main` shows `upstream-later.txt` as deleted. A core that diffs against the base branch tip instead of the merge base shows it.

## The 21 changes in `git diff HEAD`

| Path | Change | Tests |
|---|---|---|
| `added.txt` | new file, staged | add |
| `empty-added.txt` | new empty file, staged | add with no hunks |
| `deleted.txt` | removed from disk, not staged | delete |
| `renamed-old.txt` to `renamed-new.txt` | `git mv` | exact rename |
| `moved/before.txt` to `moved/after.txt` | `git mv`, then one line edited | rename with a hunk, similarity under 100 |
| `binary.dat` | bytes with NUL changed | binary flag |
| `control-chars.txt` | FIX messages split by SOH (0x01) bytes, one field changed | git calls it text since it has no NUL; libgit2's printable ratio check may call it binary |
| `crlf-convert.txt` | every line LF to CRLF | CRLF |
| `crlf-edit.txt` | one line changed in a CRLF file | `\r` kept in hunk lines |
| `whitespace.txt` | trailing spaces added, tab to spaces | whitespace-only change, no hunks under `-w` |
| `big.py` | 5,000 lines, four edits | hunk ranges deep in a file |
| `staged.txt` | edit, staged | staged change |
| `staged-unstaged.txt` | one edit staged, another not | `Head` combines index and worktree |
| `latin1.txt` | ISO-8859-1 text edited | non-UTF-8 text; git shows text hunks |
| `script.sh` | mode 100644 to 100755 | mode change with no hunks |
| `lfs/asset.bin` | git-lfs pointer changed; `.gitattributes` sets `filter=lfs diff=lfs` | a file grove must send to the git CLI (ADR 0012) |
| `no-eol.txt` | old version has no final newline, lines added | `\ No newline at end of file` |
| `emptied.txt` | all content removed | hunk to `+0,0` |
| `link` | symlink target changed | mode 120000 |
| `dir with space/naïve.txt` | one line added | quoted path with a space and non-ASCII |
| `slide.py` | a block inserted before code it repeats | the indent heuristic: with it, git puts the block first; without it, the hunk lines differ |

## Untracked files

| Path | Tests |
|---|---|
| `untracked.txt` | plain new file |
| `untracked-empty.txt` | empty file: `new file mode`, no hunks |
| `untracked.bin` | binary |
| `untracked-dir/nested.txt` | inside an untracked folder, no final newline |
| `build.log` | ignored by `.gitignore`, must not show |

## Changing the fixture

Edit `build.py`, add an assertion for the new case to `check.py`, and update the tables here. If the committed content changed, update `COMMIT_IDS` in `check.py`.
