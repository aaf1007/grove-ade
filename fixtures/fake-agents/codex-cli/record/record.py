# Record hook payloads, screens and hooks/list answers from the real Codex
# CLI, for the fake. Run: python3 record.py PATH_TO_CODEX [--keep]
#
# Codex runs with a scratch HOME and CODEX_HOME in a new folder under /tmp,
# against fake_model.py on localhost, so it uses no account, no model and no
# usage, and never reads or writes ~/.codex. The flags are the ones grove
# passes. Hooks run relay.py as grove-hook, which saves each payload and its
# environment. The hooks are approved through Codex's own "Hooks need review"
# prompt. It scrubs the results and writes them to ../payloads, ../screens and
# ../app-server, with the CLI version in ../payloads/VERSION. --keep keeps the
# scratch folder.
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

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(HERE)), "claude-code", "record"))
from pty_drive import drive  # noqa: E402

OUT = os.path.dirname(HERE)
KEY = "sk-fixture-dummy-key-not-real-0000"
# The id grove puts in the Codex process environment, and a different one in
# shell_environment_policy.set, to show which one hooks and MCP servers get.
ENV_ID = "grove-env-0000"
POLICY_ID = "grove-policy-1111"
EVENTS = ["PreToolUse", "PermissionRequest", "PostToolUse", "PreCompact", "PostCompact", "SessionStart",
          "SessionEnd", "UserPromptSubmit", "SubagentStart", "SubagentStop", "Stop", "Interrupt"]
TOOL_EVENTS = {"PreToolUse", "PermissionRequest", "PostToolUse"}
# Payload file name suffix, by event: the field matchers are checked against.
QUALIFIER = {"SessionStart": "source", "PreToolUse": "tool_name", "PostToolUse": "tool_name",
             "PermissionRequest": "tool_name"}
# Terminal queries (cursor position, DA1, XTVERSION, kitty keyboard, colors).
# The recorder removes them from screens, so printing one never makes the
# terminal answer.
QUERIES = re.compile(rb"\x1b\[6n|\x1b\[c|\x1b\[>0?q|\x1b\[\?u|\x1b\]1[01];\?(\x07|\x1b\\)")
# Path prefixes that must not survive scrubbing.
LEAKS = ["/Users/", "/home/", "/private/", "/var/folders/", "/tmp/"]


def overrides(hook_command):
    """The -c flags grove passes, with every hook event running hook_command."""
    handler = f"hooks=[{{type=\"command\",command={json.dumps(hook_command)},timeout=1}}]"
    flags = []
    for ev in EVENTS:
        group = f"{{matcher=\"*\",{handler}}}" if ev in TOOL_EVENTS else f"{{{handler}}}"
        flags += ["-c", f"hooks.{ev}=[{group}]"]
    flags += ["-c", 'mcp_servers.grove={command="grove-mcp",env_vars=["GROVE_SESSION_ID"]}',
              "-c", f'shell_environment_policy.set.GROVE_SESSION_ID="{POLICY_ID}"']
    return flags


def sessions(flags, resume_id):
    """Each entry is one TUI launch: (name, args, pty steps). resume_id is
    called after the first launch, once SessionStart has given the id."""
    approve = r"Would\s*you\s*like\s*to\s*run"
    quit_ = [("send", "/quit"), ("sleep", 1), ("send", "\r"), ("sleep", 3)]
    yield ("first", ["--no-daemon", *flags, "GROVE_SHELL"], [
        ("wait", r"Hooks\s*need\s*review", 30), ("sleep", 2), ("snap", "hooks-review"),
        ("send", "\x1b[B"), ("sleep", 0.5), ("send", "\r"),
        ("wait", approve, 30), ("sleep", 2), ("mark",), ("send", "\r"),
        ("wait", r"Done\.", 30), ("sleep", 3), ("snap", "idle-prompt"),
        ("mark",), ("send", "GROVE_SHELL"), ("sleep", 0.5), ("send", "\r"),
        ("wait", approve, 30), ("sleep", 2), ("send", "\x1b"), ("sleep", 3),
        ("mark",), ("send", "GROVE_ASK"), ("sleep", 0.5), ("send", "\r"),
        ("wait", r"Done\.", 30), ("sleep", 3),
    ] + quit_)
    yield ("resume", ["resume", resume_id(), "--no-daemon", *flags, "GROVE_ASK"], [
        ("wait", r"Which\s*color", 30), ("sleep", 2), ("send", "\r"),
        ("wait", r"Done\.", 20), ("sleep", 3),
    ] + quit_)


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def write_config(codex_home, port, repo, extra=""):
    with open(os.path.join(codex_home, "config.toml"), "a") as f:
        f.write(f'''model = "gpt-fixture"
model_provider = "fixture"
approval_policy = "on-request"
sandbox_mode = "workspace-write"
check_for_update_on_startup = false
chatgpt_base_url = "http://127.0.0.1:{port}/backend-api"

[model_providers.fixture]
name = "fixture"
base_url = "http://127.0.0.1:{port}/v1"
env_key = "FIXTURE_API_KEY"
wire_api = "responses"

[analytics]
enabled = false

[tui]
screen_reader_detection_done = true

[projects.{json.dumps(repo)}]
trust_level = "trusted"
''')


def write_wrapper(path, script, log):
    with open(path, "w") as f:
        f.write(f'#!/bin/sh\nexec {shlex.quote(sys.executable)} -I {shlex.quote(script)} {shlex.quote(log)} "$@"\n')
    os.chmod(path, 0o755)


def hooks_list(codex, flags, env, cwd):
    """Runs codex app-server with flags and calls hooks/list. Returns its
    args, every message both ways in order, and how long hooks/list took."""
    args = ["app-server", *flags]
    proc = subprocess.Popen([codex, *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, env=env, cwd=cwd)
    messages = []

    def send(msg):
        messages.append({"client": msg})
        proc.stdin.write((json.dumps(msg) + "\n").encode())
        proc.stdin.flush()

    def until_reply(i):
        while True:
            msg = json.loads(proc.stdout.readline())
            messages.append({"server": msg})
            if msg.get("id") == i and "method" not in msg:
                return

    send({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "grove", "version": "0"}}})
    until_reply(1)
    send({"method": "initialized"})
    start = time.monotonic()
    send({"id": 2, "method": "hooks/list", "params": {}})
    until_reply(2)
    took = time.monotonic() - start
    proc.stdin.close()
    proc.wait(timeout=10)
    return {"args": args, "messages": messages}, took


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
    codex = os.path.realpath(sys.argv[1])
    keep = "--keep" in sys.argv[2:]
    work = os.path.realpath(tempfile.mkdtemp(prefix="grove-rec-", dir="/tmp"))
    home, codex_home, repo, tmp, bin_ = (os.path.join(work, d) for d in ("home", "codex", "repo", "tmp", "bin"))
    for d in (home, codex_home, repo, tmp, bin_):
        os.makedirs(d)
    subprocess.run(["git", "init", "-q", repo], check=True)
    subprocess.run(["git", "-C", repo, "-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid",
                    "commit", "-q", "--allow-empty", "-m", "init"], check=True)
    relay_log, mcp_log = os.path.join(work, "relay.log"), os.path.join(work, "mcp.log")
    write_wrapper(os.path.join(bin_, "grove-hook"), os.path.join(HERE, "relay.py"), relay_log)
    write_wrapper(os.path.join(bin_, "grove-mcp"), os.path.join(HERE, "mcp_stub.py"), mcp_log)

    port = free_port()
    write_config(codex_home, port, repo)
    model = subprocess.Popen([sys.executable, "-I", os.path.join(HERE, "fake_model.py"), str(port),
                              os.path.join(work, "model.log")])
    time.sleep(0.5)
    env = {"HOME": home, "CODEX_HOME": codex_home, "USER": "fixture", "LOGNAME": "fixture",
           "TERM": "xterm-256color", "SHELL": "/bin/zsh" if os.path.exists("/bin/zsh") else "/bin/bash",
           "LANG": "en_US.UTF-8", "TMPDIR": tmp + "/", "PATH": f"{bin_}:/usr/bin:/bin:/usr/sbin:/sbin",
           "FIXTURE_API_KEY": KEY, "GROVE_SESSION_ID": ENV_ID}
    version = subprocess.run([codex, "--version"], env=env, capture_output=True, text=True,
                             check=True).stdout.strip()
    print(f"CLI {version}, scratch folder {work}")

    flags = overrides("grove-hook codex")
    lists = {}
    snaps = {}
    failed = []

    def first_session_id():
        with open(relay_log) as f:
            for line in f:
                p = json.loads(json.loads(line)["stdin"])
                if p["hook_event_name"] == "SessionStart":
                    return p["session_id"]
        raise SystemExit("no SessionStart in the first launch")

    try:
        lists["untrusted"] = hooks_list(codex, flags, env, repo)
        for name, args, steps in sessions(flags, first_session_id):
            print(f"--- {name}")
            raw, got, missed = drive([codex, *args], env, repo, steps)
            with open(os.path.join(work, f"{name}.raw"), "wb") as f:
                f.write(raw)
            snaps.update(got)
            failed += missed
            if name == "first":
                lists["trusted"] = hooks_list(codex, flags, env, repo)
                # The answered question needs this feature. Without it Codex
                # rejects request_user_input, as in the first launch.
                with open(os.path.join(codex_home, "config.toml"), "a") as f:
                    f.write("\n[features]\ndefault_mode_request_user_input = true\n")
        # A per-session argument changes every hook entry, so all need review again.
        lists["modified"] = hooks_list(codex, overrides("grove-hook codex --session x"), env, repo)
    finally:
        model.terminate()

    calls = []
    with open(relay_log) as f:
        calls = [json.loads(line) for line in f if line.strip()]
    payloads = {}
    order = []
    for call in calls:
        p = json.loads(call["stdin"])
        ev = p["hook_event_name"]
        name = f"{ev}-{p[QUALIFIER[ev]]}" if ev in QUALIFIER else ev
        order.append((call["t"], name))
        payloads.setdefault(name, (p, call))

    print("hook calls in order:")
    for i, (t, name) in enumerate(order):
        gap = f" +{(t - order[i - 1][0]) * 1000:.0f} ms" if i else ""
        print(f"  {name}{gap}")
    with open(mcp_log) as f:
        mcp_ids = {json.loads(line)["env"].get("GROVE_SESSION_ID") for line in f}
    hook_ids = {c["env"].get("GROVE_SESSION_ID") for c in calls}
    print(f"GROVE_SESSION_ID seen by hooks: {hook_ids}, by the MCP server: {mcp_ids}")
    added = sorted(set(calls[0]["env"]) - set(env)) if calls else []
    print(f"variables hooks get beyond Codex's own environment: {added}")

    ids = {p["session_id"] for p, _ in payloads.values()}
    subs = [(repo, "{{cwd}}"), (codex_home, "{{codex_home}}"), (home, "{{home}}")]
    subs += [(i, "{{session_id}}") for i in ids]
    host = socket.gethostname()
    leftovers = [work, os.path.expanduser("~"), getpass.getuser(), KEY, host, host.split(".")[0]] + LEAKS
    problems = []

    os.makedirs(os.path.join(OUT, "payloads"), exist_ok=True)
    for name, (p, _) in sorted(payloads.items()):
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

    os.makedirs(os.path.join(OUT, "app-server"), exist_ok=True)
    host_subs = subs + [(host, "fixture-host"), (host.split(".")[0], "fixture-host")]
    for name, (exchange, took) in sorted(lists.items()):
        print(f"hooks/list {name} took {took * 1000:.0f} ms")
        text = json.dumps(scrub(exchange, host_subs), indent=1, ensure_ascii=False) + "\n"
        problems += [f"hooks-list-{name}: {s!r}" for s in leftovers if s in text]
        with open(os.path.join(OUT, "app-server", f"hooks-list-{name}.json"), "w") as f:
            f.write(text)

    os.makedirs(os.path.join(OUT, "screens"), exist_ok=True)
    for name, raw in sorted(snaps.items()):
        for old, new in subs:
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
