#!/usr/bin/env python3
"""Bug replay: put past bugs back and see which review stage finds them.

A case is a fix commit from grove's history. The stage under test reviews the work of the fix's
branch with the fix left out, in a fresh repo that holds only two commits. The fix's test is the
answer key: a judge model compares each finding with the fix, and the stage never sees either.

Usage, from the repo root:
  python3 evals/replay/replay.py proof [--cases SHA,...]
  python3 evals/replay/replay.py run [--stage NAME[=SHA,...]]... [--cases SHA,...] [--jobs N]
"""
import argparse
import concurrent.futures
import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import asdict, dataclass, field
from pathlib import Path

# A git hook sets these, and they would point every git command here at the hook's repo instead of the one named.
for name in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES",
             "GIT_COMMON_DIR"):
    os.environ.pop(name, None)

HERE = Path(__file__).resolve().parent
CASES_FILE = HERE / "cases.json"
RESULTS_DIR = HERE.parent / "results"
GITHUB_REPO = "aaf1007/grove-ade"
CACHE = Path(os.environ.get("GROVE_EVALS_CACHE", Path.home() / ".cache" / "grove-evals"))

# Model stages run as a slash command in a headless Claude Code session.
MODEL_STAGES = {"code-review": "/code-review:code-review", "interrogate": "/pstack:interrogate",
                "thermos": "/pstack:thermos"}
UNAVAILABLE_STAGES = {"verify-gauntlet": "needs issue #265"}
STAGES = ("gates", *MODEL_STAGES, *UNAVAILABLE_STAGES)
SCORED = ("caught", "missed", "false alarm")
JUDGE_MODEL = "sonnet"
COMMAND_ATTEMPTS = 3
# Trees that hold the fix or its test live only in folders named this way, and only while they are used.
ANSWER_KEY_PREFIX = "grove-replay-key-"
COMMAND_LOCK = threading.Lock()
COMMAND_TIMEOUT_SECONDS = 45 * 60
PLUGIN_LIST_TIMEOUT_SECONDS = 60

ANSWER_KEY = re.compile(r"(\.test\.tsx?$|^crates/[^/]+/tests/|(^|/)test_[^/]*\.py$|^app/src/fakeBridge\.ts$)")
# `git diff` with two paths prints any file on disk, so the stage may only diff its own branch.
STAGE_READ_TOOLS = ["Read", "Grep", "Glob", "Agent", "Task", "Skill", "TodoWrite", "Bash(git diff main...HEAD:*)",
                    "Bash(git log:*)", "Bash(git show:*)", "Bash(git status:*)", "Bash(ls:*)"]
STAGE_PLUGINS = ("pstack@pstack-claude", "code-review@claude-plugins-official")
STAGE_DENIED_TOOLS = ["Bash(gh:*)", "Edit", "Write", "NotebookEdit", "WebFetch", "WebSearch",
                      "mcp__github_inline_comment__create_inline_comment"]


class CaseError(Exception):
    pass


@dataclass(frozen=True)
class Case:
    fix: str
    issue: int
    subject: str
    base: str
    fix_files: tuple
    answer_key: tuple

    @property
    def area(self):
        return ",".join(sorted({area_of(path) for path in self.fix_files}))


@dataclass
class Proof:
    fails_without_fix: bool
    passes_with_fix: bool

    @property
    def label(self):
        return "pass" if self.fails_without_fix and self.passes_with_fix else "FAIL"


@dataclass
class StageResult:
    outcome: str
    findings: int = 0
    other_findings: int = 0
    tokens: int = 0
    cost_usd: float = 0.0
    judge_cost_usd: float = 0.0
    seconds: float = 0.0
    note: str = ""
    leaks: list = field(default_factory=list)


def git(repo, *args, binary=False):
    out = subprocess.run(["git", "-C", str(repo), *args], check=True, capture_output=True, text=not binary)
    return out.stdout if binary else out.stdout.strip()


def is_ancestor(repo, older, newer):
    return subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", older, newer]).returncode == 0


def area_of(path):
    if path.startswith(("crates/", "app/src-tauri/", "Cargo.")):
        return "rust"
    if path.startswith("app/"):
        return "app"
    if path.startswith("sidecar/claude/"):
        return "sidecar"
    if path.endswith(".py"):
        return "python"
    return "other"


def pull_request_base(repo, fix):
    """The main commit the fix's branch last merged, found from the merge that brought the fix in."""
    for line in git(repo, "log", "--first-parent", "--merges", "--format=%H %P", "main", "--").splitlines():
        merge, first_parent = line.split()[:2]
        if is_ancestor(repo, fix, merge) and not is_ancestor(repo, fix, first_parent):
            return git(repo, "merge-base", fix, first_parent)
    raise CaseError(f"{fix[:7]}: no merge on main brought it in")


def load_case(repo, fix, issue):
    fix = git(repo, "rev-parse", "--verify", f"{fix}^{{commit}}")
    changed = git(repo, "diff", "--name-only", f"{fix}^", fix).splitlines()
    changed = [path for path in changed if not path.endswith(".md")]
    answer_key = tuple(path for path in changed if ANSWER_KEY.search(path))
    fix_files = tuple(path for path in changed if path not in answer_key)
    if not answer_key:
        raise CaseError(f"{fix[:7]}: the fix has no test to use as the answer key")
    if not fix_files:
        raise CaseError(f"{fix[:7]}: the fix changes only tests")
    base = pull_request_base(repo, fix)
    if not git(repo, "diff", "--name-only", base, f"{fix}^"):
        raise CaseError(f"{fix[:7]}: nothing to review, the fix is the first commit on its branch")
    return Case(fix, issue, git(repo, "log", "-1", "--format=%s", fix), base, fix_files, answer_key)


def export_tree(repo, rev, dest):
    dest.mkdir(parents=True, exist_ok=True)
    archive = subprocess.run(["git", "-C", str(repo), "archive", rev], check=True, capture_output=True).stdout
    subprocess.run(["tar", "-x", "-m", "-C", str(dest)], input=archive, check=True)


def overlay(repo, rev, paths, dest):
    """Sets each path in dest to its content at rev, or deletes it when rev does not have it."""
    for path in paths:
        target = dest / path
        shown = subprocess.run(["git", "-C", str(repo), "show", f"{rev}:{path}"], capture_output=True)
        # Writing through an old symlink would change the file it points to, so the old entry goes first.
        if target.is_symlink() or target.exists():
            target.unlink()
        if shown.returncode == 0:
            target.parent.mkdir(parents=True, exist_ok=True)
            mode = git(repo, "ls-tree", rev, "--", path).split()[0]
            if mode == "120000":
                target.symlink_to(os.fsdecode(shown.stdout))
            else:
                target.write_bytes(shown.stdout)
                target.chmod(0o755 if mode == "100755" else 0o644)


def build_stage_repo(repo, case, dest, issue):
    """A fresh repo with two commits: the branch's base on main, and the branch just before the fix.

    It shares no objects with grove and has no remote, so the fix commit and the fix's test are
    not reachable from it. Raises CaseError when that does not hold."""
    title, body = issue
    if case.fix[:7] in f"{title}\n{body}":
        raise CaseError(f"{case.fix[:7]}: the issue names the fix commit")
    dest.mkdir(parents=True)
    env = {**os.environ, "GIT_AUTHOR_NAME": "grove", "GIT_AUTHOR_EMAIL": "grove@localhost",
           "GIT_COMMITTER_NAME": "grove", "GIT_COMMITTER_EMAIL": "grove@localhost"}

    def stage_git(*args):
        subprocess.run(["git", "-C", str(dest), *args], check=True, capture_output=True, env=env)

    stage_git("init", "-q", "-b", "main")
    export_tree(repo, case.base, dest)
    stage_git("add", "-A")
    stage_git("commit", "-qm", "Base")
    stage_git("checkout", "-qb", "change")
    stage_git("rm", "-rqf", "--", ".")
    export_tree(repo, f"{case.fix}^", dest)
    stage_git("add", "-A")
    stage_git("commit", "-qm", title)
    problems = leak_problems(repo, case, dest)
    if problems:
        raise CaseError(f"{case.fix[:7]}: stage repo leaks {', '.join(problems)}")
    return dest


def leak_problems(repo, case, stage_repo):
    problems = []
    if subprocess.run(["git", "-C", str(stage_repo), "cat-file", "-e", case.fix], capture_output=True).returncode == 0:
        problems.append("the fix commit")
    if git(stage_repo, "remote"):
        problems.append("a remote")
    for path in case.answer_key + case.fix_files:
        fixed = subprocess.run(["git", "-C", str(repo), "show", f"{case.fix}:{path}"], capture_output=True).stdout
        before = subprocess.run(["git", "-C", str(repo), "show", f"{case.fix}^:{path}"], capture_output=True).stdout
        target = stage_repo / path
        if fixed != before and target.exists() and target.read_bytes() == fixed:
            problems.append(f"the fixed {path}")
    return problems


def crate_name(tree, crate_dir):
    match = re.search(r'^name\s*=\s*"([^"]+)"', (tree / crate_dir / "Cargo.toml").read_text(), re.M)
    return match.group(1)


def test_commands(tree, answer_key):
    """(folder, argv) for each test file of the answer key."""
    commands = []
    vitest = {}
    for path in answer_key:
        rust = re.match(r"(crates/[^/]+)/tests/([^/]+)\.rs$", path)
        if rust:
            commands.append((".", ["cargo", "test", "-p", crate_name(tree, rust.group(1)), "--test", rust.group(2)]))
        elif re.search(r"\.test\.tsx?$", path):
            package = "app" if path.startswith("app/") else "sidecar/claude"
            vitest.setdefault(package, []).append(path[len(package) + 1:])
        elif re.search(r"(^|/)test_[^/]*\.py$", path):
            folder, name = os.path.split(path)
            commands.append((".", ["python3", "-m", "unittest", "discover", "-s", folder or ".", "-p", name]))
    for package, files in vitest.items():
        commands.append((package, ["npx", "--no-install", "vitest", "run", *files]))
    return commands


def gate_commands(paths):
    """The six gates, plus CI's Python checks, for the areas the paths touch."""
    areas = {area_of(path) for path in paths}
    commands = []
    if "rust" in areas:
        commands += [(".", ["cargo", "fmt", "--check"]),
                     (".", ["cargo", "clippy", "--all-targets", "--", "-D", "warnings"]),
                     (".", ["cargo", "test"])]
    for area, package in (("app", "app"), ("sidecar", "sidecar/claude")):
        if area in areas:
            commands += [(package, ["npx", "--no-install", "tsc", "--noEmit"]),
                         (package, ["npx", "--no-install", "eslint", "."]),
                         (package, ["npx", "--no-install", "vitest", "run"])]
    for folder in sorted({os.path.dirname(path) or "." for path in paths if path.endswith(".py")}):
        commands.append((".", ["python3", "-m", "unittest", "discover", "-s", folder]))
    return commands


def link_node_modules(package_dir):
    """npm ci once per lockfile, then link the result in. Saves a full install per tree."""
    lock = package_dir / "package-lock.json"
    key = hashlib.sha256(lock.read_bytes()).hexdigest()[:16]
    cached = CACHE / "node" / key
    if not (cached / "node_modules").exists():
        cached.mkdir(parents=True, exist_ok=True)
        shutil.copy(package_dir / "package.json", cached)
        shutil.copy(lock, cached)
        subprocess.run(["npm", "ci", "--prefer-offline", "--no-audit", "--no-fund"], cwd=cached, check=True,
                       capture_output=True)
    link = package_dir / "node_modules"
    if not link.exists():
        link.symlink_to(cached / "node_modules")


def passes(argv, cwd, env):
    try:
        return subprocess.run(argv, cwd=cwd, env=env, capture_output=True,
                              timeout=COMMAND_TIMEOUT_SECONDS).returncode == 0
    except subprocess.TimeoutExpired:
        return False


def run_commands(tree, commands):
    """Runs each command in tree. Returns the names of the ones that fail every attempt, and the
    seconds spent running them, not counting the wait for another tree's turn.

    Some older timing tests fail under load, so a failure only counts when it repeats. Trees from
    parallel cases take turns, so the harness adds no load of its own and one tree's build never
    lands in another's run."""
    env = {**os.environ, "CARGO_TARGET_DIR": str(CACHE / "target")}
    failed = []
    with COMMAND_LOCK:
        started = time.monotonic()
        # Cargo shares one target folder between trees and reuses a build newer than the sources.
        # A tree exported before another tree was built would get that tree's build.
        touch_tree(tree)
        for folder, argv in commands:
            cwd = tree / folder
            if argv[0] == "npx":
                link_node_modules(cwd)
            if not any(passes(argv, cwd, env) for _ in range(COMMAND_ATTEMPTS)):
                failed.append(" ".join(argv[2:] if argv[:2] == ["npx", "--no-install"] else argv) + f" (in {folder})")
        return failed, time.monotonic() - started


def touch_tree(tree):
    """Stamps every source with now. Python's bytecode cache checks whole seconds and the size, so it goes too."""
    now = time.time()
    for folder, dirs, files in os.walk(tree):
        for cache in [d for d in dirs if d == "__pycache__"]:
            shutil.rmtree(os.path.join(folder, cache))
        dirs[:] = [d for d in dirs if d not in ("node_modules", "target", ".git", "__pycache__")]
        for name in files:
            path = os.path.join(folder, name)
            if not os.path.islink(path):
                os.utime(path, (now, now))


def prove(repo, case, scratch):
    """The fix's own test fails with the fix left out and passes with it."""
    tree = scratch / "tree"
    export_tree(repo, f"{case.fix}^", tree)
    overlay(repo, case.fix, case.answer_key, tree)
    commands = test_commands(tree, case.answer_key)
    fails_without_fix = bool(run_commands(tree, commands)[0])
    # A fresh export of the fix, since overlaying the fix cannot turn a folder into a file.
    fixed = scratch / "fixed"
    export_tree(repo, case.fix, fixed)
    passes_with_fix = not run_commands(fixed, test_commands(fixed, case.answer_key))[0]
    return Proof(fails_without_fix, passes_with_fix)


def run_gates(repo, case, scratch):
    """Caught when a gate fails just before the fix and passes on it. Gates that fail on both are other findings."""
    commands = gate_commands(case.fix_files + case.answer_key)
    buggy = scratch / "buggy"
    export_tree(repo, f"{case.fix}^", buggy)
    failed_on_bug, seconds = run_commands(buggy, commands)
    if not failed_on_bug:
        return StageResult("missed", seconds=seconds)
    # The whole fix commit: it may rewrite an old test that expected the bug.
    fixed = scratch / "fixed"
    export_tree(repo, case.fix, fixed)
    failed_on_fix, more_seconds = run_commands(fixed, commands)
    failed_on_both = [gate for gate in failed_on_bug if gate in failed_on_fix]
    outcome = "caught" if len(failed_on_both) < len(failed_on_bug) else "false alarm"
    return StageResult(outcome, findings=len(failed_on_bug), other_findings=len(failed_on_both),
                       seconds=seconds + more_seconds, note="failed: " + "; ".join(failed_on_bug))


def stage_prompt(stage, issue):
    title, body = issue
    return f"""{MODEL_STAGES[stage]} Review the change on this branch: `git diff main...HEAD` in this folder. It is the work so far on this GitHub issue:

# {title}

{body}

There is no pull request and no network: read the change with git and the files in this folder, never with gh. Post nothing anywhere. Change no file.

When you are done, end your reply with one fenced json block that lists every finding you report:
```json
{{"findings": [{{"file": "path/to/file", "line": 12, "summary": "what is wrong"}}]}}
```
Use {{"findings": []}} when you report none."""


def last_json_block(text, key):
    for block in reversed(re.findall(r"```json\s*(.*?)```", text, re.S)):
        try:
            data = json.loads(block)
        except json.JSONDecodeError:
            continue
        if isinstance(data, dict) and key in data:
            return data
    return None


def usage_totals(result_event):
    tokens = sum(model.get(k, 0) for model in result_event.get("modelUsage", {}).values()
                 for k in ("inputTokens", "outputTokens", "cacheReadInputTokens", "cacheCreationInputTokens"))
    return tokens, round(result_event.get("total_cost_usd", 0.0), 4)


def judge(repo, case, findings, scratch, claude):
    """Asks a model whether any finding describes the bug the fix commit fixed."""
    listed = "\n".join(f"{n}. {f.get('file')}:{f.get('line')}: {f.get('summary')}" for n, f in enumerate(findings, 1))
    prompt = f"""You score a code review eval. A bug was fixed by the commit below. Its test is the answer key. Decide whether any review finding describes this same bug: the same root cause, even if worded differently or pointing at a nearby line. A finding about something else does not count.

Fix commit diff:
```diff
{git(repo, "show", "--format=%s", case.fix)}
```

Findings:
{listed}

Reply with one fenced json block: {{"caught": true or false, "finding": <number or null>, "reason": "<one sentence>"}}"""
    folder = scratch / "judge"
    folder.mkdir()
    out = subprocess.run([claude, "-p", "--model", JUDGE_MODEL, "--output-format", "json", "--tools", "",
                          "--no-session-persistence", "--strict-mcp-config", "--setting-sources", "project"],
                         input=prompt, cwd=folder, capture_output=True, text=True, timeout=COMMAND_TIMEOUT_SECONDS)
    event = json.loads(out.stdout)
    verdict = last_json_block(event.get("result", ""), "caught")
    if verdict is None:
        raise CaseError(f"judge gave no verdict: {event.get('result', '')[:200]}")
    return verdict, usage_totals(event)[1]


def worktree_paths(repo):
    """Each worktree's path, plus its /var form on macOS, where /var links to /private/var."""
    listed = git(repo, "worktree", "list", "--porcelain")
    paths = []
    for line in listed.splitlines():
        if line.startswith("worktree "):
            path = str(Path(line[len("worktree "):]).resolve())
            paths.append(path)
            if path.startswith("/private/") and Path(path[len("/private"):]).resolve() == Path(path):
                paths.append(path[len("/private"):])
    return paths


def fixed_lines(repo, case, stage_repo):
    """Lines the fix adds that are nowhere in the stage repo. A tool can return one only by reading the fix."""
    added = git(repo, "diff", "--no-color", "-U0", f"{case.fix}^", case.fix, "--", *case.fix_files,
                *case.answer_key)
    lines = {line[1:].strip() for line in added.splitlines() if line.startswith("+") and not line.startswith("+++")}
    # The stage also sees main, in the deleted side of its diff, so a line main has is no secret.
    return [line for line in lines if len(line) >= 12
            and not in_tree(stage_repo, line, []) and not in_tree(stage_repo, line, ["main"])]


def in_tree(stage_repo, line, tree):
    found = subprocess.run(["git", "-C", str(stage_repo), "grep", "-qF", "-e", line, *tree, "--"], capture_output=True)
    if found.returncode not in (0, 1):
        raise CaseError(f"git grep failed in the stage repo: {found.stderr.decode(errors='replace')[-200:]}")
    return found.returncode == 0


def tool_output(events):
    """Text the stage's tools returned. A subagent's reply is model text, which may propose the fix on its own."""
    names = {block.get("id"): block.get("name") for event in events if event.get("type") == "assistant"
             for block in event.get("message", {}).get("content", []) if isinstance(block, dict)}
    texts = []
    for event in events:
        content = event.get("message", {}).get("content") if event.get("type") == "user" else None
        for block in content if isinstance(content, list) else []:
            if not isinstance(block, dict) or names.get(block.get("tool_use_id")) in ("Agent", "Task"):
                continue
            output = block.get("content")
            texts += [output] if isinstance(output, str) else [
                part.get("text", "") for part in output or [] if isinstance(part, dict)]
    return "\n".join(texts)


def plugin_dirs(claude):
    """Where the stages' plugins are installed. A restricted session loads no plugin from settings."""
    try:
        listed = json.loads(subprocess.run([claude, "plugin", "list", "--json"], check=True, capture_output=True,
                                           text=True, stdin=subprocess.DEVNULL,
                                           timeout=PLUGIN_LIST_TIMEOUT_SECONDS).stdout)
    except subprocess.TimeoutExpired:
        raise CaseError(f"claude plugin list did not answer in {PLUGIN_LIST_TIMEOUT_SECONDS} s") from None
    found = {plugin["id"]: plugin["installPath"] for plugin in listed if plugin.get("id") in STAGE_PLUGINS}
    missing = [plugin for plugin in STAGE_PLUGINS if plugin not in found]
    if missing:
        raise CaseError(f"install the plugins the stages run: {', '.join(missing)}")
    return [found[plugin] for plugin in STAGE_PLUGINS]


def without_refusals(events):
    """Drops refused tool calls. A refusal quotes the deny rule, which names the grove path, but nothing was read."""
    refused = {event.get("tool_use_id") for event in events if event.get("subtype") == "permission_denied"}

    def is_refusal(event):
        if event.get("subtype") == "permission_denied":
            return True
        content = event.get("message", {}).get("content") if event.get("type") == "user" else None
        return isinstance(content, list) and bool(content) and all(
            isinstance(block, dict) and block.get("tool_use_id") in refused for block in content)
    return [event for event in events if not is_refusal(event)]


def run_model_stage(stage, repo, case, issue, scratch, claude, scratch_root, other_cases=()):
    stage_repo = build_stage_repo(repo, case, scratch / "repo", issue)
    repo_path = str(Path(git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")).resolve().parent)
    # Every checkout of grove holds the fix, and a worktree can live anywhere.
    checkouts = list(dict.fromkeys([repo_path, str(repo), *worktree_paths(repo)]))
    # So do the build cache, which holds the fixed tree's builds, and other cases' proof and gates trees.
    keys = [f"{root}/{ANSWER_KEY_PREFIX}*" for root in dict.fromkeys([str(scratch_root), str(scratch_root.resolve())])]
    keys += list(dict.fromkeys(form for folder in other_cases for form in (str(folder), str(Path(folder).resolve()))))
    caches = list(dict.fromkeys([str(CACHE), str(CACHE.resolve())]))
    denied = STAGE_DENIED_TOOLS + ["Bash(git -C:*)"]
    denied += [f"{tool}(/{path}/**)" for path in checkouts + caches + keys for tool in ("Read", "Grep", "Glob")]
    # Claude's logs of grove sessions name the fixes. Other logs, the stage's own included, stay readable.
    denied += [f"Read(/{Path.home() / '.claude' / 'projects' / re.sub(r'[^A-Za-z0-9]', '-', path)}*/**)"
               for path in checkouts]
    # Restricted mode ignores the user, project and local settings files, so no allow rule of the caller's applies;
    # managed settings still do. It keeps the file tools inside the stage repo. Plugins come back through
    # --plugin-dir. Source: `claude --help` for --restricted, checked on Claude Code 2.1.327.
    tools = ",".join(dict.fromkeys(rule.split("(")[0] for rule in STAGE_READ_TOOLS))
    # The skills read their own reference files, which hold nothing of the fix.
    plugins = [arg for path in plugin_dirs(claude) for arg in ("--plugin-dir", path, "--add-dir", path)]
    # `git show --output=FILE` and its kin write files, so commands run in a sandbox that may not write to any case
    # folder or the stage repo. Outside the temp folder the sandbox allows no writes anyway.
    no_writes = list(dict.fromkeys([str(scratch_root), str(scratch_root.resolve()), str(stage_repo)]))
    # A command can open a file whose path the deny rules above never see, so the sandbox also refuses reads of
    # every case folder but this stage's own, the grove checkouts, the build cache and Claude's session logs.
    no_reads = list(dict.fromkeys([str(scratch_root), str(scratch_root.resolve()), *checkouts, *caches,
                                   str(Path.home() / ".claude" / "projects")]))
    sandbox = {"sandbox": {"enabled": True, "allowUnsandboxedCommands": False,
                           "filesystem": {"denyWrite": no_writes, "denyRead": no_reads,
                                          "allowRead": list(dict.fromkeys([str(scratch), str(scratch.resolve())]))}}}
    started = time.monotonic()
    out = subprocess.run([claude, "-p", "--output-format", "stream-json", "--verbose", "--no-session-persistence",
                          "--strict-mcp-config", "--restricted", "--tools", tools, *plugins,
                          "--settings", json.dumps(sandbox),
                          "--allowedTools", *STAGE_READ_TOOLS, "--disallowedTools", *denied],
                         input=stage_prompt(stage, issue), cwd=stage_repo, capture_output=True, text=True,
                         timeout=COMMAND_TIMEOUT_SECONDS)
    seconds = time.monotonic() - started
    if os.environ.get("GROVE_EVALS_TRANSCRIPTS"):
        Path(os.environ["GROVE_EVALS_TRANSCRIPTS"], f"{case.fix[:7]}-{stage}.jsonl").write_text(out.stdout)
    events = [json.loads(line) for line in out.stdout.splitlines() if line.startswith("{")]
    events_read = without_refusals(events)
    seen = "\n".join(json.dumps(event) for event in events_read)
    needles = {"fix commit": [case.fix[:7]], "source repo path": checkouts, "build cache": caches,
               "answer key folder": [ANSWER_KEY_PREFIX]}
    leaks = [name for name, found in needles.items() if any(needle in seen for needle in found)]
    returned = tool_output(events_read)
    if any(line in returned for line in fixed_lines(repo, case, stage_repo)):
        leaks.append("fixed code")
    # Other cases' stage repos hold their trees, and an earlier case's fix is often in a later case's tree.
    # A deny rule cannot spare this case's own folder, so naming another one marks the stage leaked.
    # Claude's log folders spell paths with every other character as "-", so the own folder has two spellings.
    own_folder = (scratch.parent.name, re.sub(r"[^A-Za-z0-9]", "-", scratch.parent.name))
    named = re.findall(r"grove-replay-(?!key-)[A-Za-z0-9_-]+", seen)
    if any(not any(name == own or name.startswith(own + "-") for own in own_folder) for name in named):
        leaks.append("another case's folder")
    results = [event for event in events if event.get("type") == "result"]
    if not results:
        return StageResult("error", seconds=seconds, leaks=leaks, note=f"exit {out.returncode}: {out.stderr[-300:]}")
    tokens, cost = usage_totals(results[-1])
    text = "\n".join(event.get("result", "") for event in results)
    found = last_json_block(text, "findings")
    common = dict(tokens=tokens, cost_usd=cost, seconds=seconds, leaks=leaks)
    if leaks:
        return StageResult("leaked", note="transcript names " + ", ".join(leaks), **common)
    if found is None:
        return StageResult("error", note="no findings block in the reply", **common)
    findings = found["findings"]
    if not findings:
        return StageResult("missed", **common)
    verdict, judge_cost = judge(repo, case, findings, scratch, claude)
    caught = bool(verdict.get("caught"))
    return StageResult("caught" if caught else "missed", findings=len(findings),
                       other_findings=len(findings) - caught, judge_cost_usd=judge_cost,
                       note=verdict.get("reason", ""), **common)


def answer_key_folder(scratch_root):
    Path(scratch_root).mkdir(parents=True, exist_ok=True)
    return tempfile.TemporaryDirectory(prefix=ANSWER_KEY_PREFIX, dir=scratch_root)


def run_stage(stage, repo, case, issue, scratch, claude="claude", scratch_root=None, other_cases=()):
    """scratch_root holds every case's folders. Model stages may not read its answer key folders, nor
    other_cases, the folders of the run's other cases: an earlier case's fix is often in a later case's tree."""
    scratch_root = Path(scratch_root or scratch.parent)
    if stage in UNAVAILABLE_STAGES:
        return StageResult("not available", note=UNAVAILABLE_STAGES[stage])
    if stage == "gates":
        with answer_key_folder(scratch_root) as keys:
            return run_gates(repo, case, Path(keys))
    scratch.mkdir(parents=True, exist_ok=True)
    return run_model_stage(stage, repo, case, issue, scratch, claude, scratch_root, other_cases)


def read_issue_from_github(number):
    out = subprocess.run(["gh", "issue", "view", str(number), "--repo", GITHUB_REPO, "--json", "title,body"],
                         check=True, capture_output=True, text=True).stdout
    data = json.loads(out)
    return data["title"], data["body"]


def run_case(repo, spec, stages, scratch_root, claude, read_issue, scratch, other_cases):
    """Runs one case in its folder, scratch, and removes the folder after."""
    try:
        case = load_case(repo, spec["fix"], spec["issue"])
        issue = read_issue(case.issue)
        with answer_key_folder(scratch_root) as keys:
            proof = prove(repo, case, Path(keys))
        log(f"{case.fix[:7]} proof {proof.label}")
        results = {}
        for stage, only in stages.items():
            if only is not None and not any(case.fix.startswith(sha) for sha in only):
                results[stage] = StageResult("not run")
                continue
            try:
                results[stage] = run_stage(stage, repo, case, issue, scratch / stage, claude, scratch_root, other_cases)
            except Exception as error:  # one broken stage must not lose the rest of the run
                results[stage] = StageResult("error", note=str(error)[:300])
            log(f"{case.fix[:7]} {stage} {results[stage].outcome}")
        return {"case": asdict(case), "area": case.area, "proof": asdict(proof),
                "stages": {stage: asdict(result) for stage, result in results.items()}}
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def log(message):
    print(f"[replay {datetime.datetime.now():%H:%M:%S}] {message}", file=sys.stderr, flush=True)


RUN_NAME = re.compile(r"^(\d{4}-\d{2}-\d{2})(?:-(\d+))?\.json$")


def run_key(path):
    match = RUN_NAME.match(path.name)
    return (match.group(1), int(match.group(2) or 1))


def earlier_runs(results_dir):
    return sorted((p for p in results_dir.glob("*.json") if RUN_NAME.match(p.name)), key=run_key)


def next_run_stem(results_dir, date):
    taken = [run_key(p)[1] for p in earlier_runs(results_dir) if run_key(p)[0] == date]
    n = max(taken, default=0) + 1
    return date if n == 1 else f"{date}-{n}"


def proven(case):
    return case["proof"]["fails_without_fix"] and case["proof"]["passes_with_fix"]


def scored_results(run, stage):
    """The stage's results that count: cases whose proof passed, and no leaked or unscored cell."""
    return [case["stages"][stage] for case in run["cases"]
            if proven(case) and stage in case["stages"] and case["stages"][stage]["outcome"] in SCORED]


def stage_tally(run, stage):
    results = scored_results(run, stage)
    return sum(r["outcome"] == "caught" for r in results), len(results)


def render(run, previous):
    stages = run["stages"]
    lines = [f"# Bug replay, {run['date']}", "",
             f"{len(run['cases'])} cases. Each cell says whether the stage found the bug the fix commit fixed.",
             "Proof: the fix's own test fails with the fix left out and passes with it. "
             "A case whose proof fails does not count in the totals.", "",
             "| Issue | Fix | Area | Proof | " + " | ".join(stages) + " |",
             "| --- | --- | --- | --- | " + " | ".join("---" for _ in stages) + " |"]
    for case in run["cases"]:
        proof = "pass" if proven(case) else "FAIL"
        cells = [case["stages"][stage]["outcome"] for stage in stages]
        lines.append(f"| {case['case']['issue']} | `{case['case']['fix'][:7]}` | {case['area']} | {proof} | "
                     + " | ".join(cells) + " |")
    lines += ["", "## Per stage", "",
              "Other findings are findings that do not match the replayed bug. Some may be real bugs, so "
              "this is the most the false alarms can be. Gates catch the bug when a gate fails just before the "
              "fix and passes on it. Gates that fail on both are other findings. The cell is a false alarm when "
              "every gate that failed also fails on the fix. Wall time leaves out waiting for another case's "
              "turn.", "",
              "| Stage | Caught | Missed | False alarms | Other findings | Tokens | Cost (USD) | Judge (USD) | Wall time |",
              "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for stage in stages:
        rows = scored_results(run, stage)
        if not rows:
            reason = next((c["stages"][stage]["outcome"] for c in run["cases"]), "not run")
            lines.append(f"| {stage} | {reason} | | | | | | | |")
            continue
        caught, scored = stage_tally(run, stage)
        lines.append(f"| {stage} | {caught}/{scored} | {sum(r['outcome'] == 'missed' for r in rows)} | "
                     f"{sum(r['outcome'] == 'false alarm' for r in rows)} | {sum(r['other_findings'] for r in rows)} | "
                     f"{sum(r['tokens'] for r in rows):,} | {sum(r['cost_usd'] for r in rows):.2f} | "
                     f"{sum(r['judge_cost_usd'] for r in rows):.2f} | {sum(r['seconds'] for r in rows) / 60:.0f} min |")
    lines += ["", "## Compared with the last run", ""]
    if previous is None:
        lines.append("No earlier run.")
    else:
        lines += [f"Last run: {previous['date']}. Caught over cases scored.", "",
                  "| Stage | This run | Last run |", "| --- | --- | --- |"]
        for stage in stages:
            now = "{}/{}".format(*stage_tally(run, stage))
            before = "{}/{}".format(*stage_tally(previous, stage)) if stage in previous["stages"] else "not run"
            lines.append(f"| {stage} | {now} | {before} |")
    lines += ["", "## Notes", ""]
    for case in run["cases"]:
        for stage in stages:
            result = case["stages"][stage]
            if result["note"] and result["outcome"] not in ("not available", "not run"):
                note = result["note"].replace(" \u2014 ", "; ").replace("\u2014", "; ")
                lines.append(f"- {case['case']['issue']} `{case['case']['fix'][:7]}` {stage}, {result['outcome']}: "
                             f"{note}")
    for error in run.get("errors", []):
        lines.append(f"- {error}")
    lines += ["", "## How the stage never sees the fix", "",
              "- The stage reviews a fresh repo made with `git archive`: one commit for the branch's base on "
              "main, one for the branch just before the fix. It shares no objects with grove and has no remote.",
              "- The harness refuses a stage repo that holds the fix commit, a remote, or a fixed file.",
              "- Trees that hold the fix or its test, for the proof and the gates, live in their own folders and "
              "are deleted as soon as that step ends. The stage may not read the ones other cases are using, nor "
              "any other case's folder in the run.",
              "- The stage session runs in Claude Code's restricted mode: it reads no user, project or local "
              "settings file, though managed settings still apply, and its file tools stay inside the stage repo "
              "and the plugins' folders. Its commands run in Claude Code's sandbox, which may not write to the "
              "stage repo or any case's folder, nor read another case's folder, the grove checkouts, the build "
              "cache or Claude's session logs.",
              "- The stage session may only read and run `git diff main...HEAD`, `git log`, `show` and `status`, "
              "since other `git diff` forms print files outside its repo. Reads of the "
              "grove checkout and of Claude's session logs of grove are denied, and so are `gh` and MCP servers.",
              "- After each stage the harness searches its whole transcript for the fix sha, the grove path, the "
              "answer key folders and other cases' folders, and its tool output for any line the fix adds that the "
              "stage repo lacks. A hit marks the stage `leaked` and it does not count.",
              "- Only the judge sees the fix and its test, in a separate session with no tools.", ""]
    return "\n".join(lines)


def run_replay(repo, cases, stages, results_dir, scratch_root, claude="claude", read_issue=read_issue_from_github,
               date=None, jobs=1):
    """Runs every case through the stages and writes <date>.md and <date>.json. Returns the .md path.

    stages is a list of names, or a dict from name to the fix shas to run it on (None for all)."""
    stages = stages if isinstance(stages, dict) else {stage: None for stage in stages}
    unknown = set(stages) - set(STAGES)
    if unknown:
        raise CaseError(f"unknown stages: {', '.join(sorted(unknown))}")
    for stage, shas in stages.items():
        if shas is not None and not all(shas):
            raise CaseError(f"--stage {stage}: an empty sha in the list")
        missing = [sha for sha in shas or [] if not any(spec["fix"].startswith(sha) for spec in cases)]
        if missing:
            raise CaseError(f"--stage {stage}: not in the selected cases: {', '.join(missing)}")
    date = date or datetime.date.today().isoformat()
    results_dir = Path(results_dir)
    results_dir.mkdir(parents=True, exist_ok=True)
    runs = earlier_runs(results_dir)
    previous = json.loads(runs[-1].read_text()) if runs else None
    done, errors = [], []
    # Every case's folder exists before any stage runs, so each stage can be denied all the others.
    folders = [Path(tempfile.mkdtemp(prefix="grove-replay-", dir=scratch_root)) for _ in cases]
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        futures = {pool.submit(run_case, repo, spec, stages, scratch_root, claude, read_issue, folder,
                               [other for other in folders if other != folder]): spec
                   for spec, folder in zip(cases, folders)}
        for future in concurrent.futures.as_completed(futures):
            try:
                done.append(future.result())
            except CaseError as error:
                errors.append(str(error))
                log(str(error))
            except Exception as error:  # a failed gh call on one case must not lose the others
                message = f"{futures[future]['fix'][:7]}: {type(error).__name__}: {error}"
                errors.append(message[:300])
                log(message)
    order = [spec["fix"] for spec in cases]
    done.sort(key=lambda c: next(i for i, sha in enumerate(order) if c["case"]["fix"].startswith(sha)))
    run = {"date": date, "stages": list(stages), "cases": done, "errors": errors}
    stem = next_run_stem(results_dir, date)
    (results_dir / f"{stem}.json").write_text(json.dumps(run, indent=1) + "\n")
    markdown = results_dir / f"{stem}.md"
    markdown.write_text(render(run, previous))
    return markdown


def parse_stage_args(values):
    stages = {}
    for value in values or ["gates", "code-review"]:
        name, _, shas = value.partition("=")
        stages[name] = shas.split(",") if "=" in value else None
        if stages[name] is not None and not all(stages[name]):
            raise CaseError(f"--stage {value}: an empty sha in the list")
    return stages


def select_cases(cases, shas):
    wanted = shas.split(",")
    missing = [sha for sha in wanted if not any(c["fix"].startswith(sha) for c in cases)]
    if missing:
        raise CaseError(f"not in cases.json: {', '.join(missing)}. Add a case there before checking it.")
    return [c for c in cases if any(c["fix"].startswith(sha) for sha in wanted)]


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["proof", "run"])
    parser.add_argument("--cases", help="comma-separated fix shas from cases.json (default: all)")
    parser.add_argument("--stage", action="append", help=f"one of {', '.join(STAGES)}, optionally =SHA,...")
    parser.add_argument("--jobs", type=int, default=1)
    args = parser.parse_args(argv)
    repo = Path(git(HERE, "rev-parse", "--show-toplevel"))
    cases = json.loads(CASES_FILE.read_text())["cases"]
    if args.cases:
        try:
            cases = select_cases(cases, args.cases)
        except CaseError as error:
            print(error, file=sys.stderr)
            return 2
    if args.command == "proof":
        failed = 0
        for spec in cases:
            case = load_case(repo, spec["fix"], spec["issue"])
            with answer_key_folder(tempfile.gettempdir()) as scratch:
                proof = prove(repo, case, Path(scratch))
            print(f"{proof.label}  issue {case.issue} {case.fix[:7]}: fix test fails without the fix: "
                  f"{proof.fails_without_fix}; passes with it: {proof.passes_with_fix}")
            failed += proof.label != "pass"
        return 1 if failed else 0
    try:
        path = run_replay(repo, cases, parse_stage_args(args.stage), RESULTS_DIR, tempfile.gettempdir(),
                          jobs=args.jobs)
    except CaseError as error:
        print(error, file=sys.stderr)
        return 2
    print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
