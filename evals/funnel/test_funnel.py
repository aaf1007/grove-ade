# Tests for the live funnel. Run: python3 -m unittest discover evals/funnel
import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import funnel  # noqa: E402

REVIEWER_SUMMARY = ("## Review of issue #12\n\n| Gate | Where | Result |\n| --- | --- | --- |\n"
                    "| `cargo test` | . | FAIL |\n| `eslint` | app | PASS |\n\nCommit reviewed: abc123")


def comment(login, body, review_body=None):
    return {"author": {"login": login}, "body": body, "url": "https://github.com/x/y/pull/1#c",
            "pullRequestReview": {"body": review_body} if review_body is not None else None}


def thread(*comments, resolved=True, outdated=False):
    return {"isResolved": resolved, "isOutdated": outdated, "comments": {"nodes": list(comments)}}


def pull_request(number=20, title="#12 feat: a thing", threads=(), reviews=(), comments=(), body="",
                 created="2026-11-10T10:00:00Z", merged="2026-11-11T10:00:00Z", commits=()):
    return {"number": number, "title": title, "url": f"https://github.com/x/y/pull/{number}", "body": body,
            "createdAt": created, "mergedAt": merged,
            "commits": {"nodes": [{"commit": {"committedDate": d, "messageHeadline": h}} for d, h in commits]},
            "reviewThreads": {"nodes": list(threads)}, "reviews": {"nodes": list(reviews)},
            "comments": {"nodes": list(comments)}}


class FunnelTest(unittest.TestCase):
    def defects(self, pr, issues=()):
        return funnel.analyze([pr], list(issues))[0]["defects"]

    def test_a_bot_thread_counts_only_when_the_reply_says_it_was_fixed(self):
        pr = pull_request(threads=[
            thread(comment("coderabbitai", "Race here"), comment("aaf1007", "**Fixed** in abc: lock first.")),
            thread(comment("coderabbitai", "Rename x"), comment("aaf1007", "Not fixing: style only.")),
            thread(comment("cursor", "Off by one"), comment("aaf1007", "Real: deferred to issue #30.")),
            thread(comment("coderabbitai", "Hmm"), resolved=False)])
        record = funnel.analyze([pr], [])[0]
        self.assertEqual(record["defects"], {"coderabbit": 1, "cursor": 1})
        self.assertEqual(len(record["unclear"]), 1)

    def test_an_unanswered_thread_whose_code_changed_and_was_resolved_is_a_defect(self):
        pr = pull_request(threads=[thread(comment("coderabbitai", "Leak"), resolved=True, outdated=True)])
        self.assertEqual(self.defects(pr), {"coderabbit": 1})

    def test_reviewer_threads_and_owner_threads_are_told_apart_by_the_reviewer_summary(self):
        pr = pull_request(threads=[
            thread(comment("aaf1007", "Logic in src-tauri", review_body=REVIEWER_SUMMARY),
                   comment("aaf1007", "Moved into the core.")),
            thread(comment("aaf1007", "This label reads wrong", review_body=""), comment("aaf1007", "Fixed."))])
        self.assertEqual(self.defects(pr), {"reviewer": 1, "owner": 1})

    def test_each_failing_gate_in_a_reviewer_summary_is_a_gates_defect(self):
        pr = pull_request(reviews=[{"author": {"login": "aaf1007"}, "body": REVIEWER_SUMMARY}])
        self.assertEqual(self.defects(pr), {"gates": 1})

    def test_the_caught_by_column_of_the_template_counts_stages_before_the_pull_request(self):
        body = ("## Findings\n\n| Finding | Category | Caught by |\n| --- | --- | --- |\n"
                "| Missing await | bug | interrogate |\n| Wrong color | bug | QA |\n| Typo | bug | someone |\n")
        self.assertEqual(self.defects(pull_request(body=body)), {"interrogate": 1, "qa": 1})

    def test_the_template_adds_only_stages_that_run_before_the_pull_request(self):
        body = ("| Finding | Category | Caught by |\n| --- | --- | --- |\n| Race | bug | coderabbit |\n"
                "| Label | ui | owner |\n| Missing await | bug | interrogate |\n")
        pr = pull_request(body=body, threads=[
            thread(comment("coderabbitai", "Race here"), comment("aaf1007", "Fixed.")),
            thread(comment("aaf1007", "This label reads wrong", review_body=""), comment("aaf1007", "Fixed."))])
        self.assertEqual(self.defects(pr), {"coderabbit": 1, "owner": 1, "interrogate": 1})

    def test_only_bug_rows_of_the_template_are_defects(self):
        body = ("| Finding | Category | Caught by |\n| --- | --- | --- |\n| Extract helper | refactor | thermos |\n"
                "| Missing await | Bug | interrogate |\n")
        self.assertEqual(self.defects(pull_request(body=body)), {"interrogate": 1})

    def test_a_cause_in_another_repository_is_no_escape(self):
        issues = [{"number": 40, "title": "x", "url": "u", "labels": [{"name": "bug"}],
                   "body": "Caused by https://github.com/other/project/pull/20."},
                  {"number": 41, "title": "y", "url": "u", "labels": [{"name": "bug"}],
                   "body": "Caused by https://github.com/x/y/pull/20."}]
        self.assertEqual(self.defects(pull_request(), issues), {"escaped": 1})

    def test_a_pull_request_with_more_than_one_page_of_threads_is_listed_not_counted(self):
        pr = pull_request(threads=[thread(comment("coderabbitai", "Bug"), comment("aaf1007", "Fixed."))])
        pr["reviewThreads"]["pageInfo"] = {"hasNextPage": True}
        table = funnel.render(funnel.analyze([pr], []), "2026-11-01", "2026-11-16")
        self.assertNotIn("| All |", table)
        self.assertIn("Not counted", table)
        self.assertIn(pr["url"], table)

    def test_the_table_is_named_by_the_last_day_it_covers(self):
        with tempfile.TemporaryDirectory() as folder:
            results, fetch = funnel.RESULTS_DIR, funnel.fetch
            funnel.RESULTS_DIR, funnel.fetch = Path(folder), lambda repo, since, until: ([], [])
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    funnel.main(["--since", "2026-10-01", "--until", "2026-10-31"])
            finally:
                funnel.RESULTS_DIR, funnel.fetch = results, fetch
            self.assertEqual(os.listdir(folder), ["funnel-2026-10-31.md"])

    def test_a_bug_issue_that_names_the_issue_or_pull_request_behind_it_is_an_escape(self):
        issues = [{"number": 40, "title": "Crash on start", "url": "u", "body": "Caused by #12 (the thing).",
                   "labels": [{"name": "bug"}]},
                  {"number": 41, "title": "Slow", "url": "u", "body": "Regressed in PR #20.",
                   "labels": [{"name": "dogfood"}]},
                  {"number": 42, "title": "Odd", "url": "u", "body": "First seen on #20.", "labels": [{"name": "bug"}]}]
        self.assertEqual(self.defects(pull_request(), issues), {"escaped": 2})

    def test_a_fix_round_is_a_review_the_author_answered_with_a_commit(self):
        def review(who, at, body="Actionable comments posted: 1", state="COMMENTED"):
            return {"author": {"login": who}, "body": body, "state": state, "submittedAt": at}
        pr = pull_request(created="2026-11-10T10:00:00Z", reviews=[
            review("coderabbitai", "2026-11-10T10:05:00Z", body="Actionable comments posted: 0"),
            review("coderabbitai", "2026-11-10T10:30:00Z"),
            review("aaf1007", "2026-11-10T11:10:00Z", body=""),
            review("coderabbitai", "2026-11-10T12:30:00Z"),
            review("aaf1007", "2026-11-10T14:00:00Z", body=REVIEWER_SUMMARY.replace("FAIL", "PASS"))],
            commits=[("2026-11-10T09:00:00Z", "feat: first"), ("2026-11-10T10:10:00Z", "feat: more"),
                     ("2026-11-10T11:00:00Z", "fix: a"), ("2026-11-10T11:25:00Z", "fix: b"),
                     ("2026-11-10T13:00:00Z", "Merge remote-tracking branch 'origin/main'"),
                     ("2026-11-10T15:00:00Z", "fix: c")])
        self.assertEqual(funnel.analyze([pr], [])[0]["fix_rounds"], 2)

    def test_an_implementer_note_from_the_owner_login_is_not_an_owner_finding(self):
        pr = pull_request(threads=[thread(comment("aaf1007", "Removed in 11cf0ed. The test was there for one run.",
                                                  review_body=""), comment("coderabbitai", "Thanks for the update."),
                                          resolved=True, outdated=True)])
        self.assertEqual(self.defects(pr), {})

    def test_the_weekly_table_sums_stages_and_what_reached_the_owner(self):
        prs = [pull_request(number=20, merged="2026-11-10T10:00:00Z", body="Cost (USD): $1.50", threads=[
                   thread(comment("aaf1007", "Wrong", review_body=""), comment("aaf1007", "Fixed."))]),
               pull_request(number=21, title="#13 fix: b", merged="2026-11-12T10:00:00Z", threads=[
                   thread(comment("coderabbitai", "Bug"), comment("aaf1007", "Fixed."))])]
        issues = [{"number": 50, "title": "x", "url": "u", "body": "Introduced by #21.", "labels": [{"name": "bug"}]}]
        table = funnel.render(funnel.analyze(prs, issues), "2026-11-01", "2026-11-16")
        columns = " | ".join(funnel.STAGES)
        self.assertIn(f"| Week | PRs | {columns} | Reached owner | Fix rounds | Cost (USD) |", table)
        zeros = dict.fromkeys(funnel.STAGES, 0) | {"coderabbit": 1, "owner": 1, "escaped": 1}
        cells = " | ".join(str(zeros[s]) for s in funnel.STAGES)
        self.assertIn(f"| 2026-11-09 | 2 | {cells} | 2 | 0.0 | 1.50 (1 PR) |", table)

    def test_fetching_only_reads_from_github(self):
        calls = []

        def gh(args):
            calls.append(args)
            if args[:2] == ["api", "graphql"]:
                return json.dumps({"data": {"search": {"pageInfo": {"hasNextPage": False, "endCursor": None},
                                                       "nodes": [pull_request()]}}})
            return "[]"

        prs, issues = funnel.fetch("aaf1007/grove-ade", "2026-10-16", "2026-11-16", gh=gh)
        self.assertEqual([p["number"] for p in prs], [20])
        for args in calls:
            self.assertIn(args[:2], (["api", "graphql"], ["issue", "list"]))
            self.assertNotIn("mutation", " ".join(args))
            self.assertNotIn("--method", args)


if __name__ == "__main__":
    unittest.main()
