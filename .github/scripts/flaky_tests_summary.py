"""Prints the flaky tests in a nextest JUnit report as Markdown for the CI job summary.

A flaky test failed at least once and then passed on a retry. nextest marks each failed try
with a `flakyFailure` or `flakyError` element inside the test's `testcase`.

Usage: flaky_tests_summary.py <path to junit.xml>
"""

import sys
import xml.etree.ElementTree as ElementTree
from pathlib import Path

FAILED_TRY_TAGS = ("flakyFailure", "flakyError")
# The `type` nextest gives a try it stopped at the slow-timeout limit.
TIMEOUT_TYPE = "test timeout"


def flaky_tests(junit_report: Path) -> list[tuple[str, str, int, int]]:
    """Returns (test binary, test name, failed tries, tries that timed out) for each flaky test."""
    flaky = []
    for test_case in ElementTree.parse(junit_report).iter("testcase"):
        failed_tries = [failed_try for tag in FAILED_TRY_TAGS for failed_try in test_case.findall(tag)]
        if failed_tries:
            timed_out_tries = sum(failed_try.get("type") == TIMEOUT_TYPE for failed_try in failed_tries)
            flaky.append(
                (test_case.get("classname"), test_case.get("name"), len(failed_tries), timed_out_tries)
            )
    return flaky


def summary(junit_report: Path) -> str:
    """Returns the Markdown under the heading: the flaky tests, or why there are none."""
    if not junit_report.exists():
        return "No test report, so the tests did not run."
    flaky = flaky_tests(junit_report)
    if not flaky:
        return "None."
    lines = [
        f"{len(flaky)} failed and then passed on a retry. "
        "Open an issue for each one with the cause. See `docs/workflow.md`.",
        "",
        "| Test binary | Test | Failed tries | Of those, timed out |",
        "|---|---|---|---|",
    ]
    lines += [
        f"| `{binary}` | `{name}` | {failed_tries} | {timed_out_tries} |"
        for binary, name, failed_tries, timed_out_tries in flaky
    ]
    return "\n".join(lines)


if __name__ == "__main__":
    print("### Flaky Rust tests\n")
    print(summary(Path(sys.argv[1])))
