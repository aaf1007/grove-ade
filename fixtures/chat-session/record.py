# Record a real Claude chat session through the sidecar, for the load tests.
# Run: python3 fixtures/chat-session/record.py [PATH_TO_CLAUDE] [--keep]
#
# Unlike the load-session recorder, this one uses a real model: it runs the
# sidecar (sidecar/claude, after `npm ci` there) with your claude and your
# login, so it spends about 10 minutes of your usage. Each turn takes a few
# minutes, so it sends the next prompt from PROMPTS until 10 minutes have gone. The agent works in a
# scratch git repo under /tmp, deleted at the end (--keep keeps it). Every tool
# is allowed there, so no approval stops the turn. A request that still comes
# is denied. The recorder writes each line both ways with its time, scrubs
# them and writes session.jsonl. It exits 1 if a turn failed or anything
# personal survived scrubbing, and then writes the result into the scratch
# folder instead.
import getpass
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True  # no __pycache__ next to check.py
sys.path.insert(0, HERE)
from check import SECRETS  # noqa: E402

SIDECAR = os.path.join(HERE, "..", "..", "sidecar", "claude", "src", "main.ts")
OUT = os.path.join(HERE, "session.jsonl")
MIN_SECONDS = 600
TURN_TIMEOUT = 20 * 60
ACCOUNT_EMAIL = "user@example.com"
PROMPTS = [
    """Build a small Python 3 command-line tool in this empty git repo: a Markdown table of contents \
generator, toc.py. Do all of these steps in this one turn, without asking me anything:
1. Write toc.py. It reads a Markdown file, finds ATX headings (# to ######), skips headings inside \
fenced code blocks, makes GitHub-style anchors (lowercase, punctuation removed, spaces to hyphens, \
repeats get -1, -2 and so on) and prints a nested bullet list of links. Options: --min-level, \
--max-level, and --in-place, which replaces the block between <!-- toc --> and <!-- tocstop -->.
2. Write thorough unittest tests in test_toc.py, at least 15 cases, and run them with \
python3 -m unittest -v. Fix any failures.
3. Add sample.md and run the tool on it to show the output.
4. Add setext headings (=== and --- underlines), with tests, and run the tests again.
5. Write README.md with usage and examples.
6. Commit the work with git in a few logical commits.
7. End with a detailed written review of the design: how anchors are built, the edge cases you \
handled and the ones you did not, and what you would do next. Make it at least 800 words.""",
    """Now add a --check option that exits 1 when the table of contents in a file is out of date, \
with tests. Run all the tests, commit, then explain the change and its edge cases in detail.""",
    """Add a way to leave a heading out of the table of contents with a <!-- no-toc --> comment on \
the line before it, with tests. Run all the tests, commit, then write a long summary of the whole \
project as it stands.""",
    """Add a --numbered option that writes an ordered list (1., 2., ...) instead of bullets, with \
tests. Run all the tests, commit, then explain how nesting works for both list styles.""",
    """Read every file in the repo again and review it as a strict reviewer would: list each bug or \
risk you find with its line. Fix the real ones with tests, run all the tests and commit.""",
    """Add a --format json option that prints the headings as a JSON list of objects with level, \
text and anchor, with tests. Run all the tests, commit, and update README.md to cover it.""",
    """Handle headings with inline code, links, emphasis and emoji the way GitHub does when it makes \
anchors, with a test for each case. Run all the tests, commit, then explain each rule you used.""",
    """Split toc.py into a small package (parsing, anchors, rendering, cli) without changing \
behavior, keep toc.py as the entry point, run all the tests and commit. Then write a long summary \
of the final design and of every change since the start.""",
]


class Session:
    """The sidecar, with every line both ways recorded with its time."""

    def __init__(self, cmd, cwd, env, stderr):
        self.start = time.monotonic()
        self.lines = []
        self.lock = threading.Lock()
        self.turn_ended = threading.Event()
        self.error = False
        self.proc = subprocess.Popen(cmd, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=stderr, start_new_session=True)
        threading.Thread(target=self.read, daemon=True).start()

    def record(self, origin, message):
        with self.lock:
            self.lines.append({"t": round(time.monotonic() - self.start, 3), "from": origin, "message": message})

    def send(self, message):
        self.record("core", message)
        self.proc.stdin.write(json.dumps(message).encode() + b"\n")
        self.proc.stdin.flush()

    def read(self):
        for raw in self.proc.stdout:
            try:
                message = json.loads(raw)
            except ValueError:
                print(f"not JSON: {raw[:200]!r}")
                continue
            self.record("sidecar", message)
            kind = message.get("type")
            if kind in ("approval_request", "question_request"):
                print(f"denied a {kind}: {json.dumps(message)[:200]}")
                self.send({"type": "answer", "id": message["id"], "reply": "deny",
                           "message": "Not in this session. Decide for yourself and go on."})
            elif kind == "turn_end":
                print(f"turn end at {self.lines[-1]['t']} s: {json.dumps(message)}")
                self.error = self.error or "error" in message
                self.turn_ended.set()
        # Stops the wait for a turn that never ends.
        self.error = True
        self.turn_ended.set()

    def elapsed(self):
        return time.monotonic() - self.start


# Any email but a made-up one, such as a commit trailer or test data the agent wrote.
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@(?!example\.com\b)[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+")


def replace_strings(value, pairs):
    if isinstance(value, str):
        for old, new in pairs:
            value = value.replace(old, new)
        return EMAIL.sub(ACCOUNT_EMAIL, value)
    if isinstance(value, dict):
        return {k: replace_strings(v, pairs) for k, v in value.items()}
    if isinstance(value, list):
        return [replace_strings(v, pairs) for v in value]
    return value


def scrub(lines, pairs):
    """Drops what replay cannot play without a user (approvals, questions and
    the recorder's answers) and the commands list, which names your own skills
    and plugins. Puts placeholders in for paths and the session id, and a
    made-up account and email addresses."""
    out = []
    for line in lines:
        message = line["message"]
        kind = message.get("type")
        if kind in ("commands", "approval_request", "question_request", "answer"):
            continue
        if kind == "account" and message.get("account"):
            account = message["account"]
            message = {**message, "account": {
                "email": ACCOUNT_EMAIL if account.get("email") else None, "organization": None,
                "plan": account.get("plan"), "api_key_source": account.get("api_key_source")}}
        out.append({**line, "message": replace_strings(message, pairs)})
    return out


def git(repo, *args):
    subprocess.run(["git", "-C", repo, *args], check=True)


def main():
    args = [a for a in sys.argv[1:] if a != "--keep"]
    keep = "--keep" in sys.argv[1:]
    claude = os.path.realpath(args[0] if args else shutil.which("claude") or "claude")
    work = os.path.realpath(tempfile.mkdtemp(prefix="grove-chat-", dir="/tmp"))
    repo, tmp = os.path.join(work, "repo"), os.path.join(work, "tmp")
    os.makedirs(tmp)
    subprocess.run(["git", "init", "-q", repo], check=True)
    # The agent commits, so the repo has its own identity and never signs with yours.
    git(repo, "config", "user.name", "fixture")
    git(repo, "config", "user.email", "fixture@example.com")
    git(repo, "config", "commit.gpgsign", "false")
    settings = os.path.join(work, "settings.json")
    with open(settings, "w") as f:
        json.dump({"permissions": {"defaultMode": "acceptEdits",
                                   "allow": ["Bash", "Edit", "Write", "Read", "Glob", "Grep", "TodoWrite"]},
                   "disableAllHooks": True}, f)
    mcp = os.path.join(work, "mcp.json")
    with open(mcp, "w") as f:
        json.dump({"mcpServers": {}}, f)
    session_id = str(uuid.uuid4())
    cmd = ["node", SIDECAR, "--settings", settings, "--mcp-config", mcp, "--session-id", session_id,
           "--claude", claude]
    print(f"scratch folder {work}")
    with open(os.path.join(work, "sidecar.log"), "wb") as log:
        # Your own shell's environment, without what a host app such as Claude Code passes down,
        # so claude uses the login you made with it.
        env = {k: v for k, v in os.environ.items() if not k.startswith(("CLAUDE", "ANTHROPIC"))}
        s = Session(cmd, repo, {**env, "TMPDIR": tmp + "/"}, log)
        failed = False
        for n, prompt in enumerate(PROMPTS):
            if n and s.elapsed() >= MIN_SECONDS:
                break
            s.turn_ended.clear()
            s.send({"type": "user_message", "text": prompt})
            if not s.turn_ended.wait(TURN_TIMEOUT) or s.error:
                print("turn timed out, failed, or the sidecar stopped")
                failed = True
                break
        s.proc.stdin.close()
        try:
            s.proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            failed = True
        try:
            os.killpg(s.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    if s.elapsed() < MIN_SECONDS:
        print(f"only {s.elapsed():.0f} s")
        failed = True

    home = os.path.realpath(os.path.expanduser("~"))
    user, host = getpass.getuser(), socket.gethostname()
    pairs = [(repo, "{{cwd}}"), (tmp, "/tmp/scratch"), (work, "/tmp/scratch"), (home, "{{home}}"),
             ("/private/tmp/", "/tmp/"), (session_id, "{{session_id}}"), (host, "host"), (user, "user")]
    lines = scrub(s.lines, pairs)
    text = "".join(json.dumps(line, ensure_ascii=False, separators=(",", ":")) + "\n" for line in lines)

    real_name = subprocess.run(["git", "config", "--global", "user.name"], capture_output=True,
                               text=True).stdout.strip()
    accounts = [line["message"]["account"] or {} for line in s.lines if line["message"].get("type") == "account"]
    leftovers = [work, home, user, host, host.split(".")[0], session_id, real_name,
                 *[a.get(k) for a in accounts for k in ("email", "organization")]]
    problems = [f"{x!r} survived" for x in leftovers if x and x in text]
    problems += [f"{name}: {m!r}" for name, pattern in SECRETS.items() for m in re.findall(pattern, text)]
    # A failed or unscrubbed run never replaces the recording in the repo.
    out = os.path.join(work, "session.jsonl.rejected") if problems or failed else OUT
    with open(out, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"wrote {out}: {len(lines)} lines, {len(text.encode())} bytes, {lines[-1]['t'] if lines else 0} s")
    for problem in problems:
        print(f"not scrubbed: {problem}")
    if keep or problems or failed:
        print(f"kept {work}")
    else:
        shutil.rmtree(work)
    sys.exit(1 if failed or problems else 0)


if __name__ == "__main__":
    main()
