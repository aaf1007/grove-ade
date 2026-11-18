#!/usr/bin/env python3
"""Drive grove's core with no window, for the verify skill's headless surface.

Every run lives in its own folder, /tmp/grove-verify.XXXXXX, with its own HOME, so runs can go
side by side. Standard library only. Run from anywhere in the repo:

  headless.py start                      build, start a run, print its folder
  headless.py doctor <run>               is this run worth driving? exits 1 if not
  headless.py call <run> <cmd> [k=v|k:=json ...]
                                         one request to the core, prints the JSON reply
  headless.py wait <run> <file> <regex> [--timeout S] [--after LINES]
                                         wait for a line in <run>/evidence/<file>, after the
                                         first LINES lines (take LINES from `wc -l` before acting)
  headless.py settle <run> <file> [--for S]
                                         wait until <run>/evidence/<file> gets no new line for
                                         S seconds (default 0.5), so a late line from an
                                         earlier step is less likely to land after a count.
                                         It does not rule that out.
  headless.py grovecode <run> <session> <args ...>
                                         run grovecode as that agent session's shell would
  headless.py restart <run>              quit the run's core and start it again on the same HOME
  headless.py stop <run>                 stop the core this run started, keep the evidence

`call` takes key=text for a string and key:=value for JSON, as in `session:=0`. Commands:
ping, add_repo path,
worktrees, create_worktree repo branch, spawn_agent worktree agent=claude|codex
mode=chat|terminal [prompt], spawn_shell worktree, follow session, send session text, answer session request
reply=allow_once|allow_session|deny|[[answers]], interrupt session, set_model session model
[effort], set_mode session mode, write session text, kill session, diff worktree
base=head|merge_base, workspace worktree, undo_workspace worktree, add_comment worktree path
start_line end_line body, comments worktree, send_review_batch worktree [session], profiles,
suggest_profile repo, use_profile repo name, attention, sessions worktree, quit.
"""

import json
import os
import pwd
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path

SKILL = Path(__file__).resolve().parent
REPO = SKILL.parents[2]
FAKES = REPO / "fixtures" / "fake-agents"
TARGET = REPO / "target" / "debug"
DRIVER = TARGET / "examples" / "headless"
REAL_HOME = Path(pwd.getpwuid(os.getuid()).pw_dir)

# Each fake reads its scenario and log from its environment, so a wrapper sets them.
WRAPPERS = {
    "claude": {
        "FAKE_CLAUDE_SCENARIO": "{run}/scenarios/claude.json",
        "FAKE_CLAUDE_LOG": "{run}/evidence/fake-claude.jsonl",
        "FAKE_CLAUDE_RECORD": "{run}/home/.fake-record/claude",
    },
    "claude-sidecar": {
        "FAKE_CLAUDE_SIDECAR_SCENARIO": "{run}/scenarios/claude-sidecar.json",
        "FAKE_CLAUDE_SIDECAR_LOG": "{run}/evidence/fake-claude-sidecar.jsonl",
        "FAKE_CLAUDE_RECORD": "{run}/home/.fake-record/claude",
    },
    "codex": {
        "FAKE_CODEX_SCENARIO": "{run}/scenarios/codex.json",
        "FAKE_CODEX_APP_SERVER_SCENARIO": "{run}/scenarios/codex-app-server.json",
        "FAKE_CODEX_LOG": "{run}/evidence/fake-codex.jsonl",
        "FAKE_CODEX_RECORD": "{run}/home/.fake-record/codex",
    },
}


def fail(message):
    print(f"headless: {message}", file=sys.stderr)
    sys.exit(1)


def run_folder(arg):
    run = Path(arg).resolve()
    if not run.name.startswith("grove-verify.") or not (run / "home").is_dir():
        fail(f"{arg} is not a run folder that `headless.py start` made")
    return run


def request(run, message, timeout=30):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
        s.settimeout(timeout)
        try:
            s.connect(str(run / "control.sock"))
        except OSError as error:
            fail(f"the run's core is not answering ({error}). Run `headless.py doctor {run}`.")
        s.sendall((json.dumps(message) + "\n").encode())
        reply = b""
        while not reply.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                break
            reply += chunk
    return json.loads(reply)


def driver_pid(run):
    """The pid of the core this run started, if it is still that process."""
    try:
        pid = int((run / "driver.pid").read_text())
    except (OSError, ValueError):
        return None
    command = subprocess.run(
        ["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True
    ).stdout.strip()
    # A reused pid is someone else's process.
    return pid if command.startswith(str(DRIVER)) and str(run) in command else None


def launch(run):
    """Starts the run's core on its own HOME and waits until it answers."""
    env = {**os.environ, "HOME": str(run / "home"), "GIT_CONFIG_NOSYSTEM": "1"}
    (run / "control.sock").unlink(missing_ok=True)
    with open(run / "evidence" / "driver.out", "ab") as out:
        driver = subprocess.Popen(
            [str(DRIVER), str(run)], env=env, stdin=subprocess.DEVNULL, stdout=out,
            stderr=subprocess.STDOUT, start_new_session=True,
        )
    (run / "driver.pid").write_text(str(driver.pid))
    deadline = time.time() + 30
    while not (run / "control.sock").exists():
        if driver.poll() is not None or time.time() > deadline:
            fail(f"the core did not start. See {run}/evidence/driver.out")
        time.sleep(0.1)


def start(_args):
    # Two builds: a target filter such as --example applies to every package on the line.
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "grove-core", "--example", "headless"],
        cwd=REPO, check=True,
    )
    subprocess.run(
        ["cargo", "build", "--quiet", "-p", "grove-mcp", "-p", "grove-hook", "--bins"],
        cwd=REPO, check=True,
    )
    # macOS caps a socket path at 104 bytes, so the run lives in /tmp, not the scratchpad.
    run = Path(tempfile.mkdtemp(prefix="grove-verify.", dir="/tmp")).resolve()
    home = run / "home"
    for folder in ("home", "bin", "evidence"):
        (run / folder).mkdir()
    shutil.copytree(SKILL / "scenarios", run / "scenarios")

    account = (run / "scenarios" / "codex-account.json").read_text().strip()
    for name, variables in WRAPPERS.items():
        exports = " ".join(f"{k}='{v.format(run=run)}'" for k, v in variables.items())
        if name == "codex":
            exports += f" FAKE_CODEX_ACCOUNT='{account}'"
        wrapper = run / "bin" / name
        wrapper.write_text(f'#!/bin/sh\nexport {exports}\nexec "{FAKES / name}" "$@"\n')
        wrapper.chmod(0o755)

    # A test repo inside the throwaway home. Never the owner's real repos.
    repo = home / "repo"
    git = ["git", "-c", "init.defaultBranch=main"]
    env = {**os.environ, "HOME": str(home), "GIT_CONFIG_NOSYSTEM": "1"}
    subprocess.run(git + ["init", "-q", str(repo)], check=True, env=env)
    for key, value in (("user.name", "grove verify"), ("user.email", "verify@example.com")):
        subprocess.run(["git", "-C", str(repo), "config", key, value], check=True, env=env)
    (repo / "README.md").write_text("verify\n")
    subprocess.run(["git", "-C", str(repo), "add", "."], check=True, env=env)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "start"], check=True, env=env)

    launch(run)
    reply = request(run, {"cmd": "add_repo", "path": str(repo)})
    if "error" in reply:
        fail(f"add_repo: {reply['error']}")
    print(run)


def doctor(args):
    run = run_folder(args[0])
    problems = []
    pid = driver_pid(run)
    if pid is None:
        problems.append("the core this run started is not running")
    else:
        reply = request(run, {"cmd": "ping"}, timeout=5)
        home = reply.get("ok", {}).get("home")
        if home != str(run / "home"):
            problems.append(f"the core's HOME is {home}, not {run / 'home'}")
        if Path(home or "") == REAL_HOME:
            problems.append("the core runs on the owner's real home")
        if not (run / "home" / ".grovecode" / "grove.sock").is_socket():
            problems.append("no grove.sock under the run's home")
        worktrees = request(run, {"cmd": "worktrees"}, timeout=5).get("ok", [])
        if not any(str(run / "home" / "repo") in w for w in worktrees):
            problems.append("the test repo is not tracked")
    for binary in ("grove-mcp", "grovecode", "grove-hook", "examples/headless"):
        if not (TARGET / binary).exists():
            problems.append(f"target/debug/{binary} is missing. Run `headless.py start` again.")
    for problem in problems:
        print(f"FAIL {problem}")
    if problems:
        sys.exit(1)
    print(f"OK run {run}, core pid {pid}, home {run / 'home'}, repo {run / 'home' / 'repo'}")


def call(args):
    run = run_folder(args[0])
    message = {"cmd": args[1]}
    for pair in args[2:]:
        key, _, value = pair.partition("=")
        # httpie style: key=text is a string, key:=value is JSON.
        if key.endswith(":"):
            message[key[:-1]] = json.loads(value)
        else:
            message[key] = value
    reply = request(run, message)
    print(json.dumps(reply, indent=2))
    if "error" in reply:
        sys.exit(1)


def option(args, name, default):
    """Removes `name value` from args and returns the value, or default."""
    if name not in args:
        return default, args
    i = args.index(name)
    return args[i + 1], args[:i] + args[i + 2:]


def wait(args):
    timeout, args = option(args, "--timeout", 10.0)
    after, args = option(args, "--after", 0)
    try:
        timeout, after = float(timeout), int(after)
    except ValueError:
        fail(f"--timeout takes seconds and --after a line count, not {timeout!r} and {after!r}")
    run = run_folder(args[0])
    path, pattern = run / "evidence" / args[1], re.compile(args[2])
    deadline = time.time() + timeout
    while time.time() < deadline:
        text = path.read_text(errors="replace") if path.exists() else ""
        for line in text.splitlines()[after:]:
            if pattern.search(line):
                print(line)
                return
        time.sleep(0.1)
    fail(f"no line matching {args[2]!r} in {path} after {timeout} s")


def settle(args):
    quiet, args = option(args, "--for", 0.5)
    quiet = float(quiet)
    path = run_folder(args[0]) / "evidence" / args[1]
    deadline = time.time() + 30
    size, since = -1, time.time()
    while time.time() < deadline:
        now = path.stat().st_size if path.exists() else 0
        if now != size:
            size, since = now, time.time()
        elif time.time() - since >= quiet:
            return
        time.sleep(0.05)
    fail(f"{path} kept growing for 30 s")


def grovecode(args):
    run = run_folder(args[0])
    session = args[1]
    # The fakes log the environment grove spawned them with, which is what the agent's shell has.
    env = None
    for log in sorted((run / "evidence").glob("fake-*.jsonl")):
        for line in log.read_text().splitlines():
            spawned = json.loads(line).get("env") or {}
            if spawned.get("GROVE_SESSION_ID") == session:
                env = spawned
    if env is None:
        fail(f"no fake agent log names session {session}")
    child = {**os.environ, "HOME": str(run / "home")}
    for key in ("GROVE_SESSION_ID", "GROVE_SESSION_TOKEN", "GROVE_SOCKET"):
        child[key] = env[key]
    result = subprocess.run(
        [str(TARGET / "grovecode"), *args[2:]], env=child, capture_output=True, text=True
    )
    with open(run / "evidence" / "grovecode.log", "a") as log:
        log.write(json.dumps({"session": session, "args": args[2:], "code": result.returncode,
                              "stdout": result.stdout, "stderr": result.stderr}) + "\n")
    sys.stdout.write(result.stdout)
    sys.stderr.write(result.stderr)
    sys.exit(result.returncode)


def restart(args):
    """Quits the run's core and starts it again on the same HOME, as quitting and reopening grove."""
    run = run_folder(args[0])
    stop([str(run)])
    launch(run)
    print(f"restarted. Core pid {driver_pid(run)}")


def stop(args):
    run = run_folder(args[0])
    pid = driver_pid(run)
    if pid is None:
        print(f"no core of this run is running. Evidence: {run}/evidence")
        return
    try:
        request(run, {"cmd": "quit"}, timeout=10)
    except (OSError, ValueError, SystemExit) as error:
        # A core that does not answer still gets the bounded wait and the kill below.
        print(f"quit got no answer ({error or 'no connection'}), stopping by pid")
    deadline = time.time() + 10
    while driver_pid(run) is not None and time.time() < deadline:
        time.sleep(0.1)
    # Only the pid this run started, and only while it is still that process.
    if driver_pid(run) is not None:
        os.kill(pid, signal.SIGKILL)
    print(f"stopped core pid {pid}. Evidence: {run}/evidence")


COMMANDS = {"start": start, "doctor": doctor, "call": call, "wait": wait, "settle": settle,
            "grovecode": grovecode, "restart": restart, "stop": stop}

if __name__ == "__main__":
    if len(sys.argv) < 2 or sys.argv[1] not in COMMANDS:
        print(__doc__)
        sys.exit(2)
    COMMANDS[sys.argv[1]](sys.argv[2:])
