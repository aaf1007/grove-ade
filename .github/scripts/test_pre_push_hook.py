# Tests for .githooks/pre-push. Run: python3 -m unittest discover .github/scripts
# Each test builds a throwaway repo with the hook in it, and puts fake cargo and npx first on
# PATH. The fakes log each gate they are asked to run, so no test builds or pushes anything real.
import os
import shutil
import subprocess
import tempfile
import signal
import textwrap
import time
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
HOOK = REPO_ROOT / ".githooks" / "pre-push"
RUN_GATES = REPO_ROOT / ".claude" / "skills" / "reviewer" / "run_gates.sh"
STEP_GROUPS_SCRIPT = REPO_ROOT / ".github" / "scripts" / "step_groups_to_run.py"

# The Rust gates when the build folder was last used by another checkout, or never. The clean is
# not a gate, but the hook runs it with the gates, in this order.
RUST_GATES = [
    "cargo fmt --check",
    "cargo clean --workspace",
    "cargo clippy --all-targets -- -D warnings",
    "cargo test",
]
# The Rust gates when this checkout built in the folder last.
RUST_GATES_AGAIN = [gate for gate in RUST_GATES if gate != "cargo clean --workspace"]
TYPESCRIPT_GATES = [
    gate
    for typescript_directory in ("app", "sidecar/claude")
    for gate in (
        f"npx tsc --noEmit (in {typescript_directory})",
        f"npx eslint . (in {typescript_directory})",
        f"npx vitest run (in {typescript_directory})",
    )
]

# Logs "<run name> start|end <gate> (in <directory>)" around each gate. FAKE_GATE_FAILS names a
# gate word, such as clippy, that fails with output. FAKE_GATE_SECONDS makes each gate take that
# long.
FAKE_GATE = textwrap.dedent(
    """\
    #!/bin/sh
    gate="$(basename "$0") $*"
    directory=$(pwd -P)
    directory=${directory#"$FAKE_REPO_ROOT"}
    directory=${directory#/}
    [ -n "$directory" ] && gate="$gate (in $directory)"
    echo "$FAKE_RUN_NAME start $gate" >> "$FAKE_GATE_LOG"
    echo "$CARGO_TARGET_DIR" > "$FAKE_TARGET_DIR_LOG"
    sleep "${FAKE_GATE_SECONDS:-0}"
    case " $* " in
      *" $FAKE_GATE_FAILS "*)
        echo "fake $FAKE_GATE_FAILS found 3 problems"
        exit 1 ;;
    esac
    echo "$FAKE_RUN_NAME end $gate" >> "$FAKE_GATE_LOG"
    """
)


def git(repository, *arguments):
    return subprocess.run(
        ["git", "-c", "user.name=test", "-c", "user.email=test@example.com", *arguments],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


class HookRepo:
    """A repo with the hook, an origin whose main has one commit, and a feature branch."""

    def __init__(self, root: Path):
        self.root = root
        self.work = root / "work"
        self.gate_log = root / "gates.log"
        self.target_dir_log = root / "target_dir.log"
        fake_bin = root / "bin"
        fake_bin.mkdir()
        for gate_binary in ("cargo", "npx", "npm"):
            (fake_bin / gate_binary).write_text(FAKE_GATE)
            (fake_bin / gate_binary).chmod(0o755)

        origin = root / "origin.git"
        subprocess.run(["git", "init", "--quiet", "--bare", "-b", "main", str(origin)], check=True)
        subprocess.run(["git", "init", "--quiet", "-b", "main", str(self.work)], check=True)
        self.write(".githooks/pre-push", HOOK.read_text())
        (self.work / ".githooks/pre-push").chmod(0o755)
        self.write(".github/scripts/step_groups_to_run.py", STEP_GROUPS_SCRIPT.read_text())
        for path in ("crates/grove-core/src/lib.rs", "app/src/App.tsx", "docs/vision.md"):
            self.write(path, "first\n")
        self.write(".gitignore", "node_modules/\n")
        for typescript_directory in ("app", "sidecar/claude"):
            for gate_binary in ("tsc", "eslint", "vitest"):
                self.write(f"{typescript_directory}/node_modules/.bin/{gate_binary}", "")
                (self.work / typescript_directory / "node_modules/.bin" / gate_binary).chmod(0o755)
        git(self.work, "add", ".")
        git(self.work, "commit", "--quiet", "-m", "first")
        git(self.work, "remote", "add", "origin", str(origin))
        git(self.work, "push", "--quiet", "--no-verify", "origin", "main")
        git(self.work, "switch", "--quiet", "-c", "feature")

        self.environment = {
            name: value
            for name, value in os.environ.items()
            if not name.startswith(("GIT_", "GROVE_", "CARGO_TARGET_DIR"))
        }
        self.environment.update(
            HOME=str(root / "home"),
            PATH=f"{fake_bin}{os.pathsep}{os.environ['PATH']}",
            FAKE_REPO_ROOT=str(self.work.resolve()),
            FAKE_GATE_LOG=str(self.gate_log),
            FAKE_TARGET_DIR_LOG=str(self.target_dir_log),
            FAKE_RUN_NAME="run",
        )

    def write(self, path, content):
        (self.work / path).parent.mkdir(parents=True, exist_ok=True)
        (self.work / path).write_text(content)

    def commit_change_to(self, *paths):
        for path in paths:
            self.write(path, f"changed {path}\n")
        git(self.work, "add", ".")
        git(self.work, "commit", "--quiet", "-m", "change")
        return git(self.work, "rev-parse", "HEAD")

    def pushed_refs(self, local_sha):
        return f"refs/heads/feature {local_sha} refs/heads/feature {'0' * 40}\n"

    def start(self, command, output_path, checkout=None, **environment):
        """Starts command in a checkout, in a process group of its own, output to a file."""
        with open(output_path, "w") as output:
            return subprocess.Popen(
                command,
                cwd=checkout or self.work,
                stdin=subprocess.PIPE,
                stdout=output,
                stderr=output,
                env={**self.environment, **environment},
                text=True,
                start_new_session=True,
            )

    def start_hook(self, local_sha, stderr_path, checkout=None, **environment):
        hook = self.start(
            ["sh", ".githooks/pre-push", "origin", str(self.root / "origin.git")],
            stderr_path,
            checkout,
            **environment,
        )
        hook.stdin.write(self.pushed_refs(local_sha))
        hook.stdin.close()
        return hook

    def run_hook(self, local_sha, checkout=None, **environment):
        stderr_path = self.root / f"hook-{os.urandom(4).hex()}.err"
        hook = self.start_hook(local_sha, stderr_path, checkout, **environment)
        hook.wait(timeout=60)
        return hook.returncode, stderr_path.read_text()

    def wait_for_gate_start(self, run_name):
        deadline = time.monotonic() + 30
        while not any(line.startswith(f"{run_name} start") for line in self.gate_log_lines()):
            if time.monotonic() > deadline:
                raise AssertionError(f"{run_name} started no gate in 30 s")
            time.sleep(0.05)

    def gate_log_lines(self):
        return self.gate_log.read_text().splitlines() if self.gate_log.exists() else []

    def gates_run(self):
        return [line.split(" start ", 1)[1] for line in self.gate_log_lines() if " start " in line]


class PrePushHook(unittest.TestCase):
    def setUp(self):
        temporary_directory = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, temporary_directory, ignore_errors=True)
        self.repo = HookRepo(Path(temporary_directory).resolve())

    def test_a_push_that_changes_only_app_src_runs_no_cargo_gate(self):
        pushed_sha = self.repo.commit_change_to("app/src/App.tsx")

        exit_code, stderr = self.repo.run_hook(pushed_sha)

        self.assertEqual(exit_code, 0, stderr)
        self.assertEqual(self.repo.gates_run(), TYPESCRIPT_GATES)
        self.assertIn("rust: skip", stderr)
        self.assertIn("ts: run, app/src/App.tsx changed", stderr)

    def test_a_push_that_changes_crates_runs_every_rust_gate(self):
        pushed_sha = self.repo.commit_change_to("crates/grove-core/src/lib.rs")

        exit_code, stderr = self.repo.run_hook(pushed_sha)

        self.assertEqual(exit_code, 0, stderr)
        self.assertEqual(self.repo.gates_run(), RUST_GATES)
        self.assertIn("rust: run, crates/grove-core/src/lib.rs changed", stderr)
        self.assertIn("ts: skip", stderr)

    def test_a_push_whose_changed_files_cannot_be_listed_runs_every_gate(self):
        pushed_sha = self.repo.commit_change_to("docs/vision.md")
        git(self.repo.work, "update-ref", "-d", "refs/remotes/origin/main")

        exit_code, stderr = self.repo.run_hook(pushed_sha)

        self.assertEqual(exit_code, 0, stderr)
        self.assertEqual(self.repo.gates_run(), RUST_GATES + TYPESCRIPT_GATES)
        self.assertIn("could not list the files", stderr)

    def test_a_push_of_files_no_gate_reads_runs_no_gate_and_says_why(self):
        pushed_sha = self.repo.commit_change_to("docs/vision.md")

        exit_code, stderr = self.repo.run_hook(pushed_sha)

        self.assertEqual(exit_code, 0, stderr)
        self.assertEqual(self.repo.gates_run(), [])
        self.assertIn("rust: skip, none of the 1 changed files is one it reads", stderr)
        self.assertIn("ts: skip, none of the 1 changed files is one it reads", stderr)

    def test_only_the_commits_of_the_branch_count_not_what_main_gained_since(self):
        git(self.repo.work, "switch", "--quiet", "main")
        self.repo.commit_change_to("crates/grove-core/src/lib.rs")
        git(self.repo.work, "push", "--quiet", "--no-verify", "origin", "main")
        git(self.repo.work, "switch", "--quiet", "feature")
        pushed_sha = self.repo.commit_change_to("app/src/App.tsx")

        exit_code, stderr = self.repo.run_hook(pushed_sha)

        self.assertEqual(exit_code, 0, stderr)
        self.assertEqual(self.repo.gates_run(), TYPESCRIPT_GATES)

    def test_a_failed_gate_refuses_the_push_with_its_output(self):
        pushed_sha = self.repo.commit_change_to("crates/grove-core/src/lib.rs")

        exit_code, stderr = self.repo.run_hook(pushed_sha, FAKE_GATE_FAILS="clippy")

        self.assertNotEqual(exit_code, 0)
        self.assertIn("fake clippy found 3 problems", stderr)
        self.assertIn("FAILED gate: cargo clippy", stderr)
        self.assertIn("push refused", stderr)
        self.assertNotIn("cargo test", self.repo.gates_run())

    def test_a_real_push_runs_the_hook_on_the_pushed_refs(self):
        git(self.repo.work, "config", "core.hooksPath", ".githooks")
        self.repo.commit_change_to("crates/grove-core/src/lib.rs")

        push = subprocess.run(
            ["git", "push", "--quiet", "origin", "feature"],
            cwd=self.repo.work,
            env={**self.repo.environment, "FAKE_GATE_FAILS": "clippy"},
            capture_output=True,
            text=True,
        )

        self.assertNotEqual(push.returncode, 0)
        self.assertIn("fake clippy found 3 problems", push.stderr)
        self.assertEqual(self.repo.gates_run(), RUST_GATES[:3])


    def test_the_rust_gates_build_in_one_shared_folder_outside_the_worktree(self):
        pushed_sha = self.repo.commit_change_to("crates/grove-core/src/lib.rs")

        self.repo.run_hook(pushed_sha)
        default_target_dir = self.repo.target_dir_log.read_text().strip()
        self.repo.run_hook(pushed_sha, GROVE_HOOK_TARGET_DIR=str(self.repo.root / "elsewhere/target"))
        chosen_target_dir = self.repo.target_dir_log.read_text().strip()

        self.assertEqual(default_target_dir, f"{self.repo.root}/home/.cache/grove-hook/target")
        self.assertEqual(chosen_target_dir, f"{self.repo.root}/elsewhere/target")

    # cargo names the grove crates' artifacts the same for every checkout and judges them fresh by
    # mtime, so a worktree older than another checkout's build would test that checkout's code.
    def test_a_push_after_another_checkout_built_rebuilds_the_grove_crates(self):
        pushed_sha = self.repo.commit_change_to("crates/grove-core/src/lib.rs")
        other_checkout = self.repo.root / "other-worktree"
        git(self.repo.work, "worktree", "add", "--quiet", "--detach", str(other_checkout), pushed_sha)
        other_exit_code, other_stderr = self.repo.run_hook(pushed_sha, checkout=other_checkout)
        self.assertEqual(other_exit_code, 0, other_stderr)
        built_by_other_checkout = self.repo.root / "home/.cache/grove-hook/target/tmp/grove-hook"
        built_by_other_checkout.mkdir(parents=True)
        self.repo.gate_log.unlink()

        exit_code, stderr = self.repo.run_hook(pushed_sha)

        self.assertEqual(exit_code, 0, stderr)
        self.assertEqual(self.repo.gates_run(), RUST_GATES)
        self.assertFalse(built_by_other_checkout.exists())

    def test_a_second_push_from_the_same_checkout_keeps_its_build(self):
        pushed_sha = self.repo.commit_change_to("crates/grove-core/src/lib.rs")
        first_exit_code, first_stderr = self.repo.run_hook(pushed_sha)
        self.assertEqual(first_exit_code, 0, first_stderr)
        self.repo.gate_log.unlink()

        exit_code, stderr = self.repo.run_hook(pushed_sha)

        self.assertEqual(exit_code, 0, stderr)
        self.assertEqual(self.repo.gates_run(), RUST_GATES_AGAIN)


class OneGateRunAtATime(unittest.TestCase):
    def setUp(self):
        temporary_directory = tempfile.mkdtemp()
        self.addCleanup(shutil.rmtree, temporary_directory, ignore_errors=True)
        self.repo = HookRepo(Path(temporary_directory).resolve())
        self.pushed_sha = self.repo.commit_change_to("crates/grove-core/src/lib.rs")

    def start_hook(self, run_name, **environment):
        stderr_path = self.repo.root / f"{run_name}.err"
        hook = self.repo.start_hook(self.pushed_sha, stderr_path, FAKE_RUN_NAME=run_name, **environment)
        self.addCleanup(kill_process_group, hook)
        return hook, stderr_path

    def test_two_runs_started_together_take_turns(self):
        first, _ = self.start_hook("first", FAKE_GATE_SECONDS="0.5")
        self.repo.wait_for_gate_start("first")
        second, second_stderr = self.start_hook("second")

        self.assertEqual(first.wait(timeout=60), 0)
        self.assertEqual(second.wait(timeout=60), 0)

        run_names = [line.split()[0] for line in self.repo.gate_log_lines()]
        self.assertEqual(run_names, ["first"] * 8 + ["second"] * 6)
        self.assertRegex(second_stderr.read_text(), r"waiting for the gate lock, held by pre-push in .*work")

    def test_a_lock_whose_owner_died_does_not_block_the_next_run(self):
        dead, _ = self.start_hook("dead", FAKE_GATE_SECONDS="60")
        self.repo.wait_for_gate_start("dead")
        # Only the hook dies. The gate it started goes on.
        os.kill(dead.pid, signal.SIGKILL)
        dead.wait(timeout=10)

        next_run, next_stderr = self.start_hook("next")

        self.assertEqual(next_run.wait(timeout=30), 0, next_stderr.read_text())
        self.assertNotIn("waiting", next_stderr.read_text())

    def test_the_reviewer_gate_run_waits_for_a_hook_run(self):
        hook, _ = self.start_hook("hook", FAKE_GATE_SECONDS="0.5")
        self.repo.wait_for_gate_start("hook")
        reviewer_stderr = self.repo.root / "reviewer.err"
        reviewer = self.repo.start(
            ["sh", str(RUN_GATES), self.pushed_sha], reviewer_stderr, FAKE_RUN_NAME="reviewer"
        )
        self.addCleanup(kill_process_group, reviewer)

        self.assertEqual(hook.wait(timeout=60), 0)
        reviewer.wait(timeout=120)

        run_names = [line.split()[0] for line in self.repo.gate_log_lines()]
        self.assertEqual(run_names[:8], ["hook"] * 8)
        self.assertEqual(set(run_names[8:]), {"reviewer"})
        self.assertIn("waiting for the gate lock, held by pre-push", reviewer_stderr.read_text())


def kill_process_group(process):
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=10)


if __name__ == "__main__":
    unittest.main()
