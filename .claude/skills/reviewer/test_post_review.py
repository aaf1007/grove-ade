"""Tests for post_review.py. Run: python3 -m unittest discover .claude/skills/reviewer"""

import io
import json
import os
import tempfile
import unittest
from contextlib import redirect_stdout
from datetime import datetime, timedelta

import post_review

REPO = "owner/repo"
PR = 7
HEAD = "a" * 40


class FakeGitHub:
    """Answers the reads from fixed data and records every write."""

    def __init__(self, head=HEAD, threads=(), comment_bodies=()):
        self.head = head
        self.threads = list(threads)
        self.comment_bodies = list(comment_bodies)
        self.writes = []
        self.activity = None
        self.head_after_threads = None

    def coderabbit_activity(self, repo, pr):
        return self.activity

    def pull_request_head(self, repo, pr):
        return self.head

    def pull_request_comment_bodies(self, repo, pr):
        return self.comment_bodies

    def review_threads(self, repo, pr):
        if self.head_after_threads:
            self.head = self.head_after_threads
        return self.threads

    def viewer_login(self):
        return "owner"

    def create_review(self, repo, pr, payload):
        self.writes.append(("create_review", payload))

    def reply_to_thread(self, thread_id, body):
        self.writes.append(("reply_to_thread", thread_id, body))

    def resolve_thread(self, thread_id):
        self.writes.append(("resolve_thread", thread_id))

    def comment(self, repo, pr, body):
        self.writes.append(("comment", body))


def reviewer_thread(thread_id, path="src/lib.rs", line=3, author="owner"):
    body = "Bug here.\n\n" + post_review.THREAD_MARKER
    return {"id": thread_id, "path": path, "line": line, "isResolved": False, "author": author, "first_comment": body}


def owner_thread(thread_id):
    return {"id": thread_id, "path": "a.ts", "line": 1, "isResolved": False, "author": "owner", "first_comment": "Owner note"}


REVIEW = {
    "commit": HEAD,
    "summary": "## Review\n\nGates: all pass.",
    "findings": [
        {"path": "crates/grove-core/src/lib.rs", "line": 12, "body": "unwrap() outside tests"},
        {"path": "app/src/App.tsx", "line": 40, "body": "Off by one"},
    ],
}

RECHECK = {
    "commit": HEAD,
    "summary": "## Recheck\n\nBoth fixed.",
    "replies": [{"thread_id": "T1", "body": "Fixed in the new commit.", "resolve": True}],
}


def run(command, github, data, dry_run=False):
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as file:
        json.dump(data, file)
    output = io.StringIO()
    try:
        with redirect_stdout(output):
            post_review.run(command, REPO, PR, file.name, github, dry_run)
    finally:
        os.unlink(file.name)
    return output.getvalue()


class PostTests(unittest.TestCase):
    def test_dry_run_prints_every_finding_and_writes_nothing(self):
        github = FakeGitHub()
        output = run("post", github, REVIEW, dry_run=True)
        self.assertEqual(github.writes, [])
        self.assertIn("crates/grove-core/src/lib.rs:12", output)
        self.assertIn("app/src/App.tsx:40", output)
        self.assertIn(HEAD, output)

    def test_post_opens_one_thread_per_finding_in_one_review(self):
        github = FakeGitHub()
        run("post", github, REVIEW)
        self.assertEqual(len(github.writes), 1)
        kind, payload = github.writes[0]
        self.assertEqual(kind, "create_review")
        self.assertEqual(payload["commit_id"], HEAD)
        self.assertEqual(payload["event"], "COMMENT")
        self.assertEqual(
            [(c["path"], c["line"], c["side"]) for c in payload["comments"]],
            [("crates/grove-core/src/lib.rs", 12, "RIGHT"), ("app/src/App.tsx", 40, "RIGHT")],
        )
        for posted in payload["comments"]:
            self.assertTrue(posted["body"].endswith(post_review.THREAD_MARKER))

    def test_summary_names_the_commit_reviewed(self):
        github = FakeGitHub()
        run("post", github, REVIEW)
        self.assertIn(f"Commit reviewed: {HEAD}", github.writes[0][1]["body"])

    def test_refuses_when_the_pull_request_moved_past_the_reviewed_commit(self):
        github = FakeGitHub(head="b" * 40)
        with self.assertRaises(post_review.ReviewError):
            run("post", github, REVIEW)
        self.assertEqual(github.writes, [])

    def test_refuses_a_finding_without_a_line(self):
        github = FakeGitHub()
        review = dict(REVIEW, findings=[{"path": "a.ts", "body": "no line"}])
        with self.assertRaises(post_review.ReviewError):
            run("post", github, review)
        self.assertEqual(github.writes, [])


class RecheckTests(unittest.TestCase):
    def test_replies_and_resolves_its_own_thread_and_opens_none(self):
        github = FakeGitHub(threads=[reviewer_thread("T1")])
        run("recheck", github, RECHECK)
        kinds = [write[0] for write in github.writes]
        self.assertEqual(kinds, ["reply_to_thread", "resolve_thread", "comment"])
        self.assertNotIn("create_review", kinds)
        self.assertIn(post_review.RECHECK_MARKER, github.writes[-1][1])
        self.assertIn(f"Commit reviewed: {HEAD}", github.writes[-1][1])

    def test_refuses_new_findings(self):
        github = FakeGitHub(threads=[reviewer_thread("T1")])
        with self.assertRaises(post_review.ReviewError):
            run("recheck", github, dict(RECHECK, findings=REVIEW["findings"]))
        self.assertEqual(github.writes, [])

    def test_refuses_a_thread_it_did_not_open(self):
        github = FakeGitHub(threads=[reviewer_thread("T1"), owner_thread("T2")])
        recheck = dict(RECHECK, replies=[{"thread_id": "T2", "body": "x", "resolve": True}])
        with self.assertRaises(post_review.ReviewError):
            run("recheck", github, recheck)
        self.assertEqual(github.writes, [])

    def test_refuses_a_thread_whose_marker_another_account_copied(self):
        github = FakeGitHub(threads=[reviewer_thread("T1"), reviewer_thread("T2", author="someone")])
        recheck = dict(RECHECK, replies=[{"thread_id": "T2", "body": "x", "resolve": True}])
        with self.assertRaises(post_review.ReviewError):
            run("recheck", github, recheck)
        self.assertEqual(github.writes, [])

    def test_refuses_when_a_push_lands_while_it_reads_the_threads(self):
        github = FakeGitHub(threads=[reviewer_thread("T1")])
        github.head_after_threads = "b" * 40
        with self.assertRaises(post_review.ReviewError):
            run("recheck", github, RECHECK)
        self.assertEqual(github.writes, [])

    def test_refuses_a_second_recheck(self):
        github = FakeGitHub(
            threads=[reviewer_thread("T1")],
            comment_bodies=["Earlier recheck\n\n" + post_review.RECHECK_MARKER],
        )
        with self.assertRaises(post_review.ReviewError):
            run("recheck", github, RECHECK)
        self.assertEqual(github.writes, [])

    def test_dry_run_writes_nothing(self):
        github = FakeGitHub(threads=[reviewer_thread("T1")])
        output = run("recheck", github, RECHECK, dry_run=True)
        self.assertEqual(github.writes, [])
        self.assertIn("src/lib.rs:3", output)


FIXTURES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures")


def fixture(name):
    with open(os.path.join(FIXTURES, f"{name}.json")) as file:
        return json.load(file)


def minutes_after_push(activity, minutes):
    pushed_at = activity.get("head_pushed_at") or activity["head_committed_at"]
    pushed = datetime.fromisoformat(pushed_at.replace("Z", "+00:00"))
    return pushed + timedelta(minutes=minutes)


class CodeRabbitTests(unittest.TestCase):
    """Hand-written fixtures: limit-reached has the "Review limit reached" notice on its head
    commit, reviewed has a CodeRabbit review of its head commit, and in-progress has a review
    still going."""

    def decide(self, activity, minutes):
        return post_review.coderabbit_decision(activity, minutes_after_push(activity, minutes))[0]

    def test_a_limit_notice_on_the_head_commit_calls_for_the_full_review(self):
        self.assertEqual(self.decide(fixture("limit-reached"), 1), "full-review")

    def test_a_coderabbit_review_of_the_head_commit_leaves_only_the_gates(self):
        self.assertEqual(self.decide(fixture("reviewed"), 1), "gates-only")

    def test_a_review_still_in_progress_on_the_head_commit_means_wait(self):
        self.assertEqual(self.decide(fixture("in-progress"), 30), "wait")

    def test_a_limit_notice_on_an_older_commit_does_not_count(self):
        activity = dict(fixture("limit-reached"), head="c" * 40)
        self.assertEqual(self.decide(activity, 1), "wait")

    def test_silence_for_15_minutes_after_the_push_calls_for_the_full_review(self):
        activity = dict(fixture("reviewed"), comments=[], reviews=[])
        self.assertEqual(self.decide(activity, 15), "full-review")

    def test_silence_counts_from_the_push_not_the_commit(self):
        # Committed at 21:09 and pushed at 22:29.
        activity = dict(fixture("reviewed"), comments=[], reviews=[],
                        head_committed_at="2026-11-15T21:09:59Z", head_pushed_at="2026-11-15T22:29:38Z")
        self.assertEqual(self.decide(activity, 14), "wait")
        self.assertEqual(self.decide(activity, 15), "full-review")

    def test_silence_for_less_than_15_minutes_means_wait(self):
        activity = dict(fixture("reviewed"), comments=[], reviews=[])
        self.assertEqual(self.decide(activity, 14), "wait")

    def test_the_command_prints_the_decision_and_why(self):
        github = FakeGitHub()
        github.activity = fixture("reviewed")
        output = io.StringIO()
        with redirect_stdout(output):
            post_review.run("coderabbit", REPO, PR, None, github, False)
        # The full sha binds the gates and the post to the commit the decision was about.
        decision, commit = output.getvalue().split()[:2]
        self.assertEqual((decision, commit), ("gates-only", github.activity["head"]))


class PushTimeTests(unittest.TestCase):
    def test_an_old_commit_just_force_pushed_waits_for_coderabbit(self):
        self.check_old_commit_just_pushed_waits("force_push")

    def test_an_old_commit_on_a_new_branch_waits_for_coderabbit(self):
        # GitHub lists the first push of a branch as branch_creation, not push.
        self.check_old_commit_just_pushed_waits("branch_creation")

    def test_a_branch_name_with_a_url_delimiter_reaches_the_query_whole(self):
        # In a URL, "#" would start a fragment and cut the branch name short.
        self.check_old_commit_just_pushed_waits("push", branch="fix/#253&x=1")

    def check_old_commit_just_pushed_waits(self, activity_type, branch="feature"):
        force_push = {"activity_type": activity_type, "after": HEAD, "timestamp": "2026-11-15T22:00:00Z"}

        def gh(args, stdin=None):
            if args[:2] == ["pr", "view"]:
                return json.dumps({"headRefOid": HEAD, "headRefName": branch})
            if args[1].startswith(f"repos/{REPO}/commits/"):
                return json.dumps({"commit": {"committer": {"date": "2026-11-01T09:00:00Z"}}})
            if f"repos/{REPO}/activity" in args:
                # gh encodes each -f value, so the whole branch name reaches GitHub.
                fields = [args[i + 1] for i, arg in enumerate(args) if arg == "-f"]
                named_type = [field for field in fields if field.startswith("activity_type=")]
                listed = (f"ref=refs/heads/{branch}" in fields
                          and named_type in ([], [f"activity_type={activity_type}"]))
                return json.dumps([force_push] if listed else [])
            return json.dumps([[]])

        github = post_review.GitHub()
        github._gh = gh
        activity = github.coderabbit_activity(REPO, PR)
        self.assertEqual(activity["head_pushed_at"], "2026-11-15T22:00:00Z")
        five_minutes_later = datetime.fromisoformat("2026-11-15T22:05:00+00:00")
        self.assertEqual(post_review.coderabbit_decision(activity, five_minutes_later)[0], "wait")


class ThreadPagesTests(unittest.TestCase):
    def test_reads_every_page_of_review_threads(self):
        def page(thread_id):
            node = {"id": thread_id, "isResolved": False, "path": "a.rs", "line": 1,
                    "comments": {"nodes": [{"body": "x", "author": {"login": "owner"}}]}}
            return {"data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": [node]}}}}}

        calls = []

        def gh(args, stdin=None):
            calls.append(args)
            return json.dumps([page("T1"), page("T2")])

        github = post_review.GitHub()
        github._gh = gh
        self.assertEqual([t["id"] for t in github.review_threads(REPO, PR)], ["T1", "T2"])
        self.assertIn("--paginate", calls[0])
        self.assertIn("--slurp", calls[0])
        query = next(arg for arg in calls[0] if arg.startswith("query="))
        self.assertIn("after: $endCursor", query)
        self.assertIn("pageInfo { hasNextPage endCursor }", query)


class ThreadsTests(unittest.TestCase):
    def test_lists_only_the_reviewers_threads(self):
        github = FakeGitHub(threads=[reviewer_thread("T1"), owner_thread("T2")])
        output = io.StringIO()
        with redirect_stdout(output):
            post_review.run("threads", REPO, PR, None, github, False)
        listed = json.loads(output.getvalue())
        self.assertEqual([thread["id"] for thread in listed], ["T1"])


if __name__ == "__main__":
    unittest.main()
