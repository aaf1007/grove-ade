# Record a busy Claude Code session with `script`, for the load tests.
# Run: python3 fixtures/load-session/record.py PATH_TO_CLAUDE [--keep]
#
# Isolated like #17's recorder: the real CLI runs in a scratch HOME and a
# scratch git repo under the system temp folder, against load_api.py on
# localhost with a dummy key. It uses no account, no model and no usage, and
# never reads or writes the user's own config. #17's pty_drive.py types into
# `script`, which runs the CLI and records its output. The recorder scrubs it
# and writes session.ansi. It exits 1 if the wait timed out or anything
# personal survived scrubbing. --keep keeps the scratch folder.
import getpass
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.dont_write_bytecode = True  # no __pycache__ next to the imported modules
sys.path.insert(0, os.path.join(HERE, "..", "fake-agents", "claude-code", "record"))
from check import INPUT_MODE, QUERY  # noqa: E402
from pty_drive import COLS, ROWS, drive  # noqa: E402

OUT = os.path.join(HERE, "session.ansi")
KEY = "sk-ant-fixture-dummy-key-not-real-0000"
PROMPT = "GROVE_LOAD Add a token bucket rate limiter with tests and a README."
STEPS = [
    ("wait", r"whole\s*change", 240), ("sleep", 3),
    ("send", "/exit"), ("sleep", 1), ("send", "\r"), ("sleep", 3),
]


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main():
    claude = os.path.realpath(sys.argv[1])
    keep = "--keep" in sys.argv[2:]
    work = os.path.realpath(tempfile.mkdtemp(prefix="grove-load-"))
    home, tmp = os.path.join(work, "home"), os.path.join(work, "tmp")
    repo = os.path.join(home, "project")
    for d in (home, tmp, repo):
        os.makedirs(d)
    subprocess.run(["git", "init", "-q", repo], check=True)
    subprocess.run(["git", "-C", repo, "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                    "commit", "-q", "--allow-empty", "-m", "Initial commit"], check=True)
    # Skip onboarding, the "use this API key?" dialog and the trust dialog.
    with open(os.path.join(home, ".claude.json"), "w") as f:
        json.dump({"hasCompletedOnboarding": True,
                   "customApiKeyResponses": {"approved": [KEY[-20:]], "rejected": []},
                   "projects": {repo: {"hasTrustDialogAccepted": True}}}, f)

    port = free_port()
    api = subprocess.Popen([sys.executable, "-I", "-B", os.path.join(HERE, "load_api.py"), str(port),
                            os.path.join(work, "api.log"), repo])
    time.sleep(0.5)
    env = {"HOME": home, "USER": "fixture", "LOGNAME": "fixture", "TERM": "xterm-256color",
           "COLORTERM": "truecolor", "SHELL": "/bin/zsh" if os.path.exists("/bin/zsh") else "/bin/bash",
           "LANG": "en_US.UTF-8", "TMPDIR": tmp + "/", "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
           "DISABLE_AUTOUPDATER": "1", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
           "ANTHROPIC_API_KEY": KEY, "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}"}
    raw_path = os.path.join(work, "session.raw")
    session_id = str(uuid.uuid4())
    # Set the size before `script` starts: it copies it to the CLI's terminal.
    # The = form, since --allowedTools takes several values and would swallow the prompt.
    cmd = ["/bin/sh", "-c", f'stty rows {ROWS} cols {COLS} && exec script -q "$@"', "sh", raw_path,
           claude, "--session-id", session_id, "--permission-mode", "acceptEdits", "--allowedTools=Bash", PROMPT]
    print(f"scratch folder {work}")
    try:
        _, _, failed = drive(cmd, env, repo, STEPS)
    finally:
        api.terminate()

    with open(raw_path, "rb") as f:
        raw = f.read()
    # The exit message prints the session id. Paths are there in case a newer CLI prints them.
    for old, new in [(repo, "~/project"), (home, "~"), (session_id, "{{session_id}}")]:
        raw = raw.replace(old.encode(), new.encode())
    raw = INPUT_MODE.sub(b"", QUERY.sub(b"", raw))
    leftovers = [work, os.path.expanduser("~"), getpass.getuser(), socket.gethostname(), KEY]
    problems = [s for s in leftovers if s.encode() in raw]
    with open(OUT, "wb") as f:
        f.write(raw)
    print(f"wrote {OUT}, {len(raw)} bytes")
    for step in failed:
        print(f"wait timed out: {step}")
    for problem in problems:
        print(f"not scrubbed: {problem!r}")
    if keep:
        print(f"kept {work}")
    else:
        shutil.rmtree(work)
    sys.exit(1 if failed or problems else 0)


if __name__ == "__main__":
    main()
