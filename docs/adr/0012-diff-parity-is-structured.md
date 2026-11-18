# 0012: Diff parity means the same hunks under pinned config

- Status: Accepted (Oct 2026). Supersedes part of 0004 (what the parity test compares).
- Context: libgit2 cannot match `git diff` byte for byte: rename similarity scores, `index` hash length, binary detection, external filters such as git-lfs, and the user's `diff.*` config all differ. The UI draws diffs with Monaco from the two file versions, so patch text is never shown.
- Decision: The parity test compares structured diffs: per file the status, old and new paths, binary flag, and every hunk's ranges and lines, against `git diff` run with user config disabled (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`). Similarity percentages, `index` lines, and hunk-header function context are out of scope. grove ignores the user's `diff.*` config. Files whose attributes set a `filter` or `diff` driver use the git CLI instead of git2, since libgit2 cannot run them.
- Consequences: grove's diff can differ from the user's own `git diff` when they set options like `diff.algorithm`.
- Revisit if: the diff spike shows git2 misses structured parity on the fixture. Then the git CLI becomes the main path.
