#!/usr/bin/env python3
"""Posts the reviewer's findings on a pull request with gh, or prints them on a dry run.

  post_review.py coderabbit <owner/repo> <pr>
  post_review.py threads <owner/repo> <pr>
  post_review.py post    <owner/repo> <pr> <review.json>  [--dry-run]
  post_review.py recheck <owner/repo> <pr> <recheck.json> [--dry-run]

review.json:  {"commit": sha, "summary": md, "findings": [{"path", "line", "body"}]}
recheck.json: {"commit": sha, "summary": md, "replies": [{"thread_id", "body", "resolve"}]}

Every post carries a hidden marker, so the reviewer finds its own threads later. The reviewer
posts from the owner's login, and the marker is the only thing that tells its threads apart.
"""

import json
import re
import subprocess
import sys
from datetime import datetime, timedelta, timezone

THREAD_MARKER = "<!-- grove-reviewer -->"
RECHECK_MARKER = "<!-- grove-reviewer recheck -->"

CODERABBIT = "coderabbitai[bot]"
CODERABBIT_SILENCE = timedelta(minutes=15)
# CodeRabbit keeps one summary comment and edits it. A run it skipped for its rate limit, or one
# still going, sits in a marked block that links to the commit. Outside those blocks, a link to the
# commit means CodeRabbit reviewed it.
def coderabbit_block(kind):
    return re.compile(
        rf"<!-- This is an auto-generated comment: {kind} by coderabbit\.ai -->.*?"
        rf"<!-- end of auto-generated comment: {kind} by coderabbit\.ai -->",
        re.DOTALL,
    )


RATE_LIMITED_BLOCK = coderabbit_block("rate limited")
IN_PROGRESS_BLOCK = coderabbit_block("review in progress")

REVIEW_THREADS_QUERY = """
query($owner: String!, $name: String!, $number: Int!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $endCursor) {
        nodes { id isResolved path line comments(first: 1) { nodes { body author { login } } } }
        pageInfo { hasNextPage endCursor }
      }
    }
  }
}
"""


class ReviewError(Exception):
    pass


class GitHub:
    """The gh calls. post() and recheck() make no write call on a dry run."""

    def _gh(self, args, stdin=None):
        result = subprocess.run(["gh", *args], input=stdin, capture_output=True, text=True)
        if result.returncode != 0:
            raise ReviewError(f"gh {' '.join(args[:3])} failed: {result.stderr.strip()}")
        return result.stdout

    def _write(self, description, args, stdin=None):
        self._gh(args, stdin)
        print(f"posted: {description}")

    def coderabbit_activity(self, repo, pr):
        pull_request = json.loads(self._gh(["pr", "view", str(pr), "--repo", repo, "--json", "headRefOid,headRefName"]))
        head = pull_request["headRefOid"]
        commit = json.loads(self._gh(["api", f"repos/{repo}/commits/{head}"]))
        # -f lets gh encode the branch name, which may hold "#" or "&".
        pushes = json.loads(self._gh([
            "api", "--method", "GET", f"repos/{repo}/activity",
            "-f", f"ref=refs/heads/{pull_request['headRefName']}", "-f", "per_page=100",
        ]))
        comments = json.loads(self._gh(["api", "--paginate", "--slurp", f"repos/{repo}/issues/{pr}/comments"]))
        reviews = json.loads(self._gh(["api", "--paginate", "--slurp", f"repos/{repo}/pulls/{pr}/reviews"]))
        return {
            "head": head,
            "head_committed_at": commit["commit"]["committer"]["date"],
            # The newest push that made the head. GitHub lists a plain push, a force push and the
            # first push of a branch as three types.
            "head_pushed_at": next(
                (push["timestamp"] for push in pushes
                 if push["activity_type"] in ("push", "force_push", "branch_creation") and push["after"] == head),
                None,
            ),
            "comments": [{"user": c["user"]["login"], "body": c["body"] or ""} for page in comments for c in page],
            "reviews": [{"user": r["user"]["login"], "commit_id": r["commit_id"]} for page in reviews for r in page],
        }

    def pull_request_head(self, repo, pr):
        return self._gh(["pr", "view", str(pr), "--repo", repo, "--json", "headRefOid", "-q", ".headRefOid"]).strip()

    def pull_request_comment_bodies(self, repo, pr):
        raw = self._gh(["pr", "view", str(pr), "--repo", repo, "--json", "comments"])
        return [comment["body"] for comment in json.loads(raw)["comments"]]

    def review_threads(self, repo, pr):
        owner, name = repo.split("/")
        raw = self._gh([
            "api", "graphql", "--paginate", "--slurp", "-f", f"query={REVIEW_THREADS_QUERY}",
            "-F", f"owner={owner}", "-F", f"name={name}", "-F", f"number={pr}",
        ])
        threads = []
        for page in json.loads(raw):
            for node in page["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]:
                first = (node["comments"]["nodes"] or [{"body": "", "author": None}])[0]
                threads.append({
                    "id": node["id"],
                    "path": node["path"],
                    "line": node["line"],
                    "isResolved": node["isResolved"],
                    "author": (first["author"] or {}).get("login"),
                    "first_comment": first["body"],
                })
        return threads

    def viewer_login(self):
        return self._gh(["api", "user", "-q", ".login"]).strip()

    def create_review(self, repo, pr, payload):
        self._write(
            f"review with {len(payload['comments'])} thread(s)",
            ["api", "--method", "POST", f"repos/{repo}/pulls/{pr}/reviews", "--input", "-"],
            json.dumps(payload),
        )

    def reply_to_thread(self, thread_id, body):
        mutation = "mutation($id: ID!, $body: String!) { addPullRequestReviewThreadReply(input: {pullRequestReviewThreadId: $id, body: $body}) { comment { id } } }"
        self._write(f"reply on {thread_id}", ["api", "graphql", "-f", f"query={mutation}", "-f", f"id={thread_id}", "-f", f"body={body}"])

    def resolve_thread(self, thread_id):
        mutation = "mutation($id: ID!) { resolveReviewThread(input: {threadId: $id}) { thread { id } } }"
        self._write(f"resolved {thread_id}", ["api", "graphql", "-f", f"query={mutation}", "-f", f"id={thread_id}"])

    def comment(self, repo, pr, body):
        self._write("summary comment", ["pr", "comment", str(pr), "--repo", repo, "--body-file", "-"], body)


def coderabbit_decision(activity, now):
    """gates-only when CodeRabbit reviewed the head commit. wait while it is reviewing it.
    full-review when its notice for the head commit is "Review limit reached", or when it said
    nothing about the head commit 15 minutes after it was pushed. wait otherwise. The commit date
    stands in for the push when GitHub lists no push of the head, as for a fork."""
    head = activity["head"]
    commit_link = f"/commit/{head}"
    limited = in_progress = False
    for review in activity["reviews"]:
        if review["user"] == CODERABBIT and review["commit_id"] == head:
            return "gates-only", f"CodeRabbit reviewed {head[:7]}"
    for comment in activity["comments"]:
        if comment["user"] != CODERABBIT:
            continue
        body = comment["body"]
        limited |= any(commit_link in block for block in RATE_LIMITED_BLOCK.findall(body))
        in_progress |= any(commit_link in block for block in IN_PROGRESS_BLOCK.findall(body))
        if commit_link in IN_PROGRESS_BLOCK.sub("", RATE_LIMITED_BLOCK.sub("", body)):
            return "gates-only", f"CodeRabbit reviewed {head[:7]}"
    if in_progress:
        return "wait", f"CodeRabbit is still reviewing {head[:7]}. Ask again when it is done"
    if limited:
        return "full-review", f"CodeRabbit's notice for {head[:7]} is \"Review limit reached\""
    pushed_at = activity.get("head_pushed_at") or activity["head_committed_at"]
    pushed = datetime.fromisoformat(pushed_at.replace("Z", "+00:00"))
    if now - pushed >= CODERABBIT_SILENCE:
        return "full-review", f"CodeRabbit said nothing about {head[:7]} in 15 minutes"
    wait = CODERABBIT_SILENCE - (now - pushed)
    return "wait", f"CodeRabbit may still review {head[:7]}. Ask again in {int(wait.total_seconds() // 60) + 1} minutes"


def own_threads(github, repo, pr):
    """Threads the reviewer opened: the marker, posted from the login gh uses here."""
    login = github.viewer_login()
    return [t for t in github.review_threads(repo, pr) if t["author"] == login and THREAD_MARKER in t["first_comment"]]


def require_head(github, repo, pr, commit):
    head = github.pull_request_head(repo, pr)
    if head != commit:
        raise ReviewError(f"PR #{pr} head is {head}, not the reviewed commit {commit}. Review the new head.")


def with_commit(summary, commit, marker):
    return f"{summary.rstrip()}\n\nCommit reviewed: {commit}\n\n{marker}"


def post(github, repo, pr, review, dry_run):
    commit = review["commit"]
    for finding in review["findings"]:
        if not all(finding.get(key) for key in ("path", "line", "body")):
            raise ReviewError(f"Each finding needs path, line and body: {finding}")
    require_head(github, repo, pr, commit)
    payload = {
        "commit_id": commit,
        "event": "COMMENT",
        "body": with_commit(review["summary"], commit, THREAD_MARKER),
        "comments": [
            {"path": f["path"], "line": int(f["line"]), "side": "RIGHT", "body": f"{f['body'].rstrip()}\n\n{THREAD_MARKER}"}
            for f in review["findings"]
        ],
    }
    if dry_run:
        print(f"DRY RUN: nothing posted. Would post on {repo} PR #{pr}:\n")
        print(f"--- Summary ---\n{payload['body']}\n")
        for number, comment in enumerate(payload["comments"], 1):
            print(f"--- Thread {number}: {comment['path']}:{comment['line']} ---\n{comment['body']}\n")
        return
    github.create_review(repo, pr, payload)


def recheck(github, repo, pr, data, dry_run):
    if "findings" in data:
        raise ReviewError("A recheck opens no new thread. List anything new in the report for the owner.")
    if any(RECHECK_MARKER in body for body in github.pull_request_comment_bodies(repo, pr)):
        raise ReviewError(f"PR #{pr} already has a recheck. The reviewer rechecks once, then stops.")
    commit = data["commit"]
    threads = {t["id"]: t for t in own_threads(github, repo, pr)}
    for reply in data["replies"]:
        if reply["thread_id"] not in threads:
            raise ReviewError(f"Thread {reply['thread_id']} was not opened by the reviewer. Leave it alone.")
    # Last read before any write, so a push meanwhile stops the recheck.
    require_head(github, repo, pr, commit)
    summary = with_commit(data["summary"], commit, RECHECK_MARKER)
    if dry_run:
        print(f"DRY RUN: nothing posted. Would post on {repo} PR #{pr}:\n")
        for reply in data["replies"]:
            thread = threads[reply["thread_id"]]
            action = "reply and resolve" if reply.get("resolve") else "reply"
            print(f"--- {action} on {thread['path']}:{thread['line']} ---\n{reply['body']}\n")
        print(f"--- Summary ---\n{summary}\n")
        return
    for reply in data["replies"]:
        github.reply_to_thread(reply["thread_id"], reply["body"])
        if reply.get("resolve"):
            github.resolve_thread(reply["thread_id"])
    github.comment(repo, pr, summary)


def run(command, repo, pr, path, github, dry_run):
    if command == "coderabbit":
        activity = github.coderabbit_activity(repo, pr)
        decision, reason = coderabbit_decision(activity, datetime.now(timezone.utc))
        print(f"{decision} {activity['head']}\n{reason}")
        return
    if command == "threads":
        print(json.dumps(own_threads(github, repo, pr), indent=2))
        return
    with open(path) as file:
        data = json.load(file)
    if command == "post":
        post(github, repo, pr, data, dry_run)
    elif command == "recheck":
        recheck(github, repo, pr, data, dry_run)
    else:
        raise ReviewError(f"Unknown command {command}")


def main(argv):
    dry_run = "--dry-run" in argv
    args = [arg for arg in argv if arg != "--dry-run"]
    if len(args) < 3 or (args[0] in ("post", "recheck") and len(args) < 4):
        print(__doc__, file=sys.stderr)
        return 2
    command, repo, pr = args[0], args[1], int(args[2])
    try:
        run(command, repo, pr, args[3] if len(args) > 3 else None, GitHub(), dry_run)
    except ReviewError as error:
        print(f"post_review: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
