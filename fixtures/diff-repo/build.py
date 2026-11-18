#!/usr/bin/env python3
"""Build the diff fixture repo from scratch. See README.md for the cases.

Usage: python3 build.py <dir>

<dir> must be missing or empty. Writes <dir>/repo, the fixture, and
<dir>/origin.git, its bare remote, then prints the repo path.
Needs only git and Python 3.9 or later.
"""

import hashlib
import os
import subprocess
import sys
from pathlib import Path

# 2026-01-01T00:00:00Z. Commit n is dated n minutes later.
EPOCH = 1767225600

# Keep out user and system git config, attributes and ignore files.
ENV = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
ENV.update(
    GIT_CONFIG_GLOBAL=os.devnull,
    GIT_CONFIG_NOSYSTEM="1",
    GIT_ATTR_NOSYSTEM="1",
    XDG_CONFIG_HOME=os.devnull,
    GIT_AUTHOR_NAME="Grove Fixture",
    GIT_AUTHOR_EMAIL="fixture@grove.invalid",
    GIT_COMMITTER_NAME="Grove Fixture",
    GIT_COMMITTER_EMAIL="fixture@grove.invalid",
)


def lfs_pointer(content):
    oid = hashlib.sha256(content).hexdigest().encode()
    return b"version https://git-lfs.github.com/spec/v1\noid sha256:%s\nsize %d\n" % (
        oid,
        len(content),
    )


# 1250 four-line functions: 5000 lines.
BIG_PY = b"".join(b"def f%d():\n    x = %d\n    return x\n\n" % (n, n) for n in range(1, 1251))

# FIX messages split by SOH bytes. No NUL, so git calls it text.
FIX = b"".join(b"8=FIX.4.2\x0134=%d\x0135=0\x0149=GROVE\x0156=TEST\x01\n" % n for n in range(1, 4))

BASE = {
    ".gitignore": b"*.log\n",
    ".gitattributes": b"lfs/*.bin filter=lfs diff=lfs merge=lfs -text\n",
    "deleted.txt": b"this file gets deleted\n",
    "renamed-old.txt": b"this file gets renamed\n",
    "moved/before.txt": b"".join(b"line %d\n" % n for n in range(1, 21)),
    "binary.dat": bytes(range(256)),
    "control-chars.txt": FIX,
    "crlf-convert.txt": b"one\ntwo\nthree\n",
    "crlf-edit.txt": b"alpha\r\nbeta\r\ngamma\r\n",
    "whitespace.txt": b"key = value\n\tindented line\nend\n",
    "big.py": BIG_PY,
    "staged.txt": b"before staging\n",
    "staged-unstaged.txt": b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
    "latin1.txt": "caf\u00e9\nna\u00efve\nend\n".encode("latin-1"),
    "script.sh": b"#!/bin/sh\necho fixture\n",
    "lfs/asset.bin": lfs_pointer(b"asset v1"),
    "no-eol.txt": b"first\nlast",
    "emptied.txt": b"x\ny\nz\n",
    "dir with space/na\u00efve.txt": b"a path with a space and a non-ASCII name\n",
    "slide.py": b"if foo:\n    qux()\n",
}


def git(cwd, *args, env=ENV):
    subprocess.run(["git", *args], cwd=cwd, env=env, check=True, stdout=subprocess.DEVNULL)


def commit(repo, n, message):
    date = f"{EPOCH + 60 * n} +0000"
    env = dict(ENV, GIT_AUTHOR_DATE=date, GIT_COMMITTER_DATE=date)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", message, env=env)


def write(repo, path, data):
    file = repo / path
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_bytes(data)


def edit(repo, path, old, new):
    file = repo / path
    data = file.read_bytes()
    if data.count(old) != 1:
        sys.exit(f"build.py: {old!r} is not in {path} exactly once")
    file.write_bytes(data.replace(old, new))


def relink(repo, path, target):
    link = repo / path
    if link.is_symlink():
        link.unlink()
    os.symlink(target, link)


def change_worktree(repo):
    """The 21 changes `git diff HEAD` shows. Some are staged."""
    write(repo, "added.txt", b"a new file\n")
    write(repo, "empty-added.txt", b"")
    git(repo, "add", "added.txt", "empty-added.txt")
    (repo / "deleted.txt").unlink()
    git(repo, "mv", "renamed-old.txt", "renamed-new.txt")
    git(repo, "mv", "moved/before.txt", "moved/after.txt")
    edit(repo, "moved/after.txt", b"line 10\n", b"line ten\n")
    write(repo, "binary.dat", bytes(reversed(range(256))))
    edit(repo, "control-chars.txt", b"34=2\x0135=0", b"34=2\x0135=1")
    write(repo, "crlf-convert.txt", b"one\r\ntwo\r\nthree\r\n")
    edit(repo, "crlf-edit.txt", b"beta\r\n", b"BETA\r\n")
    write(repo, "whitespace.txt", b"key = value  \n    indented line\nend\n")
    edit(repo, "big.py", b"    x = 10\n", b"    x = 10 + 1\n")
    edit(repo, "big.py", b"def f601():\n", b"def f600b():\n    x = 0\n    return x\n\ndef f601():\n")
    edit(repo, "big.py", b"def f900():\n    x = 900\n    return x\n\n", b"")
    edit(repo, "big.py", b"    x = 1250\n", b"    x = -1\n")
    write(repo, "staged.txt", b"after staging\n")
    git(repo, "add", "staged.txt")
    edit(repo, "staged-unstaged.txt", b"two\n", b"two, staged\n")
    git(repo, "add", "staged-unstaged.txt")
    edit(repo, "staged-unstaged.txt", b"nine\n", b"nine, not staged\n")
    edit(repo, "latin1.txt", b"na\xefve\n", b"na\xefvet\xe9\n")
    os.chmod(repo / "script.sh", 0o755)
    write(repo, "lfs/asset.bin", lfs_pointer(b"asset v2"))
    write(repo, "no-eol.txt", b"first\nlast\nadded\n")
    write(repo, "emptied.txt", b"")
    relink(repo, "link", "target-b.txt")
    edit(repo, "dir with space/na\u00efve.txt", b"name\n", b"name\nedited\n")
    # The inserted block repeats the lines after it, so it can slide. git's
    # indent heuristic puts it before them.
    write(repo, "slide.py", b"if foo:\n    bar()\n\nif foo:\n    qux()\n")


def add_untracked(repo):
    write(repo, "untracked.txt", b"not added yet\n")
    write(repo, "untracked-empty.txt", b"")
    write(repo, "untracked.bin", b"\x00\x01\x02untracked binary\n")
    write(repo, "untracked-dir/nested.txt", b"inside an untracked folder")
    write(repo, "build.log", b"ignored by .gitignore\n")


def main(argv):
    if len(argv) != 2:
        sys.exit("usage: build.py <dir>")
    out = Path(argv[1]).resolve()
    if out.exists() and any(out.iterdir()):
        sys.exit(f"build.py: {out} is not empty")
    repo = out / "repo"
    origin = out / "origin.git"
    out.mkdir(parents=True, exist_ok=True)

    # main: A, B, C. origin/main gets all three, local main stays at A,
    # and feature branches from B. So `merge-base origin/main HEAD` is B
    # while `merge-base main HEAD` is A.
    git(out, "init", "-q", "--object-format=sha1", "-b", "main", str(repo))
    for path, data in BASE.items():
        write(repo, path, data)
    relink(repo, "link", "target-a.txt")
    os.chmod(repo / "script.sh", 0o644)
    commit(repo, 0, "Base files")
    write(repo, "upstream.txt", b"on main before the branch point\n")
    commit(repo, 1, "Add upstream.txt")
    write(repo, "upstream-later.txt", b"on origin/main after the branch point\n")
    commit(repo, 2, "Add upstream-later.txt")

    git(out, "init", "-q", "--bare", "--object-format=sha1", "-b", "main", str(origin))
    git(repo, "remote", "add", "origin", str(origin))
    git(repo, "push", "-q", "origin", "main")
    git(repo, "remote", "set-head", "origin", "main")
    git(repo, "checkout", "-q", "-b", "feature", "main~1")
    git(repo, "branch", "-f", "main", "main~2")

    write(repo, "feature.txt", b"feature work\n")
    commit(repo, 3, "Add feature.txt")
    edit(repo, "feature.txt", b"work\n", b"work\nmore work\n")
    edit(repo, "big.py", b"    x = 5\n", b"    x = 5 * 2\n")
    commit(repo, 4, "Extend feature.txt and edit big.py")

    change_worktree(repo)
    add_untracked(repo)
    print(repo)


if __name__ == "__main__":
    main(sys.argv)
