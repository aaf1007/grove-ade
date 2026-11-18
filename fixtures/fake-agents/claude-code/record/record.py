# Record hook payloads and screens from the real Claude Code CLI, for the fake.
# Run: python3 record.py PATH_TO_CLAUDE [--keep]
#
# The CLI runs in a scratch HOME and a scratch git repo under the system temp
# folder, against fake_api.py on localhost, so it uses no account, no model and
# no usage, and never reads or writes the user's own config. Hooks run relay.sh,
# which saves each payload. It scrubs the results and writes them to
# ../payloads and ../screens, with the CLI version in ../payloads/VERSION. It
# leaves hand-written payloads alone. --keep keeps the scratch folder.
import getpass
import json
import os
import re
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from pty_drive import drive  # noqa: E402

OUT = os.path.dirname(HERE)
KEY = "sk-ant-fixture-dummy-key-not-real-0000"
EVENTS = ["SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse",
          "Notification", "Stop", "StopFailure", "SubagentStop", "SessionEnd"]
TOOL_EVENTS = {"PreToolUse", "PermissionRequest", "PostToolUse"}
# Payload file name suffix, by event: the field the CLI matches matchers on.
QUALIFIER = {"SessionStart": "source", "SessionEnd": "reason", "PreToolUse": "tool_name",
             "PostToolUse": "tool_name", "PermissionRequest": "tool_name",
             "Notification": "notification_type"}
# Terminal queries (DA1, XTVERSION, kitty keyboard, OSC 7501). The recorder
# removes them from screens, so printing one never makes the terminal answer.
QUERIES = re.compile(rb"\x1b\[c|\x1b\[>0q|\x1b\[\?u|\x1b\]7501;\?(\x07|\x1b\\)")
# Path prefixes that must not survive scrubbing.
LEAKS = ["/Users/", "/home/", "/private/", "/var/folders/", "/tmp/"]


def sessions(first_id, error_id):
    """Each entry is one CLI launch: (name, args, pty steps)."""
    exit_twice = [("send", "\x03"), ("sleep", 1), ("send", "\x03"), ("sleep", 2)]
    return [
        ("first", ["--session-id", first_id, "--permission-mode", "default", "GROVE_BASH"], [
            ("wait", r"trust\s*this\s*folder", 30), ("sleep", 2), ("snap", "trust-dialog"),
            ("send", "\x1b[B"), ("sleep", 0.5), ("send", "\r"),
            # Notification fires about 6 s after the permission prompt shows.
            ("wait", r"Do\s*you\s*want", 30), ("sleep", 8),
            ("mark",), ("send", "\r"),
            ("wait", r"Done\.", 30), ("sleep", 3), ("snap", "idle-prompt"),
            ("send", "/exit"), ("sleep", 1), ("send", "\r"), ("sleep", 3),
        ]),
        ("resume", ["--resume", first_id, "--permission-mode", "default", "GROVE_ASK"], [
            ("wait", r"Which\s*color", 30), ("sleep", 3), ("send", "\r"),
            ("wait", r"Done\.", 20), ("sleep", 2),
            ("mark",), ("send", "/clear"), ("sleep", 1), ("send", "\r"), ("sleep", 4),
            ("send", "GROVE_AGENT"), ("sleep", 0.5), ("send", "\r"),
            ("wait", r"Hi\.", 30), ("sleep", 3),
        ] + exit_twice),
        ("error", ["--session-id", error_id, "GROVE_FAIL"], [
            ("wait", r"forced\s*failure", 30), ("sleep", 3), ("snap", "error-prompt"),
        ] + exit_twice),
    ]


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def write_settings(path, relay_log):
    command = f"{shlex.quote(os.path.join(HERE, 'relay.sh'))} {shlex.quote(relay_log)}"
    handler = {"hooks": [{"type": "command", "command": command, "timeout": 5}]}
    hooks = {ev: [{"matcher": "*", **handler} if ev in TOOL_EVENTS else handler] for ev in EVENTS}
    with open(path, "w") as f:
        json.dump({"hooks": hooks}, f, indent=1)


def scrub(value, subs):
    if isinstance(value, str):
        for old, new in subs:
            value = value.replace(old, new)
        return value
    if isinstance(value, dict):
        return {k: scrub(v, subs) for k, v in value.items()}
    if isinstance(value, list):
        return [scrub(v, subs) for v in value]
    return value


def main():
    claude = os.path.realpath(sys.argv[1])
    keep = "--keep" in sys.argv[2:]
    work = os.path.realpath(tempfile.mkdtemp(prefix="grove-record-"))
    home, repo, tmp = (os.path.join(work, d) for d in ("home", "repo", "tmp"))
    for d in (home, repo, tmp):
        os.makedirs(d)
    subprocess.run(["git", "init", "-q", repo], check=True)
    subprocess.run(["git", "-C", repo, "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                    "commit", "-q", "--allow-empty", "-m", "init"], check=True)
    # Skip onboarding and the "use this API key?" dialog. The key is a dummy.
    with open(os.path.join(home, ".claude.json"), "w") as f:
        json.dump({"hasCompletedOnboarding": True,
                   "customApiKeyResponses": {"approved": [KEY[-20:]], "rejected": []}}, f)
    relay_log = os.path.join(work, "relay.log")
    settings = os.path.join(work, "settings.json")
    write_settings(settings, relay_log)

    port = free_port()
    api = subprocess.Popen([sys.executable, "-I", os.path.join(HERE, "fake_api.py"), str(port),
                            os.path.join(work, "api.log")])
    time.sleep(0.5)
    env = {"HOME": home, "USER": "fixture", "LOGNAME": "fixture", "TERM": "xterm-256color",
           "SHELL": "/bin/zsh" if os.path.exists("/bin/zsh") else "/bin/bash",
           "LANG": "en_US.UTF-8", "TMPDIR": tmp + "/", "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
           "DISABLE_AUTOUPDATER": "1", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
           "ANTHROPIC_API_KEY": KEY, "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{port}"}
    version = subprocess.run([claude, "--version"], env=env, capture_output=True, text=True,
                             check=True).stdout.strip()
    print(f"CLI {version}, scratch folder {work}")

    snaps = {}
    failed = []
    try:
        for name, args, steps in sessions(str(uuid.uuid4()), str(uuid.uuid4())):
            print(f"--- {name}")
            raw, got, missed = drive([claude, "--settings", settings, *args], env, repo, steps)
            with open(os.path.join(work, f"{name}.raw"), "wb") as f:
                f.write(raw)
            snaps.update(got)
            failed += missed
    finally:
        api.terminate()

    payloads = {}
    with open(relay_log) as f:
        for line in f:
            if line.strip():
                p = json.loads(line)
                ev = p["hook_event_name"]
                name = f"{ev}-{p[QUALIFIER[ev]]}" if ev in QUALIFIER else ev
                payloads.setdefault(name, p)

    slug = re.sub(r"[^A-Za-z0-9]", "-", repo)
    cli_tmp = os.path.join(os.path.realpath("/tmp"), f"claude-{os.getuid()}")
    ids = {p["session_id"] for p in payloads.values()}
    subs = [(repo, "{{cwd}}"), (home, "{{home}}"), (cli_tmp, "{{tmp}}"), (slug, "{{project}}")]
    subs += [(i, "{{session_id}}") for i in ids]
    leftovers = [work, os.path.expanduser("~"), getpass.getuser(), KEY] + LEAKS
    problems = []

    os.makedirs(os.path.join(OUT, "payloads"), exist_ok=True)
    for name, p in sorted(payloads.items()):
        p = scrub(p, subs)
        if p["hook_event_name"] == "UserPromptSubmit":
            p["prompt"] = "{{prompt}}"
        text = json.dumps(p, indent=2, ensure_ascii=False) + "\n"
        problems += [f"{name}: {s!r}" for s in leftovers if s in text]
        with open(os.path.join(OUT, "payloads", f"{name}.json"), "w") as f:
            f.write(text)
        print(f"payload {name}")
    with open(os.path.join(OUT, "payloads", "VERSION"), "w") as f:
        f.write(version + "\n")

    os.makedirs(os.path.join(OUT, "screens"), exist_ok=True)
    for name, raw in sorted(snaps.items()):
        for old, new in subs[:2]:
            raw = raw.replace(old.encode(), new.encode())
        raw = QUERIES.sub(b"", raw)
        problems += [f"screen {name}: {s!r}" for s in leftovers if s.encode() in raw]
        with open(os.path.join(OUT, "screens", f"{name}.ansi"), "wb") as f:
            f.write(raw)
        print(f"screen {name}")

    for step in failed:
        print(f"wait timed out: {step}")
    for problem in problems:
        print(f"not scrubbed: {problem}")
    if keep:
        print(f"kept {work}")
    else:
        shutil.rmtree(work)
    sys.exit(1 if failed or problems else 0)


if __name__ == "__main__":
    main()
