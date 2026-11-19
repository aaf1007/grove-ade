"""Prints which step groups of ci.yml a run needs, from the files a pull request changed.

Reads the changed paths from stdin, each ended by a NUL byte, as `git diff --name-only -z`
prints them. Prints one `<group>=true` or `<group>=false` line per group for $GITHUB_OUTPUT,
and the reason for each on stderr. The last line, `apt_packages=`, repeats the value of the
group named in the argument: the one whose steps need the apt packages of the job.

No path on stdin runs every group. ci.yml sends none for a push to main and when it could not
list the changed files.

Usage: git diff --name-only --no-renames -z <base> <head> |
    step_groups_to_run.py <step group that needs the job's apt packages>
"""

import sys

# The paths each group's steps read. A group runs when a changed file is one of its paths.
# A path ending in "/" is a directory and everything under it. Any other is one file.
# A step that starts to read a path outside its group's list needs that path added here, or a
# pull request that changes only that path skips the step.
PATHS_READ_BY_STEP_GROUP = {
    # The rust job: fmt, clippy, nextest and the doctests, with their toolchain and apt packages.
    "rust": [
        # The workflow, and the scripts it runs.
        ".github/",
        # The workspace crates.
        "crates/",
        # The app crate, with the Tauri config, capabilities and icons its build reads.
        "app/src-tauri/",
        # The workspace members and the pinned dependencies.
        "Cargo.toml",
        "Cargo.lock",
        # The pinned toolchain, clippy and rustfmt.
        "rust-toolchain.toml",
        # The ci profile nextest runs with.
        ".config/nextest.toml",
        # Rust tests run the fake agents, build the diff repo and read the chat recording.
        "fixtures/",
    ],
    # The fixture steps of the ts job: the Python checks, and the merge hook's test.
    "fixtures": [
        # The workflow, and the scripts it runs.
        ".github/",
        # The checks, and the fakes, builders and recordings they check.
        "fixtures/",
        # block-merge.test.sh, and the hook it tests.
        ".claude/hooks/",
    ],
    # The TypeScript steps of the ts job: npm ci, tsc, eslint and vitest in app/ and the sidecar.
    "ts": [
        # The workflow, and the scripts it runs.
        ".github/",
        # The UI, with its package files and tool configs. src-tauri is in here too. Only 2 of
        # 109 pull requests changed it and no other path of this group, so leaving it out would
        # save little.
        "app/",
        # The Claude sidecar, with its package files and tool configs.
        "sidecar/claude/",
    ],
}


def is_at_or_under(changed_file: str, path: str) -> bool:
    return changed_file.startswith(path) if path.endswith("/") else changed_file == path


def changed_file_each_group_reads(changed_files: list[str]) -> dict[str, str | None]:
    """Returns, for each group, the first changed file its steps read, or None when none is."""
    return {
        group: next(
            (
                changed_file
                for changed_file in changed_files
                if any(is_at_or_under(changed_file, path) for path in paths)
            ),
            None,
        )
        for group, paths in PATHS_READ_BY_STEP_GROUP.items()
    }


def step_groups_to_run(changed_files: list[str]) -> dict[str, bool]:
    """Returns whether each group must run. With no changed file, every group must."""
    if not changed_files:
        return dict.fromkeys(PATHS_READ_BY_STEP_GROUP, True)
    return {
        group: changed_file is not None
        for group, changed_file in changed_file_each_group_reads(changed_files).items()
    }


def main() -> None:
    if len(sys.argv) != 2 or sys.argv[1] not in PATHS_READ_BY_STEP_GROUP:
        sys.exit(f"usage: step_groups_to_run.py <{'|'.join(PATHS_READ_BY_STEP_GROUP)}>")
    apt_packages_step_group = sys.argv[1]
    # git prints a path's bytes as they are. One that is not UTF-8 still matches its directory.
    changed_paths = sys.stdin.buffer.read().decode(errors="surrogateescape")
    changed_files = [path for path in changed_paths.split("\0") if path]
    changed_file_read = changed_file_each_group_reads(changed_files)
    groups_to_run = step_groups_to_run(changed_files)
    for group, runs in groups_to_run.items():
        if not changed_files:
            reason = "run, no changed file was listed"
        elif runs:
            reason = f"run, {changed_file_read[group]} changed"
        else:
            reason = f"skip, none of the {len(changed_files)} changed files is one it reads"
        print(f"{group}: {reason}", file=sys.stderr)
        print(f"{group}={str(runs).lower()}")
    print(f"apt_packages={str(groups_to_run[apt_packages_step_group]).lower()}")


if __name__ == "__main__":
    main()
