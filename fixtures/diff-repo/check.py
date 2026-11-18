#!/usr/bin/env python3
"""Check that the diff fixture builds the same every time and that git shows each scripted case.

Run: python3 fixtures/diff-repo/check.py
Needs only git and Python 3.9 or later.
"""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

BUILD = Path(__file__).resolve().parent / "build.py"

# Keep out user and system git config, attributes and ignore files.
GIT_ENV = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
GIT_ENV.update(
    GIT_CONFIG_GLOBAL=os.devnull,
    GIT_CONFIG_NOSYSTEM="1",
    GIT_ATTR_NOSYSTEM="1",
    XDG_CONFIG_HOME=os.devnull,
    LC_ALL="C",
)


def build(out):
    result = subprocess.run(
        [sys.executable, str(BUILD), str(out)], stdout=subprocess.PIPE, check=True
    )
    return Path(result.stdout.decode().strip())


def git(repo, *args, ok=(0,)):
    result = subprocess.run(
        ["git", *args], cwd=repo, env=GIT_ENV, stdout=subprocess.PIPE
    )
    if result.returncode not in ok:
        raise AssertionError(f"git {' '.join(args)} exited {result.returncode}")
    return result.stdout


# main, HEAD (feature) and origin/main. Any change to the committed content
# changes these. Update them in the same PR.
COMMIT_IDS = [
    "cf18661ee0172095f3b975198dcd3e07eb925a35",
    "71f5d25744d459d95de47009dbb56570a2354385",
    "85c4e151095b09782ed634bb1f2dbf910ad37172",
]


def commit_ids(repo):
    return git(repo, "rev-parse", "main", "HEAD", "origin/main").decode().split()


def raw_diff(repo, *args):
    """{new path: (status, score, old path, old mode, new mode)} from `git diff --raw`."""
    fields = git(repo, "diff", "--raw", "-z", *args).split(b"\0")
    entries = {}
    i = 0
    while fields[i]:
        old_mode, new_mode, _, _, status = fields[i][1:].decode().split()
        paths = 2 if status[0] in "RC" else 1
        old, new = fields[i + 1].decode(), fields[i + paths].decode()
        entries[new] = (status[0], status[1:], old, old_mode, new_mode)
        i += 1 + paths
    return entries


def names(repo, *args):
    return set(git(repo, "diff", "--name-only", "-z", *args).decode().split("\0")) - {""}


def hunks(patch):
    return sum(line.startswith(b"@@ ") for line in patch.splitlines())


# The 21 scripted changes, as `git diff HEAD` reports them.
HEAD_CASES = {
    "added.txt": "A",
    "empty-added.txt": "A",
    "deleted.txt": "D",
    "renamed-new.txt": "R",
    "moved/after.txt": "R",
    "binary.dat": "M",
    "control-chars.txt": "M",
    "crlf-convert.txt": "M",
    "crlf-edit.txt": "M",
    "whitespace.txt": "M",
    "big.py": "M",
    "staged.txt": "M",
    "staged-unstaged.txt": "M",
    "latin1.txt": "M",
    "script.sh": "M",
    "lfs/asset.bin": "M",
    "no-eol.txt": "M",
    "emptied.txt": "M",
    "link": "M",
    "dir with space/na\u00efve.txt": "M",
    "slide.py": "M",
}

UNTRACKED = {"untracked.txt", "untracked-empty.txt", "untracked.bin", "untracked-dir/nested.txt"}


class Fixture(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.repo = build(Path(cls.tmp.name) / "first")

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def test_every_build_has_the_same_commit_ids(self):
        again = build(Path(self.tmp.name) / "second")
        self.assertEqual(commit_ids(self.repo), COMMIT_IDS)
        self.assertEqual(commit_ids(again), COMMIT_IDS)
        self.assertEqual(
            git(self.repo, "diff", "HEAD", "--binary"),
            git(again, "diff", "HEAD", "--binary"),
        )

    def head_patch(self, path, *args):
        return git(self.repo, "diff", "HEAD", *args, "--", path)

    def numstat(self, path):
        return git(self.repo, "diff", "HEAD", "--numstat", "--", path).split(b"\t")[:2]

    def test_head_diff_shows_exactly_the_21_cases(self):
        head = raw_diff(self.repo, "HEAD")
        self.assertEqual({path: entry[0] for path, entry in head.items()}, HEAD_CASES)
        self.assertEqual(len(HEAD_CASES), 21)

    def test_renames_pair_old_and_new_paths(self):
        head = raw_diff(self.repo, "HEAD")
        self.assertEqual(head["renamed-new.txt"][1:3], ("100", "renamed-old.txt"))
        _, score, old, _, _ = head["moved/after.txt"]
        self.assertEqual(old, "moved/before.txt")
        self.assertLess(int(score), 100)

    def test_empty_added_file_has_no_hunks(self):
        self.assertEqual(hunks(self.head_patch("empty-added.txt")), 0)

    def test_binary_flag_follows_git(self):
        self.assertEqual(self.numstat("binary.dat"), [b"-", b"-"])
        self.assertIn(b"\x01", self.head_patch("control-chars.txt"))
        self.assertNotEqual(self.numstat("control-chars.txt"), [b"-", b"-"])

    def test_line_endings(self):
        convert = self.head_patch("crlf-convert.txt")
        self.assertIn(b"+one\r\n", convert)
        self.assertEqual(hunks(self.head_patch("crlf-convert.txt", "--ignore-cr-at-eol")), 0)
        edit = self.head_patch("crlf-edit.txt")
        self.assertIn(b"-beta\r\n", edit)
        self.assertIn(b"+BETA\r\n", edit)

    def test_whitespace_only_change(self):
        self.assertGreater(hunks(self.head_patch("whitespace.txt")), 0)
        self.assertEqual(hunks(self.head_patch("whitespace.txt", "-w")), 0)

    def test_big_file_has_5000_lines_and_several_hunks(self):
        self.assertEqual(git(self.repo, "show", "HEAD:big.py").count(b"\n"), 5000)
        self.assertGreaterEqual(hunks(self.head_patch("big.py")), 4)

    def test_indent_heuristic_slides_the_inserted_block(self):
        self.assertIn(b"+if foo:\n+    bar()\n+\n if foo:\n", self.head_patch("slide.py"))
        self.assertIn(
            b" if foo:\n+    bar()\n+\n+if foo:\n",
            self.head_patch("slide.py", "--no-indent-heuristic"),
        )

    def test_staged_and_unstaged_changes(self):
        staged = names(self.repo, "--cached")
        unstaged = names(self.repo)
        self.assertIn("staged.txt", staged)
        self.assertNotIn("staged.txt", unstaged)
        self.assertIn("staged-unstaged.txt", staged)
        self.assertIn("staged-unstaged.txt", unstaged)

    def test_non_utf8_text_is_text(self):
        patch = self.head_patch("latin1.txt")
        with self.assertRaises(UnicodeDecodeError):
            patch.decode("utf-8")
        self.assertNotEqual(self.numstat("latin1.txt"), [b"-", b"-"])

    def test_mode_change_has_no_hunks(self):
        self.assertEqual(raw_diff(self.repo, "HEAD")["script.sh"][3:], ("100644", "100755"))
        self.assertEqual(hunks(self.head_patch("script.sh")), 0)

    def test_lfs_file_sets_filter_and_diff_driver(self):
        attrs = git(self.repo, "check-attr", "filter", "diff", "--", "lfs/asset.bin")
        self.assertEqual(attrs, b"lfs/asset.bin: filter: lfs\nlfs/asset.bin: diff: lfs\n")

    def test_missing_newline_at_end_of_file(self):
        self.assertIn(b"\\ No newline at end of file", self.head_patch("no-eol.txt"))

    def test_emptied_file(self):
        self.assertIn(b"@@ -1,3 +0,0 @@", self.head_patch("emptied.txt"))

    def test_symlink_target_change(self):
        self.assertEqual(raw_diff(self.repo, "HEAD")["link"][3:], ("120000", "120000"))
        self.assertIn(b"+target-b.txt", self.head_patch("link"))

    def no_index_patch(self, path):
        return git(self.repo, "diff", "--no-index", "--", "/dev/null", path, ok=(1,))

    def test_untracked_files_skip_ignored_ones(self):
        listed = git(self.repo, "ls-files", "--others", "--exclude-standard", "-z")
        self.assertEqual(set(listed.decode().split("\0")) - {""}, UNTRACKED)
        self.assertTrue((self.repo / "build.log").is_file())

    def test_untracked_files_diff_against_dev_null(self):
        for path in UNTRACKED:
            with self.subTest(path):
                self.assertIn(b"new file mode 100644", self.no_index_patch(path))
        self.assertIn(
            b"Binary files /dev/null and b/untracked.bin differ",
            self.no_index_patch("untracked.bin"),
        )
        self.assertEqual(hunks(self.no_index_patch("untracked-empty.txt")), 0)
        self.assertIn(
            b"\\ No newline at end of file", self.no_index_patch("untracked-dir/nested.txt")
        )

    def merge_base(self, base):
        return git(self.repo, "merge-base", base, "HEAD").decode().strip()

    def test_origin_base_is_ahead_of_local_base(self):
        git(self.repo, "merge-base", "--is-ancestor", "main", "origin/main")
        self.assertNotEqual(self.merge_base("origin/main"), self.merge_base("main"))
        self.assertEqual(
            git(self.repo, "symbolic-ref", "refs/remotes/origin/HEAD"),
            b"refs/remotes/origin/main\n",
        )

    def test_merge_base_diff_adds_branch_commits_to_uncommitted_changes(self):
        base = self.merge_base("origin/main")
        statuses = {path: entry[0] for path, entry in raw_diff(self.repo, base).items()}
        self.assertEqual(statuses, dict(HEAD_CASES, **{"feature.txt": "A"}))
        self.assertEqual(
            hunks(git(self.repo, "diff", base, "--", "big.py")),
            hunks(self.head_patch("big.py")) + 1,
        )

    def test_wrong_bases_give_different_diffs(self):
        self.assertEqual(raw_diff(self.repo, self.merge_base("main"))["upstream.txt"][0], "A")
        self.assertEqual(raw_diff(self.repo, "origin/main")["upstream-later.txt"][0], "D")


if __name__ == "__main__":
    unittest.main()
