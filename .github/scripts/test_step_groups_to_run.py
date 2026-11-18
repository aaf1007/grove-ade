# Tests for step_groups_to_run.py. Run: python3 -m unittest discover .github/scripts
import subprocess
import sys
import unittest
from pathlib import Path

from step_groups_to_run import PATHS_READ_BY_STEP_GROUP, step_groups_to_run

SCRIPT = Path(__file__).resolve().parent / "step_groups_to_run.py"
REPO_ROOT = SCRIPT.parents[2]

ALL_GROUPS = {"rust", "fixtures", "ts"}


def groups_that_run(changed_files):
    return {group for group, runs in step_groups_to_run(changed_files).items() if runs}


class StepGroupsToRun(unittest.TestCase):
    def test_a_docs_only_change_runs_no_group(self):
        changed_files = ["docs/vision.md", "CLAUDE.md", ".claude/skills/devops/SKILL.md"]

        self.assertEqual(groups_that_run(changed_files), set())

    def test_a_crates_only_change_runs_only_the_rust_steps(self):
        self.assertEqual(groups_that_run(["crates/grove-core/src/watch.rs"]), {"rust"})

    def test_an_app_src_only_change_runs_only_the_typescript_steps(self):
        self.assertEqual(groups_that_run(["app/src/bridge.ts"]), {"ts"})

    def test_a_workflow_or_ci_script_change_runs_every_group(self):
        self.assertEqual(groups_that_run([".github/workflows/ci.yml"]), ALL_GROUPS)
        self.assertEqual(groups_that_run([".github/scripts/flaky_tests_summary.py"]), ALL_GROUPS)

    def test_a_fixtures_only_change_runs_the_fixture_and_rust_steps(self):
        changed_files = ["fixtures/fake-agents/fake_agent.py"]

        self.assertEqual(groups_that_run(changed_files), {"fixtures", "rust"})

    def test_no_changed_file_runs_every_group(self):
        self.assertEqual(groups_that_run([]), ALL_GROUPS)

    def test_one_file_a_group_reads_among_files_it_does_not_runs_that_group(self):
        changed_files = ["docs/vision.md", "README.md", "sidecar/claude/src/main.ts"]

        self.assertEqual(groups_that_run(changed_files), {"ts"})

    def test_the_tauri_crate_runs_the_rust_steps_and_the_rest_of_app_does_not(self):
        self.assertIn("rust", groups_that_run(["app/src-tauri/src/main.rs"]))
        self.assertNotIn("rust", groups_that_run(["app/package-lock.json"]))

    def test_the_files_each_gate_is_configured_by_run_its_group(self):
        self.assertEqual(groups_that_run(["Cargo.lock"]), {"rust"})
        self.assertEqual(groups_that_run(["rust-toolchain.toml"]), {"rust"})
        self.assertEqual(groups_that_run([".config/nextest.toml"]), {"rust"})
        self.assertEqual(groups_that_run(["app/package-lock.json"]), {"ts"})
        self.assertEqual(groups_that_run(["sidecar/claude/package-lock.json"]), {"ts"})

    def test_the_merge_hook_runs_the_fixture_steps(self):
        self.assertEqual(groups_that_run([".claude/hooks/block-merge.sh"]), {"fixtures"})

    def test_a_file_path_matches_whole_and_a_directory_path_matches_what_is_under_it(self):
        self.assertEqual(groups_that_run(["Cargo.lock.orig"]), set())
        self.assertEqual(groups_that_run(["cratesmanship.md"]), set())
        self.assertEqual(groups_that_run(["docs/crates/notes.md"]), set())

    # A path that was renamed or removed would leave its group skipped in silence.
    def test_every_listed_path_exists_in_the_repo(self):
        for group, paths in PATHS_READ_BY_STEP_GROUP.items():
            for path in paths:
                with self.subTest(group=group, path=path):
                    listed = REPO_ROOT / path
                    self.assertTrue(listed.is_dir() if path.endswith("/") else listed.is_file())


class CommandLine(unittest.TestCase):
    def run_script(self, stdin, *arguments):
        return subprocess.run([sys.executable, str(SCRIPT), *arguments], input=stdin, capture_output=True)

    def outputs(self, stdin, apt_packages_step_group):
        result = self.run_script(stdin, apt_packages_step_group)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        return result.stdout.decode().split()

    def test_it_reads_nul_separated_paths_and_prints_one_output_per_group(self):
        changed_files = b"docs/vision.md\0crates/grove-hook/src/main.rs\0"

        self.assertEqual(
            self.outputs(changed_files, "rust"),
            ["rust=true", "fixtures=false", "ts=false", "apt_packages=true"],
        )

    def test_the_apt_packages_output_follows_the_group_named_in_the_argument(self):
        changed_files = b"crates/grove-hook/src/main.rs\0"

        self.assertIn("apt_packages=true", self.outputs(changed_files, "rust"))
        self.assertIn("apt_packages=false", self.outputs(changed_files, "fixtures"))

    # What a push to main sends, so main always runs every step and saves its caches.
    def test_empty_input_prints_every_output_as_true(self):
        for apt_packages_step_group in ("rust", "fixtures"):
            self.assertEqual(
                self.outputs(b"", apt_packages_step_group),
                ["rust=true", "fixtures=true", "ts=true", "apt_packages=true"],
            )

    def test_a_path_that_is_not_utf8_still_runs_its_group(self):
        outputs = self.outputs(b"fixtures/caf\xe9.txt\0", "fixtures")

        self.assertEqual(outputs, ["rust=true", "fixtures=true", "ts=false", "apt_packages=true"])

    def test_a_missing_or_unknown_group_argument_fails(self):
        self.assertNotEqual(self.run_script(b"").returncode, 0)
        self.assertNotEqual(self.run_script(b"", "cargo").returncode, 0)

    def test_it_says_on_stderr_which_file_made_a_group_run(self):
        result = self.run_script(b"docs/vision.md\0app/src/App.tsx\0", "rust")

        self.assertIn("ts: run, app/src/App.tsx changed", result.stderr.decode())
        self.assertIn("rust: skip", result.stderr.decode())


if __name__ == "__main__":
    unittest.main()
