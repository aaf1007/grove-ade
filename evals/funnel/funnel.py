#!/usr/bin/env python3
"""Live funnel: which stage caught each real defect in merged pull requests.

Reads merged pull requests and `bug` or `dogfood` issues with gh, read only, and writes a weekly
table to evals/results/funnel-<until>.md.

Usage, from the repo root:
  python3 evals/funnel/funnel.py [--since YYYY-MM-DD] [--until YYYY-MM-DD]

Where each stage's defects come from:
- coderabbit, cursor: their review threads that the reply marks as fixed or real.
- reviewer: threads in a review the reviewer agent posted (its summary names the commit reviewed).
- owner: other threads the owner opened.
- gates: each FAIL row in a reviewer summary.
- interrogate, thermos, qa, verify, gauntlet: `bug` rows in the "Caught by" column of the pull request template.
- escaped: a bug or dogfood issue that says which issue or pull request caused it.
"""
import argparse
import datetime
import json
import re
import subprocess
import sys
from collections import Counter
from pathlib import Path

GITHUB_REPO = "aaf1007/grove-ade"
OWNER = "aaf1007"
RESULTS_DIR = Path(__file__).resolve().parent.parent / "results"
STAGES = ("gates", "coderabbit", "cursor", "reviewer", "interrogate", "thermos", "qa", "verify", "gauntlet",
          "owner", "escaped")
# Stages that run before the pull request. Only the template's "Caught by" column can show them.
BEFORE_PULL_REQUEST = ("interrogate", "thermos", "qa", "verify", "gauntlet")
REACHED_OWNER = ("owner", "escaped")
BOTS = {"coderabbitai": "coderabbit", "cursor": "cursor"}
REVIEWER_MARKERS = ("Commit reviewed:", "## Review of issue", "Recheck of issue")
FIXED_WORDS = {"fixed", "moved", "added", "removed", "real", "valid", "replaced", "changed", "capped", "bounded",
               "addressed", "deferred", "agreed", "yes", "raised", "correction", "done", "renamed", "dropped",
               "updated", "update", "good"}
DISMISSED_WORDS = {"not", "skipped", "skipping", "no", "leaving", "kept", "known", "already", "checked", "declined",
                   "won't", "wontfix", "intentional"}
ESCAPE_CAUSE = re.compile(r"\b(?:caused|introduced|broken|regressed)\s+(?:by|in)\s+(?:(?:PR|pull request|issue)\s*)?"
                          r"(?:#(\d+)|https://github\.com/([^/\s]+/[^/\s]+)/pull/(\d+))", re.I)

PULL_REQUESTS_QUERY = """query($q: String!, $cursor: String) {
  search(query: $q, type: ISSUE, first: 20, after: $cursor) {
    pageInfo { hasNextPage endCursor }
    nodes { ... on PullRequest {
      number title url body createdAt mergedAt
      commits(first: 100) { pageInfo { hasNextPage } nodes { commit { committedDate messageHeadline } } }
      reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { isResolved isOutdated
        comments(first: 50) { pageInfo { hasNextPage } nodes { author { login } body url pullRequestReview { body } } } } }
      reviews(first: 100) { pageInfo { hasNextPage } nodes { author { login } body state submittedAt } }
      comments(first: 100) { pageInfo { hasNextPage } nodes { author { login } body } }
    } }
  }
}"""


def run_gh(args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def fetch(repo, since, until, gh=run_gh):
    """Merged pull requests in [since, until] and every bug or dogfood issue. Only reads."""
    prs, cursor = [], None
    while True:
        args = ["api", "graphql", "-f", f"query={PULL_REQUESTS_QUERY}",
                "-f", f"q=repo:{repo} is:pr is:merged merged:{since}..{until}"]
        if cursor:
            args += ["-f", f"cursor={cursor}"]
        page = json.loads(gh(args))["data"]["search"]
        prs += [node for node in page["nodes"] if node]
        if not page["pageInfo"]["hasNextPage"]:
            break
        cursor = page["pageInfo"]["endCursor"]
    issues = {}
    for label in ("bug", "dogfood"):
        listed = gh(["issue", "list", "--repo", repo, "--state", "all", "--label", label, "--limit", "500",
                     "--json", "number,title,body,url,labels"])
        issues.update({issue["number"]: issue for issue in json.loads(listed)})
    return prs, list(issues.values())


def login(comment):
    return (comment.get("author") or {}).get("login")


def is_reviewer_text(body):
    return any(marker in (body or "") for marker in REVIEWER_MARKERS)


def thread_stage(first):
    who = login(first)
    if who in BOTS:
        return BOTS[who]
    if who == OWNER:
        review = first.get("pullRequestReview") or {}
        if is_reviewer_text(review.get("body")):
            return "reviewer"
        # Implementers post from the owner's login too. A thread that opens by saying what changed is theirs.
        return None if first_word(first["body"]) in FIXED_WORDS else "owner"
    return None


def first_word(text):
    words = re.findall(r"[a-z']+", text.lower())
    return words[0] if words else ""


def thread_disposition(thread):
    """defect, dismissed or unclear, from the first reply, else from whether the code moved under it."""
    replies = thread["comments"]["nodes"][1:]
    if replies:
        word = first_word(replies[0]["body"])
        if word in DISMISSED_WORDS:
            return "dismissed"
        if word in FIXED_WORDS:
            return "defect"
    if thread["isResolved"] and thread["isOutdated"]:
        return "defect"
    return "unclear"


def failing_gates(body):
    gates = set()
    for line in (body or "").splitlines():
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if line.lstrip().startswith("|") and len(cells) > 1 and any(cell.startswith("FAIL") for cell in cells[1:]):
            gates.add(cells[0])
    return gates


def template_stages(body):
    """Stages before the pull request named in the Caught by column of the template's findings table.

    Later stages leave threads and reviews, which are counted from those, so a row naming one is skipped.
    Only rows whose Category is bug are defects."""
    stages, column, category = [], None, None
    for line in (body or "").splitlines():
        if not line.lstrip().startswith("|"):
            column = None
            continue
        cells = [cell.strip().lower() for cell in line.strip().strip("|").split("|")]
        if "caught by" in cells and "category" in cells:
            column, category = cells.index("caught by"), cells.index("category")
        elif (column is not None and max(column, category) < len(cells) and cells[category] == "bug"
              and cells[column] in BEFORE_PULL_REQUEST):
            stages.append(cells[column])
    return stages


def is_review_round(review):
    """A review that asks for changes: a bot's, the reviewer agent's, or the owner's own."""
    who, body = login(review), review.get("body") or ""
    if who == "coderabbitai":
        return re.search(r"Actionable comments posted: [1-9]", body) is not None
    if who in BOTS:
        return bool(body) and "couldn't run" not in body
    return who == OWNER and (is_reviewer_text(review.get("body")) or review.get("state") == "CHANGES_REQUESTED")


def fix_rounds(pr):
    """Reviews the author answered with at least one commit before the next review."""
    reviews = sorted(parse_time(r["submittedAt"]) for r in pr["reviews"]["nodes"]
                     if r.get("submittedAt") and is_review_round(r))
    commits = [parse_time(node["commit"]["committedDate"]) for node in pr["commits"]["nodes"]
               if not node["commit"]["messageHeadline"].startswith("Merge ")]
    windows = zip(reviews, reviews[1:] + [datetime.datetime.max.replace(tzinfo=datetime.timezone.utc)])
    return sum(1 for start, end in windows if any(start < commit < end for commit in commits))


def cost(pr):
    match = re.search(r"^\W*Cost[^:\n]*:\s*\$?([0-9]+(?:\.[0-9]+)?)", pr.get("body") or "", re.M | re.I)
    return float(match.group(1)) if match else None


def parse_time(text):
    return datetime.datetime.fromisoformat(text.replace("Z", "+00:00"))


def repo_of(pr):
    return "/".join(pr["url"].split("/")[3:5]).lower()


def escapes(prs, issues):
    """Pull request number -> escaped issues that name it, or the issue it closed, as their cause.

    A cause given as a link counts only when it points at this repository."""
    repos = {repo_of(pr) for pr in prs}
    by_number = {pr["number"]: pr["number"] for pr in prs}
    for pr in prs:
        issue = re.match(r"#(\d+)\b", pr["title"])
        if issue:
            by_number.setdefault(int(issue.group(1)), pr["number"])
    found = {}
    for issue in issues:
        text = f"{issue['title']}\n{issue.get('body') or ''}"
        for match in ESCAPE_CAUSE.finditer(text):
            number, link_repo, link_number = match.groups()
            if link_repo and link_repo.lower() not in repos:
                continue
            pr = by_number.get(int(number or link_number))
            if pr is not None:
                found.setdefault(pr, []).append(issue["url"])
                break
    return found


def truncated(pr):
    """GitHub returned only the first page of a list, so the counts would be low."""
    lists = [pr[key] for key in ("commits", "reviewThreads", "reviews", "comments")]
    lists += [thread["comments"] for thread in pr["reviewThreads"]["nodes"]]
    return any((items.get("pageInfo") or {}).get("hasNextPage") for items in lists)


def analyze(prs, issues):
    escaped = escapes(prs, issues)
    records = []
    for pr in prs:
        defects, unclear = Counter(), []
        for thread in pr["reviewThreads"]["nodes"]:
            comments = thread["comments"]["nodes"]
            stage = thread_stage(comments[0]) if comments else None
            if stage is None:
                continue
            disposition = thread_disposition(thread)
            if disposition == "defect":
                defects[stage] += 1
            elif disposition == "unclear":
                unclear.append(comments[0].get("url"))
        gates = set()
        for item in pr["reviews"]["nodes"] + pr["comments"]["nodes"]:
            if login(item) == OWNER and is_reviewer_text(item["body"]):
                gates |= failing_gates(item["body"])
        if gates:
            defects["gates"] += len(gates)
        defects.update(template_stages(pr.get("body")))
        if escaped.get(pr["number"]):
            defects["escaped"] += len(escaped[pr["number"]])
        merged = parse_time(pr["mergedAt"]).date()
        records.append({"number": pr["number"], "title": pr["title"], "url": pr["url"], "merged": merged.isoformat(),
                        "week": (merged - datetime.timedelta(days=merged.weekday())).isoformat(),
                        "defects": dict(defects), "unclear": unclear, "fix_rounds": fix_rounds(pr),
                        "cost": cost(pr), "escaped_issues": escaped.get(pr["number"], []),
                        "incomplete": truncated(pr)})
    return records


def row(label, records):
    totals = Counter()
    for record in records:
        totals.update(record["defects"])
    costs = [r["cost"] for r in records if r["cost"] is not None]
    cost_cell = f"{sum(costs):.2f} ({len(costs)} PR{'s' if len(costs) != 1 else ''})" if costs else "unknown"
    rounds = sum(r["fix_rounds"] for r in records) / len(records)
    return (f"| {label} | {len(records)} | " + " | ".join(str(totals[s]) for s in STAGES) +
            f" | {sum(totals[s] for s in REACHED_OWNER)} | {rounds:.1f} | {cost_cell} |")


def render(records, since, until):
    left_out = [r for r in records if r["incomplete"]]
    records = [r for r in records if not r["incomplete"]]
    header = f"| Week | PRs | {' | '.join(STAGES)} | Reached owner | Fix rounds | Cost (USD) |"
    divider = "| " + " | ".join("---" for _ in range(len(STAGES) + 5)) + " |"
    weeks = sorted({r["week"] for r in records})
    lines = [f"# Funnel, merged {since} to {until}", "",
             "Real defects in merged pull requests, by the stage that caught them. Reached owner is owner plus "
             "escaped: the number the workflow should push down. Fix rounds is the mean count of reviews (a bot's, "
             "the reviewer agent's, or the owner's) that the author answered with a commit. Weeks start on Monday.", "",
             header, divider]
    lines += [row(week, [r for r in records if r["week"] == week]) for week in weeks]
    if records:
        lines.append(row("All", records))
    lines += ["", "## Per pull request", "", "| PR | Merged | Defects | Fix rounds | Unclear threads |",
              "| --- | --- | --- | --- | --- |"]
    for r in sorted(records, key=lambda r: r["number"]):
        defects = ", ".join(f"{stage} {n}" for stage, n in sorted(r["defects"].items())) or "none"
        lines.append(f"| [#{r['number']}]({r['url']}) | {r['merged']} | {defects} | {r['fix_rounds']} | "
                     f"{len(r['unclear'])} |")
    unclear = [url for r in records for url in r["unclear"]]
    lines += ["", "## Unclear threads", "",
              "Threads whose reply neither says fixed nor declines, and whose code did not move. They are not "
              "counted. Read them by hand when the numbers matter.", ""]
    lines += [f"- {url}" for url in unclear] or ["None."]
    if left_out:
        lines += ["", "## Not counted", "",
                  "GitHub gave more than one page of threads, reviews, commits or comments for these, so their "
                  "counts would be low.", ""]
        lines += [f"- [#{r['number']}]({r['url']})" for r in left_out]
    return "\n".join(lines) + "\n"


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    today = datetime.date.today()
    parser.add_argument("--since", default=(today - datetime.timedelta(days=31)).isoformat())
    parser.add_argument("--until", default=today.isoformat())
    parser.add_argument("--json", action="store_true", help="also write the per pull request records")
    args = parser.parse_args(argv)
    prs, issues = fetch(GITHUB_REPO, args.since, args.until)
    records = analyze(prs, issues)
    RESULTS_DIR.mkdir(parents=True, exist_ok=True)
    out = RESULTS_DIR / f"funnel-{args.until}.md"
    out.write_text(render(records, args.since, args.until))
    if args.json:
        out.with_suffix(".json").write_text(json.dumps(records, indent=1) + "\n")
    print(out)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
