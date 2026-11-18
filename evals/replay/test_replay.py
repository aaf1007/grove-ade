# Tests for the bug replay harness. Run: python3 -m unittest discover evals/replay
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import textwrap
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import replay  # noqa: E402

GOOD_ADD = "def add(a, b):\n    return a + b\n"
BAD_ADD = "def add(a, b):\n    return a - b\n"
OLD_TEST = textwrap.dedent("""\
    import unittest
    from calc import add

    class T(unittest.TestCase):
        def test_zero(self):
            self.assertEqual(add(0, 0), 0)
    """)
NEW_TEST = OLD_TEST + "\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n"
BROKEN_TEST = "import unittest\n\nclass T(unittest.TestCase):\n    def test_broken(self):\n        self.fail()\n"
ISSUE = ("Add a calculator", "The calculator adds two numbers.")


def git(repo, *args):
    return subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=True).stdout.strip()


def commit(repo, files, message):
    for path, text in files.items():
        target = Path(repo, path)
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)
    git(repo, "add", "-A")
    git(repo, "commit", "-qm", message)
    return git(repo, "rev-parse", "HEAD")


def toy_history(root, bug_breaks_old_test=False, old_test_expects_bug=False, extra_old_test="",
                broken_folder_the_fix_touches=False, main_has_the_good_add=False, a_file_named_main=False,
                fix_turns_a_folder_into_a_file=False):
    """main, then a branch that adds a calculator with a bug, merges main, and fixes the bug with a test.

    Returns (repo, fix sha, the main commit the branch merged last)."""
    repo = Path(root, "repo")
    repo.mkdir()
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "user.email", "t@example.com")
    git(repo, "config", "user.name", "t")
    commit(repo, {"README.md": "toy\n", **({"lib/calc.py": GOOD_ADD} if main_has_the_good_add else {}),
                  **({"main": "a file named like the branch\n"} if a_file_named_main else {})}, "start")
    git(repo, "checkout", "-qb", "feature")
    bad = "def add(a, b):\n    return a * b\n" if bug_breaks_old_test else BAD_ADD
    old_test = OLD_TEST.replace("add(0, 0), 0", "add(1, 1), 2") if bug_breaks_old_test else OLD_TEST
    if old_test_expects_bug:
        old_test += "\n    def test_three(self):\n        self.assertEqual(add(3, 3), 9)\n"
    old_test += extra_old_test
    feature = {"lib/calc.py": bad, "lib/test_calc.py": old_test}
    if fix_turns_a_folder_into_a_file:
        feature["lib/res/old.txt"] = "old\n"
    if broken_folder_the_fix_touches:
        feature |= {"util/util.py": "x = 1\n", "util/test_util.py": BROKEN_TEST}
    commit(repo, feature, "#7 feat: add")
    git(repo, "checkout", "-q", "main")
    main_tip = commit(repo, {"NOTES.md": "notes\n"}, "notes on main")
    git(repo, "checkout", "-q", "feature")
    git(repo, "merge", "-q", "--no-edit", "main")
    fixed_test = old_test.replace("add(3, 3), 9)", "add(3, 3), 6)") + NEW_TEST[len(OLD_TEST):]
    fixed = {"lib/calc.py": GOOD_ADD, "lib/test_calc.py": fixed_test}
    if broken_folder_the_fix_touches:
        fixed["util/util.py"] = "x = 2\n"
    if fix_turns_a_folder_into_a_file:
        git(repo, "rm", "-rq", "lib/res")
        fixed["lib/res"] = "now a file\n"
    fix = commit(repo, fixed,
                 "#7 fix: add adds")
    git(repo, "checkout", "-q", "main")
    git(repo, "merge", "-q", "--no-ff", "--no-edit", "feature")
    return repo, fix, main_tip


FAKE_CLAUDE = textwrap.dedent("""\
    #!/usr/bin/env python3
    # Stands in for the claude CLI. A stage call streams JSON lines; a judge call prints one result.
    import json, os, sys
    if sys.argv[1:3] == ["plugin", "list"]:
        if os.environ.get("FAKE_PLUGIN_LIST_HANGS"):
            import time
            time.sleep(30)
        print(json.dumps([{"id": "pstack@pstack-claude", "installPath": "/plugins/pstack"},
                          {"id": "code-review@claude-plugins-official", "installPath": "/plugins/code-review"},
                          {"id": "other@market", "installPath": "/plugins/other"}]))
        sys.exit(0)
    prompt = sys.stdin.read()
    fix_on_disk = []
    for folder, _, files in os.walk(os.environ.get("FAKE_SCAN_ROOT", os.devnull)):
        for name in files:
            path = os.path.join(folder, name)
            if not os.path.islink(path) and "return a + b" in open(path, errors="ignore").read():
                fix_on_disk.append(path)
    with open(os.environ["FAKE_CLAUDE_LOG"], "a") as log:
        log.write(json.dumps({"argv": sys.argv[1:], "cwd": os.getcwd(), "prompt": prompt,
                              "fix_on_disk": fix_on_disk}) + "\\n")
    usage = {"claude-sonnet-5-5": {"inputTokens": 10, "outputTokens": 20, "cacheReadInputTokens": 30,
                                   "cacheCreationInputTokens": 40, "costUSD": 0.5}}
    if "stream-json" in sys.argv:
        findings = json.loads(os.environ.get("FAKE_FINDINGS", "[]"))
        text = "Review done.\\n```json\\n" + json.dumps({"findings": findings}) + "\\n```"
        denial = os.environ.get("FAKE_DENIAL")
        tool = json.loads(os.environ.get("FAKE_TOOL_RESULT", "null"))
        used = [{"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t2", "name": tool["name"],
                                                               "input": {}}]}},
                {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t2",
                                                          "content": [{"type": "text", "text": tool["output"]}]}]}}
                ] if tool else []
        denied = [{"type": "system", "subtype": "permission_denied", "tool_use_id": "t1", "message": denial},
                  {"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1",
                                                            "is_error": True, "content": denial}]}}]
        for line in [{"type": "system", "subtype": "init"},
                     {"type": "assistant", "message": {"content": [{"type": "text", "text":
                         os.environ.get("FAKE_TRANSCRIPT_EXTRA", "reading")}]}},
                     *(denied if denial else []),
                     *used,
                     {"type": "result", "result": "", "modelUsage": usage, "total_cost_usd": 0.25},
                     {"type": "result", "result": text, "modelUsage": usage, "total_cost_usd": 0.5}]:
            print(json.dumps(line))
    else:
        verdict = os.environ.get("FAKE_VERDICT", '{"caught": true, "finding": 1, "reason": "names the minus"}')
        print(json.dumps({"type": "result", "result": "```json\\n" + verdict + "\\n```",
                          "modelUsage": {"claude-sonnet-5-5": {"inputTokens": 1, "outputTokens": 1,
                                         "cacheReadInputTokens": 0, "cacheCreationInputTokens": 0,
                                         "costUSD": 0.01}}, "total_cost_usd": 0.01}))
    """)


class ReplayTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.repo, self.fix, self.main_tip = toy_history(self.root)
        self.claude = self.root / "claude"
        self.claude.write_text(FAKE_CLAUDE)
        self.claude.chmod(self.claude.stat().st_mode | stat.S_IEXEC)
        self.log = self.root / "claude.log"
        os.environ["FAKE_CLAUDE_LOG"] = str(self.log)
        for key in ("FAKE_FINDINGS", "FAKE_VERDICT", "FAKE_TRANSCRIPT_EXTRA", "FAKE_DENIAL", "FAKE_SCAN_ROOT",
                    "FAKE_TOOL_RESULT", "FAKE_PLUGIN_LIST_HANGS"):
            os.environ.pop(key, None)

    def tearDown(self):
        self.tmp.cleanup()

    def claude_calls(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()] if self.log.exists() else []

    def test_a_case_splits_the_fix_from_its_test_and_starts_at_the_pull_request_base(self):
        case = replay.load_case(self.repo, self.fix[:7], 7)
        self.assertEqual(case.fix, self.fix)
        self.assertEqual(case.fix_files, ("lib/calc.py",))
        self.assertEqual(case.answer_key, ("lib/test_calc.py",))
        # The branch merged main before the fix, so the review starts from that main commit.
        self.assertEqual(case.base, self.main_tip)

    def test_a_fix_with_no_test_or_no_earlier_work_on_its_branch_is_not_a_case(self):
        no_test = commit(self.repo, {"lib/calc.py": GOOD_ADD + "\n"}, "#8 fix: blank line")
        with self.assertRaisesRegex(replay.CaseError, "no test"):
            replay.load_case(self.repo, no_test, 8)
        git(self.repo, "checkout", "-qb", "lone-fix")
        lone = commit(self.repo, {"lib/calc.py": GOOD_ADD + "# c\n", "lib/test_calc.py": NEW_TEST + "# c\n"},
                      "#9 fix: lone")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "merge", "-q", "--no-ff", "--no-edit", "lone-fix")
        with self.assertRaisesRegex(replay.CaseError, "nothing to review"):
            replay.load_case(self.repo, lone, 9)

    def test_exported_files_are_newer_than_any_build_so_a_shared_cache_rebuilds_them(self):
        # git archive stamps files with the commit date. Cargo would then take an older tree's sources as built.
        env = {**os.environ, "GIT_COMMITTER_DATE": "2000-01-01T00:00:00Z", "GIT_AUTHOR_DATE": "2000-01-01T00:00:00Z"}
        Path(self.repo, "lib/old.py").write_text("x = 1\n")
        git(self.repo, "add", "-A")
        subprocess.run(["git", "-C", str(self.repo), "commit", "-qm", "old"], check=True, env=env)
        before = time.time() - 5
        replay.export_tree(self.repo, "HEAD", self.root / "export")
        self.assertGreater(Path(self.root, "export", "lib/old.py").stat().st_mtime, before)

    def test_a_tree_exported_before_another_tree_was_built_still_builds_its_own_code(self):
        crate = {"Cargo.toml": '[package]\nname = "toy"\nversion = "0.1.0"\nedition = "2021"\n',
                 "src/lib.rs": "pub fn add(a: i32, b: i32) -> i32 { a - b }\n",
                 "tests/add.rs": "#[test]\nfn adds() { assert_eq!(toy::add(2, 3), 5); }\n"}
        buggy = commit(self.repo, crate, "toy crate")
        fixed = commit(self.repo, {"src/lib.rs": "pub fn add(a: i32, b: i32) -> i32 { a + b }\n"}, "fix")
        replay.export_tree(self.repo, buggy, self.root / "buggy")
        time.sleep(1.1)
        replay.export_tree(self.repo, fixed, self.root / "fixed")
        cache, replay.CACHE = replay.CACHE, self.root / "cache"
        try:
            cargo_test = [(".", ["cargo", "test", "--offline"])]
            self.assertEqual(replay.run_commands(self.root / "fixed", cargo_test)[0], [])
            self.assertEqual(len(replay.run_commands(self.root / "buggy", cargo_test)[0]), 1)
        finally:
            replay.CACHE = cache

    def test_the_stage_repo_holds_the_bug_and_never_the_fix_or_its_test(self):
        case = replay.load_case(self.repo, self.fix, 7)
        stage_repo = replay.build_stage_repo(self.repo, case, self.root / "stage", ISSUE)
        self.assertEqual(Path(stage_repo, "lib/calc.py").read_text(), BAD_ADD)
        self.assertEqual(Path(stage_repo, "lib/test_calc.py").read_text(), OLD_TEST)
        self.assertEqual(git(stage_repo, "rev-list", "--all", "--count"), "2")
        self.assertEqual(git(stage_repo, "remote"), "")
        no_object = subprocess.run(["git", "-C", str(stage_repo), "cat-file", "-e", self.fix])
        self.assertNotEqual(no_object.returncode, 0)
        diff = git(stage_repo, "diff", "main...HEAD")
        self.assertIn("return a - b", diff)
        self.assertNotIn("test_add", diff)
        self.assertNotIn("NOTES", diff)
        self.assertEqual(git(stage_repo, "log", "-1", "--format=%s"), ISSUE[0])

    def test_the_proof_fails_the_fix_test_on_the_bug_and_passes_it_on_the_fix(self):
        case = replay.load_case(self.repo, self.fix, 7)
        proof = replay.prove(self.repo, case, self.root / "proof")
        self.assertEqual((proof.fails_without_fix, proof.passes_with_fix), (True, True))

    def test_a_fix_that_turns_a_folder_into_a_file_still_proves(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo, fix, _ = toy_history(tmp, fix_turns_a_folder_into_a_file=True)
            proof = replay.prove(repo, replay.load_case(repo, fix, 7), Path(tmp, "proof"))
            self.assertEqual((proof.fails_without_fix, proof.passes_with_fix), (True, True))

    def test_gates_miss_a_bug_only_the_new_test_sees(self):
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("gates", self.repo, case, ISSUE, self.root / "gates")
        self.assertEqual(result.outcome, "missed")

    def test_gates_catch_a_bug_that_breaks_an_existing_test(self):
        other = self.root / "other"
        other.mkdir()
        repo, fix, _ = toy_history(other, bug_breaks_old_test=True)
        case = replay.load_case(repo, fix, 7)
        result = replay.run_stage("gates", repo, case, ISSUE, self.root / "gates2")
        self.assertEqual(result.outcome, "caught")

    def test_gates_judge_the_fix_with_its_own_test_changes_so_an_old_test_of_the_bug_is_no_false_alarm(self):
        other = self.root / "other"
        other.mkdir()
        repo, fix, _ = toy_history(other, bug_breaks_old_test=True, old_test_expects_bug=True)
        case = replay.load_case(repo, fix, 7)
        result = replay.run_stage("gates", repo, case, ISSUE, self.root / "gates2")
        self.assertEqual(result.outcome, "caught")

    def test_gates_catch_a_bug_when_one_gate_fails_only_on_it_while_another_fails_on_both_trees(self):
        other = self.root / "other"
        other.mkdir()
        repo, fix, _ = toy_history(other, bug_breaks_old_test=True, broken_folder_the_fix_touches=True)
        case = replay.load_case(repo, fix, 7)
        result = replay.run_stage("gates", repo, case, ISSUE, self.root / "gates2")
        self.assertEqual((result.outcome, result.findings, result.other_findings), ("caught", 2, 1))

    def test_gate_time_leaves_out_the_wait_for_other_cases(self):
        case = replay.load_case(self.repo, self.fix, 7)
        replay.COMMAND_LOCK.acquire()
        threading.Timer(1.5, replay.COMMAND_LOCK.release).start()
        result = replay.run_stage("gates", self.repo, case, ISSUE, self.root / "gates")
        self.assertLess(result.seconds, 1.0)

    def test_a_gate_that_fails_once_under_load_and_then_passes_is_no_failure(self):
        marker = self.root / "ran-once"
        flaky = textwrap.indent(textwrap.dedent(f"""
            def test_slow_under_load(self):
                import pathlib
                first = not pathlib.Path({str(marker)!r}).exists()
                pathlib.Path({str(marker)!r}).touch()
                self.assertFalse(first)
            """), "    ")
        other = self.root / "other"
        other.mkdir()
        repo, fix, _ = toy_history(other, extra_old_test=flaky)
        case = replay.load_case(repo, fix, 7)
        result = replay.run_stage("gates", repo, case, ISSUE, self.root / "gates2")
        self.assertEqual(result.outcome, "missed")

    def test_a_model_stage_reviews_the_stage_repo_and_a_judge_scores_it_against_the_fix(self):
        os.environ["FAKE_FINDINGS"] = json.dumps([
            {"file": "README.md", "line": 1, "summary": "typo"},
            {"file": "lib/calc.py", "line": 2, "summary": "add subtracts"}])
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("code-review", self.repo, case, ISSUE, self.root / "cr", claude=str(self.claude))
        self.assertEqual(result.outcome, "caught")
        self.assertEqual(result.findings, 2)
        self.assertEqual(result.other_findings, 1)
        self.assertEqual(result.tokens, 100)
        self.assertEqual(result.cost_usd, 0.5)
        self.assertEqual(result.leaks, [])
        stage_call, judge_call = self.claude_calls()
        self.assertTrue(stage_call["prompt"].startswith("/code-review:code-review "))
        self.assertIn(ISSUE[1], stage_call["prompt"])
        self.assertNotIn(self.fix[:7], json.dumps(stage_call))
        self.assertNotIn("test_add", json.dumps(stage_call))
        self.assertNotIn(str(self.repo), stage_call["cwd"])
        self.assertIn("return a + b", judge_call["prompt"])

    def test_a_stage_with_no_findings_is_a_miss_with_no_judge_call(self):
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("thermos", self.repo, case, ISSUE, self.root / "t", claude=str(self.claude))
        self.assertEqual((result.outcome, result.findings), ("missed", 0))
        self.assertEqual(len(self.claude_calls()), 1)

    def test_a_transcript_that_names_the_fix_or_the_repo_is_a_leak(self):
        os.environ["FAKE_TRANSCRIPT_EXTRA"] = f"git show {self.fix[:7]} in {self.repo}"
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("interrogate", self.repo, case, ISSUE, self.root / "i", claude=str(self.claude))
        self.assertEqual(result.outcome, "leaked")
        self.assertEqual(sorted(result.leaks), sorted(["fix commit", "source repo path"]))

    def test_a_tool_that_returns_a_line_of_the_fix_is_a_leak(self):
        os.environ["FAKE_TOOL_RESULT"] = json.dumps({"name": "Bash", "output": "+        self.assertEqual(add(2, 3), 5)"})
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("thermos", self.repo, case, ISSUE, self.root / "t", claude=str(self.claude))
        self.assertEqual((result.outcome, result.leaks), ("leaked", ["fixed code"]))

    def test_proposing_the_fix_in_its_own_words_or_through_a_subagent_is_not_a_leak(self):
        os.environ["FAKE_TRANSCRIPT_EXTRA"] = "add should be: return a + b"
        os.environ["FAKE_TOOL_RESULT"] = json.dumps({"name": "Agent", "output": "fix it with `return a + b`"})
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("thermos", self.repo, case, ISSUE, self.root / "t", claude=str(self.claude))
        self.assertEqual((result.outcome, result.leaks), ("missed", []))

    def test_no_bash_command_a_stage_may_run_prints_a_file_outside_its_repo(self):
        def allowed(command):
            for rule in replay.STAGE_READ_TOOLS:
                match = re.fullmatch(r"Bash\((.+?)(:\*)?\)", rule)
                if match and (command == match[1] or (match[2] and command.startswith(match[1] + " "))):
                    return True
            return False

        stage = self.root / "stage"
        stage.mkdir()
        git(stage, "init", "-q", "-b", "main")
        git(stage, "config", "user.email", "t@example.com")
        git(stage, "config", "user.name", "t")
        commit(stage, {"a.txt": "a\n"}, "base")
        git(stage, "checkout", "-qb", "work")
        commit(stage, {"a.txt": "b\n"}, "work")
        (self.root / "secret.txt").write_text("THE ANSWER KEY\n")
        self.assertTrue(allowed("git diff main...HEAD"))
        attempts = ["git diff --no-index /dev/null ../secret.txt", "git diff /dev/null ../secret.txt",
                    "git diff --stat --no-index /dev/null ../secret.txt",
                    "git diff main...HEAD /dev/null ../secret.txt", "git diff main...HEAD -- ../secret.txt",
                    "git diff main...HEAD --no-index /dev/null ../secret.txt",
                    "git log -p main..HEAD -- ../secret.txt", "git show HEAD -- ../secret.txt",
                    "git show --no-index /dev/null ../secret.txt", "git status ../secret.txt"]
        for command in filter(allowed, attempts):
            out = subprocess.run(command.split(), cwd=stage, capture_output=True, text=True)
            self.assertNotIn("THE ANSWER KEY", out.stdout + out.stderr, command)

    def test_a_refused_tool_call_whose_refusal_names_the_repo_is_not_a_leak(self):
        os.environ["FAKE_DENIAL"] = f"grep was denied by a deny rule (/{self.repo}/**)"
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("thermos", self.repo, case, ISSUE, self.root / "t", claude=str(self.claude))
        self.assertEqual(result.leaks, [])
        self.assertEqual(result.outcome, "missed")

    def test_no_tree_holding_the_fix_is_on_disk_when_a_model_stage_starts(self):
        other = self.root / "other"
        other.mkdir()
        repo, fix, _ = toy_history(other, bug_breaks_old_test=True)
        scratch = self.root / "scratch"
        scratch.mkdir()
        os.environ["FAKE_SCAN_ROOT"] = str(scratch)
        replay.run_replay(repo, [{"fix": fix, "issue": 7}], ["gates", "code-review"], self.root / "results", scratch,
                          claude=str(self.claude), read_issue=lambda number: ISSUE, date="2026-11-16")
        stage_call = next(call for call in self.claude_calls() if "stream-json" in call["argv"])
        self.assertEqual(stage_call["fix_on_disk"], [])

    def test_a_stage_may_not_read_another_cases_answer_key_and_naming_one_is_a_leak(self):
        scratch = self.root / "scratch"
        os.environ["FAKE_TRANSCRIPT_EXTRA"] = f"ls {scratch}/{replay.ANSWER_KEY_PREFIX}abc/tree"
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("interrogate", self.repo, case, ISSUE, scratch / "case" / "interrogate",
                                  claude=str(self.claude), scratch_root=scratch)
        self.assertEqual(result.outcome, "leaked")
        self.assertEqual(result.leaks, ["answer key folder"])
        denied = self.claude_calls()[0]["argv"]
        self.assertIn(f"Read(/{scratch}/{replay.ANSWER_KEY_PREFIX}*/**)", denied)

    def test_totals_leave_out_leaked_stages_and_cases_whose_proof_failed(self):
        def case(fix, proof_passes, outcome):
            result = replay.StageResult(outcome, tokens=10, cost_usd=1.0, seconds=60)
            return {"case": {"fix": fix, "issue": 7}, "area": "python",
                    "proof": {"fails_without_fix": True, "passes_with_fix": proof_passes},
                    "stages": {"code-review": replay.asdict(result)}}
        run = {"date": "2026-11-17", "stages": ["code-review"], "errors": [],
               "cases": [case("a" * 40, True, "caught"), case("b" * 40, True, "leaked"),
                         case("c" * 40, False, "caught"), case("d" * 40, True, "missed")]}
        table = replay.render(run, run)
        self.assertIn("| code-review | 1/2 | 1 | 0 | 0 | 20 | 2.00 | 0.00 | 2 min |", table)
        self.assertIn("| code-review | 1/2 | 1/2 |", table)

    def test_a_case_that_fails_to_start_is_listed_and_the_other_cases_are_still_written(self):
        def read_issue(number):
            if number == 8:
                raise subprocess.CalledProcessError(1, ["gh", "issue", "view"])
            return ISSUE
        cases = [{"fix": self.fix, "issue": 8}, {"fix": self.fix, "issue": 7}]
        (self.root / "scratch").mkdir()
        table = replay.run_replay(self.repo, cases, ["gates"], self.root / "results", self.root / "scratch",
                                  read_issue=read_issue, date="2026-11-16").read_text()
        self.assertIn("| 7 | `" + self.fix[:7] + "` | python | pass | missed |", table)
        self.assertNotIn("| 8 |", table)
        self.assertIn(f"- {self.fix[:7]}: ", table)

    def test_a_stage_may_not_read_the_repos_other_worktrees_or_the_build_cache(self):
        worktree = self.root / "elsewhere"
        git(self.repo, "worktree", "add", "-q", "-b", "side", str(worktree))
        os.environ["FAKE_TRANSCRIPT_EXTRA"] = f"cat {worktree}/lib/calc.py; ls {replay.CACHE}/target"
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("thermos", self.repo, case, ISSUE, self.root / "case" / "thermos",
                                  claude=str(self.claude))
        self.assertEqual(sorted(result.leaks), ["build cache", "source repo path"])
        denied = self.claude_calls()[0]["argv"]
        self.assertIn(f"Read(/{worktree.resolve()}/**)", denied)
        self.assertIn(f"Grep(/{replay.CACHE}/**)", denied)

    def test_an_issue_that_names_the_fix_commit_is_not_a_case(self):
        case = replay.load_case(self.repo, self.fix, 7)
        with self.assertRaisesRegex(replay.CaseError, "issue names the fix"):
            replay.build_stage_repo(self.repo, case, self.root / "stage", ("Add", f"Fixed in {self.fix[:7]}."))

    def test_asking_for_a_case_that_is_not_listed_is_an_error(self):
        cases = [{"fix": "abc1234", "issue": 1}, {"fix": "def5678", "issue": 2}]
        self.assertEqual(replay.select_cases(cases, "def"), [cases[1]])
        with self.assertRaisesRegex(replay.CaseError, "not in cases.json: 9999"):
            replay.select_cases(cases, "abc,9999")

    def test_notes_carry_no_em_dash(self):
        result = replay.StageResult("missed", note="no finding \u2014 they cover other code")
        run = {"date": "2026-11-17", "stages": ["thermos"], "errors": [],
               "cases": [{"case": {"fix": "a" * 40, "issue": 7}, "area": "python",
                          "proof": {"fails_without_fix": True, "passes_with_fix": True},
                          "stages": {"thermos": replay.asdict(result)}}]}
        table = replay.render(run, None)
        self.assertNotIn("\u2014", table)
        self.assertIn("no finding; they cover other code", table)

    def test_naming_another_cases_folder_is_a_leak_and_naming_its_own_is_not(self):
        scratch = self.root / "scratch"
        case = replay.load_case(self.repo, self.fix, 7)
        own = scratch / "grove-replay-own_1"
        # Claude names its log folder after the path, with every other character as "-".
        logs = "~/.claude/projects/-private-var-grove-replay-own-1-thermos-repo"
        outcomes = {}
        for label, path in (("own", own / "thermos" / "repo"), ("own log", logs),
                            ("sibling", scratch / "grove-replay-own_12" / "x")):
            os.environ["FAKE_TRANSCRIPT_EXTRA"] = f"cat {path}/lib/calc.py"
            result = replay.run_stage("thermos", self.repo, case, ISSUE, own / f"thermos-{label}",
                                      claude=str(self.claude), scratch_root=scratch)
            outcomes[label] = result.leaks
        self.assertEqual(outcomes, {"own": [], "own log": [], "sibling": ["another case's folder"]})

    def test_a_proof_rebuilds_python_bytecode_when_the_fix_has_the_same_size_and_second(self):
        tree = self.root / "tree"
        replay.export_tree(self.repo, f"{self.fix}^", tree)
        replay.overlay(self.repo, self.fix, ("lib/test_calc.py",), tree)
        test = [("lib", [sys.executable, "-m", "unittest", "test_calc"])]
        clock, no_bytecode = time.time, os.environ.pop("PYTHONDONTWRITEBYTECODE", None)
        replay.time.time = lambda: 1_800_000_000.25
        try:
            self.assertEqual(len(replay.run_commands(tree, test)[0]), 1)
            replay.overlay(self.repo, self.fix, ("lib/calc.py",), tree)
            self.assertEqual(replay.run_commands(tree, test)[0], [])
        finally:
            replay.time.time = clock
            if no_bytecode is not None:
                os.environ["PYTHONDONTWRITEBYTECODE"] = no_bytecode

    def test_each_model_stage_may_not_read_another_cases_folder(self):
        scratch = self.root / "scratch"
        scratch.mkdir()
        os.environ["FAKE_FINDINGS"] = "[]"
        cases = [{"fix": self.fix, "issue": 7}, {"fix": self.fix, "issue": 8}]
        replay.run_replay(self.repo, cases, ["code-review"], self.root / "results", scratch,
                          claude=str(self.claude), read_issue=lambda number: ISSUE, date="2026-11-16")
        stage_calls = [call for call in self.claude_calls() if "stream-json" in call["argv"]]
        folders = [Path(call["cwd"]).parents[1] for call in stage_calls]
        self.assertEqual(len(set(folders)), 2)
        for call, own, other in zip(stage_calls, folders, reversed(folders)):
            self.assertIn(f"Read(/{scratch / other.name}/**)", call["argv"])
            self.assertNotIn(f"Read(/{scratch / own.name}/**)", call["argv"])

    def test_a_stage_runs_restricted_with_only_read_tools_and_only_the_plugins_it_needs(self):
        os.environ["FAKE_FINDINGS"] = "[]"
        case = replay.load_case(self.repo, self.fix, 7)
        replay.run_stage("thermos", self.repo, case, ISSUE, self.root / "t", claude=str(self.claude))
        argv = next(call for call in self.claude_calls() if "stream-json" in call["argv"])["argv"]
        self.assertIn("--restricted", argv)
        tools = argv[argv.index("--tools") + 1].split(",")
        self.assertIn("Bash", tools)
        self.assertFalse({"Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch"} & set(tools))
        plugins = [argv[i + 1] for i, arg in enumerate(argv) if arg == "--plugin-dir"]
        self.assertEqual(sorted(plugins), ["/plugins/code-review", "/plugins/pstack"])
        readable = [argv[i + 1] for i, arg in enumerate(argv) if arg == "--add-dir"]
        self.assertEqual(sorted(readable), ["/plugins/code-review", "/plugins/pstack"])

    def test_a_stage_runs_its_commands_in_a_sandbox_that_may_not_write_to_any_case_folder(self):
        os.environ["FAKE_FINDINGS"] = "[]"
        scratch = self.root / "scratch"
        scratch.mkdir()
        replay.run_replay(self.repo, [{"fix": self.fix, "issue": 7}], ["thermos"], self.root / "results", scratch,
                          claude=str(self.claude), read_issue=lambda number: ISSUE, date="2026-11-18")
        call = next(call for call in self.claude_calls() if "stream-json" in call["argv"])
        sandbox = json.loads(call["argv"][call["argv"].index("--settings") + 1])["sandbox"]
        self.assertEqual((sandbox["enabled"], sandbox["allowUnsandboxedCommands"]), (True, False))
        self.assertIn(str(scratch.resolve()), sandbox["filesystem"]["denyWrite"])
        self.assertTrue(Path(call["cwd"]).resolve().is_relative_to(scratch.resolve()))
        no_reads = sandbox["filesystem"]["denyRead"]
        for folder in (scratch.resolve(), self.repo.resolve(), replay.CACHE.resolve(), Path.home() / ".claude" / "projects"):
            self.assertIn(str(folder), no_reads)
        self.assertIn(str(Path(call["cwd"]).parent.resolve()), sandbox["filesystem"]["allowRead"])

    def test_a_line_the_branch_changed_and_the_fix_restored_from_main_is_not_a_leak(self):
        for a_file_named_main in (False, True):
            with self.subTest(a_file_named_main=a_file_named_main), tempfile.TemporaryDirectory() as tmp:
                repo, fix, _ = toy_history(tmp, main_has_the_good_add=True, a_file_named_main=a_file_named_main)
                os.environ["FAKE_TOOL_RESULT"] = json.dumps({"name": "Bash", "output": "-    return a + b"})
                case = replay.load_case(repo, fix, 7)
                result = replay.run_stage("thermos", repo, case, ISSUE, Path(tmp, "t"), claude=str(self.claude))
                self.assertEqual((result.outcome, result.leaks), ("missed", []))

    def test_overlaying_the_fix_also_sets_a_file_mode_the_fix_changed(self):
        commit(self.repo, {"tool.sh": "#!/bin/sh\necho ok\n"}, "add tool")
        before = git(self.repo, "rev-parse", "HEAD")
        Path(self.repo, "tool.sh").chmod(0o755)
        git(self.repo, "commit", "-qam", "make tool executable")
        tree = self.root / "tree"
        replay.export_tree(self.repo, before, tree)
        self.assertFalse(os.access(tree / "tool.sh", os.X_OK))
        replay.overlay(self.repo, "HEAD", ["tool.sh"], tree)
        self.assertTrue(os.access(tree / "tool.sh", os.X_OK))
        replay.overlay(self.repo, before, ["tool.sh"], tree)
        self.assertFalse(os.access(tree / "tool.sh", os.X_OK))

    def test_overlaying_a_file_over_a_symlink_leaves_the_old_link_target_alone(self):
        commit(self.repo, {"helper.sh": "#!/bin/sh\n"}, "add helper")
        Path(self.repo, "helper.sh").chmod(0o755)
        Path(self.repo, "tool.sh").symlink_to("helper.sh")
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-qm", "link tool to helper")
        before = git(self.repo, "rev-parse", "HEAD")
        Path(self.repo, "tool.sh").unlink()
        commit(self.repo, {"tool.sh": "plain text\n"}, "tool is a plain file")
        tree = self.root / "tree"
        replay.export_tree(self.repo, before, tree)
        replay.overlay(self.repo, "HEAD", ["tool.sh"], tree)
        self.assertFalse((tree / "tool.sh").is_symlink())
        self.assertEqual((tree / "tool.sh").read_text(), "plain text\n")
        self.assertEqual((tree / "helper.sh").read_text(), "#!/bin/sh\n")
        self.assertTrue(os.access(tree / "helper.sh", os.X_OK))
        replay.overlay(self.repo, before, ["tool.sh"], tree)
        self.assertEqual(os.readlink(tree / "tool.sh"), "helper.sh")

    def test_git_commands_act_on_the_repo_they_name_when_a_hook_set_git_dir(self):
        decoy = self.root / "decoy"
        decoy.mkdir()
        git(decoy, "init", "-q")
        git(decoy, "config", "user.email", "t@example.com")
        git(decoy, "config", "user.name", "t")
        git(decoy, "commit", "-q", "--allow-empty", "-m", "decoy")
        env = {**os.environ, "GIT_DIR": str(decoy / ".git"), "GIT_WORK_TREE": str(decoy)}
        script = "import sys, replay; replay.git(sys.argv[1], 'commit', '-q', '--allow-empty', '-m', 'here')"
        subprocess.run([sys.executable, "-c", script, str(self.repo)], env=env, check=True,
                       cwd=Path(replay.__file__).parent)
        self.assertEqual(git(decoy, "log", "-1", "--format=%s"), "decoy")
        self.assertEqual(git(self.repo, "log", "-1", "--format=%s"), "here")

    def test_a_stage_limited_to_a_case_that_is_not_selected_is_an_error_before_any_work(self):
        cases = [{"fix": self.fix, "issue": 7}]
        with self.assertRaisesRegex(replay.CaseError, "code-review.*not in the selected cases: 9999"):
            replay.run_replay(self.repo, cases, {"code-review": ["9999"], "gates": None}, self.root / "results",
                              self.root, claude=str(self.claude), read_issue=lambda number: ISSUE)
        self.assertEqual(self.claude_calls(), [])
        self.assertEqual(list(self.root.glob("grove-replay-*")), [])

    def test_an_empty_sha_in_a_stage_limit_is_an_error_and_no_limit_runs_every_case(self):
        self.assertEqual(replay.parse_stage_args(["code-review"]), {"code-review": None})
        with self.assertRaisesRegex(replay.CaseError, "empty sha"):
            replay.parse_stage_args(["thermos=", "thermos"])
        for value in ("code-review=", f"code-review={self.fix[:7]},"):
            with self.subTest(value=value), self.assertRaisesRegex(replay.CaseError, "empty sha"):
                replay.run_replay(self.repo, [{"fix": self.fix, "issue": 7}], replay.parse_stage_args([value]),
                                  self.root / "results", self.root, claude=str(self.claude),
                                  read_issue=lambda number: ISSUE)

    def test_a_plugin_list_that_hangs_ends_the_stage_as_an_error(self):
        os.environ["FAKE_PLUGIN_LIST_HANGS"] = "1"
        scratch = self.root / "scratch"
        scratch.mkdir()
        original = replay.PLUGIN_LIST_TIMEOUT_SECONDS
        replay.PLUGIN_LIST_TIMEOUT_SECONDS = 1
        self.addCleanup(setattr, replay, "PLUGIN_LIST_TIMEOUT_SECONDS", original)
        started = time.monotonic()
        replay.run_replay(self.repo, [{"fix": self.fix, "issue": 7}], ["thermos"], self.root / "results", scratch,
                          claude=str(self.claude), read_issue=lambda number: ISSUE, date="2026-11-18")
        self.assertLess(time.monotonic() - started, 20)
        run = json.loads((self.root / "results" / "2026-11-18.json").read_text())
        self.assertEqual(run["cases"][0]["stages"]["thermos"]["outcome"], "error")
        self.assertIn("plugin list did not answer", run["cases"][0]["stages"]["thermos"]["note"])

    def test_the_gauntlet_is_not_available_yet(self):
        case = replay.load_case(self.repo, self.fix, 7)
        result = replay.run_stage("verify-gauntlet", self.repo, case, ISSUE, self.root / "g")
        self.assertEqual(result.outcome, "not available")

    def test_a_run_writes_its_table_compares_with_the_last_run_and_leaves_no_scratch(self):
        scratch = self.root / "scratch"
        scratch.mkdir()
        results = self.root / "results"
        os.environ["FAKE_FINDINGS"] = json.dumps([{"file": "lib/calc.py", "line": 2, "summary": "add subtracts"}])
        cases = [{"fix": self.fix, "issue": 7}]
        common = dict(repo=self.repo, cases=cases, results_dir=results, scratch_root=scratch,
                      claude=str(self.claude), read_issue=lambda number: ISSUE)
        first = replay.run_replay(stages=["gates", "code-review", "verify-gauntlet"], date="2026-11-16", **common)
        self.assertEqual(first, results / "2026-11-16.md")
        table = first.read_text()
        self.assertIn("| 7 | `" + self.fix[:7] + "` | python | pass | missed | caught | not available |", table)
        self.assertIn("No earlier run", table)
        os.environ["FAKE_FINDINGS"] = "[]"
        second = replay.run_replay(stages=["code-review"], date="2026-11-16", **common)
        self.assertEqual(second, results / "2026-11-16-2.md")
        self.assertIn("| code-review | 0/1 | 1/1 |", second.read_text())
        self.assertEqual(list(scratch.iterdir()), [])
        self.assertTrue((results / "2026-11-16.json").exists())


if __name__ == "__main__":
    unittest.main()
