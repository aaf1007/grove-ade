# Tests for the fake Codex agent. Run: python3 -m unittest discover fixtures/fake-agents
import json
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import unittest
import uuid

HERE = os.path.dirname(os.path.abspath(__file__))
FAKE = os.path.join(HERE, "codex")
DATA = os.path.join(HERE, "codex-cli")
# The events issue #42 asks for. The rejected question is PreToolUse-request_user_input then Stop.
ISSUE_42_PAYLOADS = ["SessionStart-startup", "SessionStart-resume", "UserPromptSubmit", "PreToolUse-Bash",
                     "PostToolUse-Bash", "PermissionRequest-Bash", "Interrupt", "Stop",
                     "PreToolUse-request_user_input", "PostToolUse-request_user_input"]
SESSION = "019a2b3c-4d5e-7f60-8a1b-2c3d4e5f6a7b"
# Writes one JSON line per call: the hook's extra args, its stdin and its environment.
STUB = ('import json,os,sys; open(sys.argv[1],"a").write(json.dumps('
        '{"args":sys.argv[2:],"stdin":sys.stdin.read(),"env":dict(os.environ)})+"\\n")')


def toml(value):
    """A TOML inline value, as grove writes -c overrides."""
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, str):
        return json.dumps(value)
    if isinstance(value, list):
        return "[" + ", ".join(toml(v) for v in value) + "]"
    return "{" + ", ".join(f"{k} = {toml(v)}" for k, v in value.items()) + "}"


class AppServer:
    """The fake app-server as a running process: JSON-RPC lines both ways."""

    def __init__(self, argv, cwd, env):
        self.proc = subprocess.Popen(argv, cwd=cwd, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE)
        self.next_id = 100

    def write(self, msg):
        self.proc.stdin.write(json.dumps(msg).encode() + b"\n")
        self.proc.stdin.flush()

    def read(self, timeout=10):
        """The next message. Kills the fake if none comes in time, so a stall fails instead of hanging."""
        timer = threading.Timer(timeout, self.proc.kill)
        timer.start()
        try:
            line = self.proc.stdout.readline()
        finally:
            timer.cancel()
        if not line:
            raise AssertionError(f"the fake closed stdout or sent nothing for {timeout}s: "
                                 f"{self.proc.stderr.read()!r}")
        return json.loads(line)

    def request(self, method, params):
        """Sends a request and returns its reply, skipping notifications. Fails
        on a server request in between."""
        self.next_id += 1
        self.write({"id": self.next_id, "method": method, "params": params})
        msg = self.read()
        while "id" not in msg:
            msg = self.read()
        if msg["id"] != self.next_id or "method" in msg:
            raise AssertionError(f"want the reply to {method}, got {msg}")
        return msg

    def handshake(self):
        self.request("initialize", {"clientInfo": {"name": "grove", "version": "0"}})
        self.write({"method": "initialized"})
        self.read()

    def close(self):
        """Closes stdin and returns the exit code."""
        if self.proc.stdin and not self.proc.stdin.closed:
            self.proc.stdin.close()
        try:
            code = self.proc.wait(timeout=10)
        finally:
            self.proc.stdout.close()
            self.proc.stderr.close()
        return code


class FakeCodexTest(unittest.TestCase):
    def setUp(self):
        self.dir = os.path.realpath(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)
        self.cwd = os.path.join(self.dir, "worktree")
        os.mkdir(self.cwd)
        self.log = os.path.join(self.dir, "fake.log")
        self.calls = os.path.join(self.dir, "calls.jsonl")

    def write(self, name, value):
        path = os.path.join(self.dir, name)
        with open(path, "w") as f:
            f.write(value if isinstance(value, str) else json.dumps(value))
        return path

    def hook(self, *args):
        return {"type": "command", "command": shlex.join([sys.executable, "-c", STUB, self.calls, *args])}

    def hooks(self, events=("SessionStart", "Stop")):
        """-c overrides whose hooks record each call, tagged with the event name."""
        flags = []
        for ev in events:
            flags += ["-c", f"hooks.{ev}={toml([{'hooks': [self.hook(ev)]}])}"]
        return flags

    def run_fake(self, steps, args=None, stdin=b"", env=None):
        if args is None:
            args = ["--no-daemon", *self.hooks()]
        # Hooks run through a login shell, so HOME and SHELL keep the developer's profile out.
        full = dict({k: v for k, v in os.environ.items() if not k.startswith(("FAKE_CODEX_", "GROVE_", "CODEX_"))},
                    HOME=self.dir, SHELL="/bin/sh", FAKE_CODEX_SCENARIO=self.write("scenario.json", {"steps": steps}),
                    FAKE_CODEX_LOG=self.log)
        full.update(env or {})
        return subprocess.run([FAKE, *args], input=stdin, capture_output=True, cwd=self.cwd, env=full, timeout=30)

    def hook_calls(self):
        if not os.path.exists(self.calls):
            return []
        with open(self.calls) as f:
            calls = [json.loads(line) for line in f]
        return [(c["args"], json.loads(c["stdin"])) for c in calls]

    def log_records(self):
        with open(self.log) as f:
            return [json.loads(line) for line in f]

    def test_fires_a_hook_from_a_c_override_with_the_recorded_payload(self):
        result = self.run_fake([{"hook": "Stop"}])

        self.assertEqual(result.returncode, 0, result.stderr)
        [(args, payload)] = self.hook_calls()
        self.assertEqual(args, ["Stop"])
        self.assertEqual(payload["hook_event_name"], "Stop")
        self.assertEqual(payload["cwd"], self.cwd)
        uuid.UUID(payload["session_id"])

    def test_every_recorded_payload_fires_with_its_placeholders_filled(self):
        names = sorted(f[:-5] for f in os.listdir(os.path.join(DATA, "payloads")) if f.endswith(".json"))
        self.assertLessEqual(set(ISSUE_42_PAYLOADS), set(names))
        with open(os.path.join(DATA, "payloads", "VERSION")) as f:
            self.assertEqual(f.read(), "codex-cli 0.158.0\n")
        events = sorted({name.split("-")[0] for name in names})
        home = os.path.join(self.dir, "codex-home")

        result = self.run_fake([{"hook": n} for n in names], ["--no-daemon", *self.hooks(events), "fix it"],
                               env={"CODEX_HOME": home})

        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.hook_calls()
        self.assertEqual([args for args, _ in calls], [[name.split("-")[0]] for name in names])
        with open(self.calls) as f:
            stdins = [json.loads(line)["stdin"] for line in f]
        for name, stdin in zip(names, stdins):
            self.assertNotIn("{{", stdin, name)
        [payload] = [p for args, p in calls if args == ["UserPromptSubmit"]]
        self.assertEqual(payload["prompt"], "fix it")
        self.assertTrue(payload["transcript_path"].startswith(home + "/sessions/"), payload["transcript_path"])

    def test_hooks_get_the_fakes_own_environment_never_the_policy_set(self):
        policy = ["-c", 'shell_environment_policy.set.GROVE_SESSION_ID="from-policy"',
                  "-c", 'shell_environment_policy.set.EXTRA="from-policy"']
        args = ["--no-daemon", *self.hooks(["Stop"]), *policy]

        with_id = self.run_fake([{"hook": "Stop"}], args, env={"GROVE_SESSION_ID": "from-env"})
        without_id = self.run_fake([{"hook": "Stop"}], args)

        self.assertEqual((with_id.returncode, without_id.returncode), (0, 0), with_id.stderr + without_id.stderr)
        with open(self.calls) as f:
            envs = [json.loads(line)["env"] for line in f]
        self.assertEqual([env.get("GROVE_SESSION_ID") for env in envs], ["from-env", None])
        self.assertEqual([env.get("EXTRA") for env in envs], [None, None])

    def test_runs_each_hook_through_a_login_shell_from_shell(self):
        shell_log = os.path.join(self.dir, "shell.log")
        shell = self.write("shell", f'#!/bin/sh\necho "$@" >> {shlex.quote(shell_log)}\nexec /bin/sh "$@"\n')
        os.chmod(shell, 0o755)
        command = "echo hi"
        args = ["-c", f"hooks.Stop={toml([{'hooks': [{'type': 'command', 'command': command}]}])}"]

        result = self.run_fake([{"hook": "Stop"}], args, env={"SHELL": shell})

        self.assertEqual(result.returncode, 0, result.stderr)
        with open(shell_log) as f:
            self.assertEqual(f.read(), f"-lc {command}\n")

    def test_accepts_the_flags_grove_passes_on_launch_and_on_resume(self):
        flags = [*self.hooks(["SessionStart"]),
                 "-c", 'mcp_servers.grove={command="grove-mcp", env_vars=["GROVE_SESSION_ID"]}',
                 "-c", 'shell_environment_policy.set.GROVE_SESSION_ID="grove-1"']
        model = ["-m", "gpt-5.5-mini", "-c", 'model_reasoning_effort="low"']
        launches = [["--no-daemon", *flags, "fix it"], ["resume", SESSION, "--no-daemon", *flags],
                    ["resume", SESSION, *flags, "--no-daemon", "go on"],
                    ["resume", SESSION, "--no-daemon", *flags, *model]]

        for args in launches:
            result = self.run_fake([{"hook": "SessionStart-resume"}], args)
            self.assertEqual(result.returncode, 0, result.stderr)

        ids = [p["session_id"] for _, p in self.hook_calls()]
        uuid.UUID(ids[0])
        self.assertEqual(ids[1:], [SESSION, SESSION, SESSION])

    def test_fails_loudly_on_any_other_argv(self):
        def c(text):
            return ["-c", text]

        bad = {
            "unknown flag": ["--no-daemon", "--yolo"],
            "bypass hook trust": ["--dangerously-bypass-hook-trust"],
            "long config flag": ["--config", 'hooks.Stop=[]'],
            "-c without a value": ["-c"],
            "-c without =": c("hooks"),
            "other config key": c('model="o3"'),
            "hook approval state": c('hooks.state."/<session-flags>/config.toml:stop:0:0".trusted_hash="sha256:0"'),
            "unknown hook event": c('hooks.Notification=[{hooks=[{type="command", command="x"}]}]'),
            "hooks not a list": c('hooks.Stop="grove-hook codex"'),
            "hook not a command": c('hooks.Stop=[{hooks=[{type="prompt", command="x"}]}]'),
            "hook with another field": c('hooks.Stop=[{hooks=[{type="command", command="x", async=true}]}]'),
            "hook timeout not a number": c('hooks.Stop=[{hooks=[{type="command", command="x", timeout="1"}]}]'),
            "group with another field": c('hooks.Stop=[{matcher="*", hooks=[], when="now"}]'),
            "matcher other than *": c('hooks.PreToolUse=[{matcher="Bash", hooks=[]}]'),
            "other mcp server": c('mcp_servers.other={command="x"}'),
            "mcp server without command": c('mcp_servers.grove={env_vars=["GROVE_SESSION_ID"]}'),
            "mcp server with env": c('mcp_servers.grove={command="x", env={GROVE_SESSION_ID="1"}}'),
            "policy other than set": c('shell_environment_policy.inherit="core"'),
            "policy value not a string": c("shell_environment_policy.set.GROVE_SESSION_ID=1"),
            "no-daemon twice": ["--no-daemon", "--no-daemon"],
            "model twice": ["-m", "o3", "-m", "o4"],
            "model on the app server": ["app-server", "-m", "o3"],
            "effort not a string": c("model_reasoning_effort=1"),
            "two prompts": ["one", "two"],
            "resume not first": ["--no-daemon", "resume", SESSION],
            "resume without an id": ["resume"],
            "resume id not a uuid": ["resume", "last"],
            "resume with two prompts": ["resume", SESSION, "one", "two"],
            "app-server with no-daemon": ["app-server", "--no-daemon"],
            "app-server with a prompt": ["app-server", "hi"],
        }
        for name, args in bad.items():
            with self.subTest(name):
                result = self.run_fake([{"hook": "SessionStart-startup"}], [*args, *self.hooks(["SessionStart"])])
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn(b"fake codex:", result.stderr)
        self.assertEqual(self.hook_calls(), [])
        self.assertEqual(len([r for r in self.log_records() if "error" in r]), len(bad))

    def test_logs_the_config_the_c_overrides_add_up_to(self):
        args = ["-c", "hooks.Stop=[]",
                "-c", r"""hooks.Stop = [ { matcher = '*', hooks = [
                          {type = "command", command = "say \"hi\" \u00e9", timeout = 1_0} ] } ]""",
                "-c", 'mcp_servers.grove.command="grove-mcp"',
                "-c", 'mcp_servers.grove.env_vars=["GROVE_SESSION_ID",]',
                "-c", "shell_environment_policy.set.GROVE_SESSION_ID=not toml",
                "-c", r"""shell_environment_policy.set."A.B"='C:\x'"""]

        result = self.run_fake([], args)

        self.assertEqual(result.returncode, 0, result.stderr)
        [config] = [r["config"] for r in self.log_records() if "config" in r]
        self.assertEqual(config, {
            "hooks": {"Stop": [{"matcher": "*",
                                "hooks": [{"type": "command", "command": 'say "hi" \u00e9', "timeout": 10}]}]},
            "mcp_servers": {"grove": {"command": "grove-mcp", "env_vars": ["GROVE_SESSION_ID"]}},
            "shell_environment_policy": {"set": {"GROVE_SESSION_ID": "not toml", "A.B": "C:\\x"}},
        })

    def test_unapproved_hooks_show_hooks_need_review_and_never_run(self):
        steps = [{"print": "after"}, {"hook": "SessionStart-startup"}, {"hook": "Stop"}]
        for trust in ("untrusted", "modified"):
            with self.subTest(trust):
                result = self.run_fake(steps, env={"FAKE_CODEX_HOOK_TRUST": trust})

                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn(b"Hooks need review", result.stdout)
                self.assertTrue(result.stdout.endswith(b"after"))
                self.assertEqual(self.hook_calls(), [])
                skipped = [(r["hook_skipped"], r["trust"]) for r in self.log_records() if "hook_skipped" in r]
                self.assertEqual(skipped, [("SessionStart", trust), ("Stop", trust)])
                os.remove(self.log)

    def test_trusted_hooks_run_with_no_review_and_other_trust_values_fail(self):
        quiet = self.run_fake([{"hook": "Stop"}], env={"FAKE_CODEX_HOOK_TRUST": "trusted"})
        no_hooks = self.run_fake([{"print": "hi"}], ["--no-daemon"], env={"FAKE_CODEX_HOOK_TRUST": "untrusted"})
        bad = self.run_fake([{"hook": "Stop"}], env={"FAKE_CODEX_HOOK_TRUST": "maybe"})

        self.assertEqual((quiet.returncode, quiet.stdout), (0, b""), quiet.stderr)
        self.assertEqual(len(self.hook_calls()), 1)
        self.assertEqual((no_hooks.returncode, no_hooks.stdout), (0, b"hi"))
        self.assertEqual(bad.returncode, 2)
        self.assertIn(b"FAKE_CODEX_HOOK_TRUST", bad.stderr)

    def app_server(self, args, requests, env=None):
        """Runs the fake app-server with requests on stdin. Returns its exit
        code and every message it wrote."""
        lines = b"".join(json.dumps(r).encode() + b"\n" for r in requests)
        result = self.run_fake([], args, stdin=lines, env=env)
        return result.returncode, [json.loads(line) for line in result.stdout.splitlines()]

    def test_app_server_answers_hooks_list_like_the_real_cli(self):
        for trust in ("untrusted", "trusted", "modified"):
            with self.subTest(trust):
                with open(os.path.join(DATA, "app-server", f"hooks-list-{trust}.json")) as f:
                    recorded = json.load(f)
                requests = [m["client"] for m in recorded["messages"] if "client" in m]
                want = [m["server"] for m in recorded["messages"] if "server" in m]
                home = os.path.join(self.dir, "codex-home")

                code, got = self.app_server(recorded["args"], requests,
                                            env={"FAKE_CODEX_HOOK_TRUST": trust, "CODEX_HOME": home})

                self.assertEqual(code, 0)
                self.assertEqual([m.get("id", m.get("method")) for m in got],
                                 [m.get("id", m.get("method")) for m in want])
                self.assertEqual(got[0]["result"]["codexHome"], home)
                self.assertEqual(set(got[0]["result"]), set(want[0]["result"]))
                hooks = got[-1]["result"]["data"][0]["hooks"]
                for hook in hooks:
                    self.assertRegex(hook.pop("currentHash"), r"^sha256:[0-9a-f]{64}$")
                for hook in want[-1]["result"]["data"][0]["hooks"]:
                    del hook["currentHash"]
                want[-1]["result"]["data"][0]["cwd"] = self.cwd
                self.assertEqual(got[-1], want[-1])

    def test_app_server_fills_in_timeouts_and_lists_each_cwd(self):
        no_timeout = {"hooks": [{"type": "command", "command": "grove-hook codex"}]}
        args = ["app-server", *[f for ev in ("Stop", "SessionEnd", "Interrupt")
                                for f in ("-c", f"hooks.{ev}={toml([no_timeout])}")]]
        requests = [{"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "grove", "version": "0"}}},
                    {"method": "initialized"},
                    {"id": 2, "method": "hooks/list", "params": {"cwds": ["/a", "/b"]}}]

        code, got = self.app_server(args, requests)

        self.assertEqual(code, 0)
        data = got[-1]["result"]["data"]
        self.assertEqual([entry["cwd"] for entry in data], ["/a", "/b"])
        self.assertEqual([(h["eventName"], h["timeoutSec"], h["matcher"]) for h in data[0]["hooks"]],
                         [("sessionEnd", 1, None), ("stop", 600, None), ("interrupt", 1, None)])

    def test_app_server_errors_like_the_real_cli(self):
        init = {"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "grove", "version": "0"}}}
        requests = [{"id": 0, "method": "hooks/list", "params": {}}, init, {"method": "initialized"},
                    {"id": 2, "method": "no/such", "params": {}}, dict(init, id=3)]

        code, got = self.app_server(["app-server", *self.hooks()], requests)

        self.assertEqual(code, 0)
        errors = {m["id"]: m["error"] for m in got if "error" in m}
        self.assertEqual(errors[0], {"code": -32600, "message": "Not initialized"})
        self.assertEqual(errors[2]["code"], -32600)
        self.assertTrue(errors[2]["message"].startswith("Invalid request: unknown variant `no/such`"))
        self.assertEqual(errors[3], {"code": -32600, "message": "Already initialized"})

    def test_app_server_needs_no_client_info_and_fails_loudly_on_a_line_that_is_not_an_object(self):
        code, got = self.app_server(["app-server"], [{"id": 1, "method": "initialize"},
                                                     {"id": 2, "method": "hooks/list"}])
        bad = self.run_fake([], ["app-server"], stdin=b"[1]\n")

        self.assertEqual(code, 0)
        self.assertEqual(got[-1]["result"]["data"][0]["hooks"], [])
        self.assertEqual(bad.returncode, 1)
        self.assertIn(b"fake codex: app-server: not a JSON object", bad.stderr)

    def chat(self, steps, args=("app-server",), env=None):
        """Starts the fake app-server playing steps, for a test to talk to."""
        full = dict({k: v for k, v in os.environ.items() if not k.startswith(("FAKE_CODEX_", "GROVE_", "CODEX_"))},
                    HOME=self.dir, FAKE_CODEX_APP_SERVER_SCENARIO=self.write("app-server.json", {"steps": steps}),
                    FAKE_CODEX_LOG=self.log)
        full.update(env or {})
        server = AppServer([FAKE, *args], self.cwd, full)
        self.addCleanup(server.close)
        return server

    def test_app_server_starts_a_thread_and_streams_a_turn_the_scenario_plays(self):
        steps = [{"wait_message": "turn/start"},
                 {"send": {"method": "turn/started", "params": {"threadId": "{{thread_id}}",
                                                                "turn": {"id": "{{turn_id}}", "status": "inProgress"}}}},
                 {"send": {"method": "item/agentMessage/delta",
                           "params": {"threadId": "{{thread_id}}", "turnId": "{{turn_id}}", "itemId": "m1",
                                      "delta": "Hi"}}},
                 {"send": {"method": "turn/completed", "params": {"threadId": "{{thread_id}}",
                                                                  "turn": {"id": "{{turn_id}}", "status": "completed"}}}}]
        account = {"type": "chatgpt", "email": "ann@example.com", "planType": "plus"}
        server = self.chat(steps, env={"FAKE_CODEX_ACCOUNT": json.dumps(account)})

        server.handshake()
        read = server.request("account/read", {})
        thread = server.request("thread/start", {})["result"]["thread"]
        turn = server.request("turn/start", {"threadId": thread["id"],
                                             "input": [{"type": "text", "text": "hi", "text_elements": []}]})
        notes = [server.read() for _ in range(3)]
        code = server.close()

        self.assertEqual(read["result"], {"account": account, "requiresOpenaiAuth": True})
        uuid.UUID(thread["id"])
        self.assertEqual(thread["cwd"], self.cwd)
        turn_id = turn["result"]["turn"]["id"]
        self.assertEqual([n["method"] for n in notes], ["turn/started", "item/agentMessage/delta", "turn/completed"])
        self.assertEqual({n["params"]["threadId"] for n in notes}, {thread["id"]})
        self.assertEqual(notes[0]["params"]["turn"]["id"], turn_id)
        self.assertEqual(code, 0)

    def test_app_server_resumes_the_thread_it_is_given_and_runs_one_thread(self):
        server = self.chat([])
        server.handshake()

        resumed = server.request("thread/resume", {"threadId": SESSION})
        again = server.request("thread/start", {})
        server.close()
        bad = self.chat([])
        bad.handshake()
        not_uuid = bad.request("thread/resume", {"threadId": "nope"})

        self.assertEqual(resumed["result"]["thread"]["id"], SESSION)
        self.assertIn("one thread", again["error"]["message"])
        self.assertIn("not a UUID", not_uuid["error"]["message"])

    def test_app_server_records_turns_and_a_resumed_thread_returns_them(self):
        record = os.path.join(self.dir, "record")
        os.mkdir(record)
        turn = {"id": "t1", "status": "completed", "error": None,
                "items": [{"type": "userMessage", "id": "u1", "content": [{"type": "text", "text": "{{text}}"}]}]}
        steps = [{"wait_message": "turn/start"}, {"record": turn}]
        env = {"FAKE_CODEX_RECORD": record}
        server = self.chat(steps, env=env)
        server.handshake()
        thread = server.request("thread/start", {})["result"]["thread"]
        server.request("turn/start", {"threadId": thread["id"],
                                      "input": [{"type": "text", "text": "hi", "text_elements": []}]})
        self.assertEqual(server.close(), 0)

        again = self.chat([], env=env)
        again.handshake()
        resumed = again.request("thread/resume", {"threadId": thread["id"]})["result"]["thread"]
        fresh = self.chat([], env=env)
        fresh.handshake()
        started = fresh.request("thread/start", {})["result"]["thread"]

        self.assertEqual(thread["turns"], [])
        self.assertEqual(resumed["turns"], [dict(turn, items=[dict(turn["items"][0], content=[
            {"type": "text", "text": "hi"}])])])
        self.assertEqual(started["turns"], [])

    def test_the_tui_records_into_its_sessions_record(self):
        record = os.path.join(self.dir, "record")
        os.mkdir(record)
        steps = [{"wait_input": "\r"}, {"record": {"id": "t1", "said": "{{text}}"}}]

        result = self.run_fake(steps, ["resume", SESSION, "--no-daemon"], stdin=b"hello\r",
                               env={"FAKE_CODEX_RECORD": record})

        self.assertEqual(result.returncode, 0, result.stderr)
        with open(os.path.join(record, f"{SESSION}.jsonl")) as f:
            self.assertEqual([json.loads(line) for line in f], [{"id": "t1", "said": "hello"}])

    def test_app_server_takes_one_turn_at_a_time_on_its_thread_and_interrupts_only_that_turn(self):
        steps = [{"wait_message": "turn/interrupt"},
                 {"send": {"method": "turn/completed", "params": {"threadId": "{{thread_id}}",
                                                                  "turn": {"id": "{{turn_id}}", "status": "interrupted"}}}}]
        server = self.chat(steps)
        server.handshake()
        thread = server.request("thread/start", {})["result"]["thread"]["id"]
        text = [{"type": "text", "text": "hi", "text_elements": []}]

        wrong_thread = server.request("turn/start", {"threadId": SESSION, "input": text})
        no_turn = server.request("turn/interrupt", {"threadId": thread, "turnId": "t"})
        turn = server.request("turn/start", {"threadId": thread, "input": text})["result"]["turn"]["id"]
        busy = server.request("turn/start", {"threadId": thread, "input": text})
        wrong_turn = server.request("turn/interrupt", {"threadId": thread, "turnId": "t"})
        interrupted = server.request("turn/interrupt", {"threadId": thread, "turnId": turn})
        completed = server.read()
        after = server.request("turn/start", {"threadId": thread, "input": text})

        self.assertIn("no thread", wrong_thread["error"]["message"])
        self.assertIn("no turn", no_turn["error"]["message"])
        self.assertIn("one turn", busy["error"]["message"])
        self.assertIn("no turn", wrong_turn["error"]["message"])
        self.assertEqual(interrupted["result"], {})
        self.assertEqual((completed["method"], completed["params"]["turn"]["status"]), ("turn/completed", "interrupted"))
        self.assertIn("result", after)

    def test_app_server_forks_the_thread_and_runs_the_forks_turn_beside_the_threads_own(self):
        steps = [{"wait_message": "thread/fork"}, {"wait_message": "turn/start"},
                 {"send": {"method": "item/agentMessage/delta",
                           "params": {"threadId": "{{fork_thread_id}}", "turnId": "{{fork_turn_id}}", "itemId": "m1",
                                      "delta": "{{fork_text}} / {{text}}"}}},
                 {"wait_message": "turn/interrupt"},
                 {"send": {"method": "turn/completed", "params": {"threadId": "{{fork_thread_id}}",
                                                                  "turn": {"id": "{{fork_turn_id}}", "status": "interrupted"}}}}]
        server = self.chat(steps)
        server.handshake()
        text = [{"type": "text", "text": "main", "text_elements": []}]
        aside = [{"type": "text", "text": "aside", "text_elements": []}]

        no_thread = server.request("thread/fork", {"threadId": SESSION})
        thread = server.request("thread/start", {})["result"]["thread"]["id"]
        own = server.request("turn/start", {"threadId": thread, "input": text})["result"]["turn"]["id"]
        forked = server.request("thread/fork", {"threadId": thread, "ephemeral": True, "approvalPolicy": "never"})
        fork = forked["result"]["thread"]
        started = server.read()
        turn = server.request("turn/start", {"threadId": fork["id"], "input": aside})["result"]["turn"]["id"]
        delta = server.read()
        busy = server.request("turn/start", {"threadId": fork["id"], "input": aside})
        wrong_turn = server.request("turn/interrupt", {"threadId": fork["id"], "turnId": own})
        interrupted = server.request("turn/interrupt", {"threadId": fork["id"], "turnId": turn})
        completed = server.read()
        again = server.request("turn/start", {"threadId": fork["id"], "input": aside})
        # The thread's own turn ran through all of it.
        own_busy = server.request("turn/start", {"threadId": thread, "input": text})
        unsubscribed = server.request("thread/unsubscribe", {"threadId": fork["id"]})
        closed = server.read()
        gone = server.request("thread/unsubscribe", {"threadId": fork["id"]})
        no_fork = server.request("turn/start", {"threadId": fork["id"], "input": aside})

        self.assertIn("no thread", no_thread["error"]["message"])
        uuid.UUID(fork["id"])
        self.assertNotEqual(fork["id"], thread)
        self.assertEqual((fork["forkedFromId"], fork["ephemeral"], fork["turns"]), (thread, True, []))
        self.assertEqual(forked["result"]["approvalPolicy"], "never")
        self.assertEqual((started["method"], started["params"]["thread"]["id"]), ("thread/started", fork["id"]))
        self.assertNotEqual(turn, own)
        self.assertIn("one turn", busy["error"]["message"])
        self.assertEqual(delta["params"], {"threadId": fork["id"], "turnId": turn, "itemId": "m1",
                                           "delta": "aside / main"})
        self.assertIn("no turn", wrong_turn["error"]["message"])
        self.assertEqual(interrupted["result"], {})
        self.assertEqual((completed["method"], completed["params"]["threadId"]), ("turn/completed", fork["id"]))
        self.assertIn("result", again)
        self.assertIn("one turn", own_busy["error"]["message"])
        self.assertEqual(unsubscribed["result"], {"status": "unsubscribed"})
        self.assertEqual(closed, {"method": "thread/closed", "params": {"threadId": fork["id"]}})
        self.assertEqual(gone["result"], {"status": "notLoaded"})
        self.assertIn("no thread", no_fork["error"]["message"])

    def test_app_server_lists_its_skills_for_each_folder_asked_or_its_own(self):
        server = self.chat([])
        server.handshake()

        own = server.request("skills/list", {})["result"]["data"]
        asked = server.request("skills/list", {"cwds": ["/a", "/b"]})["result"]["data"]

        self.assertEqual([entry["cwd"] for entry in own], [os.path.realpath(self.cwd)])
        self.assertEqual([entry["cwd"] for entry in asked], ["/a", "/b"])
        self.assertEqual([(s["name"], s["enabled"]) for s in asked[0]["skills"]],
                         [("review", True), ("off", False)])

    def test_app_server_reads_its_skills_from_the_file_at_each_request(self):
        path = os.path.join(self.dir, "skills.json")
        server = self.chat([], env={"FAKE_CODEX_SKILLS": path})
        server.handshake()

        before = server.request("skills/list", {})["result"]["data"][0]["skills"]
        with open(path, "w") as f:
            json.dump([{"name": "plan", "description": "Plan it", "enabled": True, "path": "/p", "scope": "repo"}], f)
        after = server.request("skills/list", {})["result"]["data"][0]["skills"]

        self.assertEqual([s["name"] for s in before], ["review", "off"])
        self.assertEqual([s["name"] for s in after], ["plan"])

    def test_app_server_lists_its_models_and_runs_each_turn_with_the_model_and_effort_it_was_given(self):
        says = {"send": {"method": "item/agentMessage/delta",
                         "params": {"threadId": "{{thread_id}}", "turnId": "{{turn_id}}", "itemId": "m1",
                                    "delta": "{{model}} {{effort}}"}}}
        done = {"send": {"method": "turn/completed", "params": {"threadId": "{{thread_id}}",
                                                                "turn": {"id": "{{turn_id}}", "status": "completed"}}}}
        server = self.chat([{"wait_message": "turn/start"}, says, done] * 3)
        server.handshake()
        text = [{"type": "text", "text": "hi", "text_elements": []}]

        listed = server.request("model/list", {})["result"]
        thread = server.request("thread/start", {})["result"]["thread"]["id"]
        said = []
        for chosen in ({}, {"model": "gpt-5.5-mini", "effort": "low"}, {"model": "gpt-5.5"}):
            server.request("turn/start", {"threadId": thread, "input": text, **chosen})
            said.append(server.read()["params"]["delta"])
            server.read()
        unknown = server.request("turn/start", {"threadId": thread, "input": text, "model": "gpt-4"})
        too_high = server.request("turn/start", {"threadId": thread, "input": text, "model": "gpt-5.5-mini",
                                                 "effort": "high"})

        self.assertEqual([(m["model"], m["isDefault"], m["defaultReasoningEffort"]) for m in listed["data"]],
                         [("gpt-5.5", True, "medium"), ("gpt-5.5-mini", False, "low")])
        self.assertEqual([e["reasoningEffort"] for e in listed["data"][1]["supportedReasoningEfforts"]],
                         ["low", "medium"])
        self.assertIsNone(listed["nextCursor"])
        # The thread starts on gpt-5.5 at medium, and each choice keeps to later turns, as in Codex.
        self.assertEqual(said, ["gpt-5.5 medium", "gpt-5.5-mini low", "gpt-5.5 low"])
        self.assertIn("gpt-4", unknown["error"]["message"])
        self.assertIn("high", too_high["error"]["message"])

    def test_app_server_sends_a_server_request_and_waits_for_the_clients_reply(self):
        steps = [{"wait_message": "turn/start"},
                 {"send": {"id": 0, "method": "item/commandExecution/requestApproval",
                           "params": {"threadId": "{{thread_id}}", "turnId": "{{turn_id}}", "itemId": "call_1"}}},
                 {"wait_message": "response"},
                 {"send": {"method": "turn/completed", "params": {"threadId": "{{thread_id}}",
                                                                  "turn": {"id": "{{turn_id}}", "status": "completed"}}}}]
        server = self.chat(steps)
        server.handshake()
        thread = server.request("thread/start", {})["result"]["thread"]["id"]
        server.request("turn/start", {"threadId": thread, "input": [{"type": "text", "text": "hi", "text_elements": []}]})

        asked = server.read()
        server.write({"id": 0, "result": {"decision": "accept"}})
        completed = server.read()
        server.close()

        self.assertEqual((asked["id"], asked["params"]["threadId"]), (0, thread))
        self.assertEqual(completed["method"], "turn/completed")
        sent = [json.loads(r["stdin"]) for r in self.log_records() if "stdin" in r]
        self.assertIn({"id": 0, "result": {"decision": "accept"}}, sent)

    def test_app_server_rejects_turn_input_that_is_not_text_and_scenario_steps_it_cannot_play(self):
        server = self.chat([])
        server.handshake()
        thread = server.request("thread/start", {})["result"]["thread"]["id"]
        no_elements = server.request("turn/start", {"threadId": thread, "input": [{"type": "text", "text": "hi"}]})
        server.close()
        # A TUI scenario is fine, as the app server never reads it.
        tui = self.run_fake([{"hook": "Stop"}], ["app-server"])
        bad = self.run_fake([], ["app-server"], env={"FAKE_CODEX_APP_SERVER_SCENARIO": self.write(
            "app-server.json", {"steps": [{"hook": "Stop"}]})})

        self.assertIn("text_elements", no_elements["error"]["message"])
        self.assertEqual(tui.returncode, 0, tui.stderr)
        self.assertEqual(bad.returncode, 2)
        self.assertIn(b"scenario step 1", bad.stderr)

    def test_app_server_exits_1_with_the_error_when_answering_a_request_fails(self):
        server = self.chat([{"wait_message": "turn/start"}], env={"FAKE_CODEX_ACCOUNT": "{not json"})
        server.handshake()
        server.write({"id": 7, "method": "account/read", "params": {}})

        code = server.proc.wait(timeout=10)
        stderr = server.proc.stderr.read()
        server.close()

        self.assertEqual(code, 1)
        self.assertIn(b"fake codex: app-server: JSONDecodeError", stderr)

    def test_plays_the_fake_claudes_scenario_steps_and_writes_its_log_format(self):
        command = "echo out; echo err >&2; exit 3"
        args = ["--no-daemon", "-c", f"hooks.Stop={toml([{'hooks': [{'type': 'command', 'command': command}]}])}"]
        steps = [{"screen": "idle-prompt"}, {"mode": "bracketed_paste", "on": True}, {"wait_input": "\r"},
                 {"hook": "Stop"}, {"exit": 3}, {"print": "never"}]

        result = self.run_fake(steps, args, stdin=b"hi\xff\r")

        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn(self.cwd.encode(), result.stdout)
        self.assertNotIn(b"{{", result.stdout)
        self.assertTrue(result.stdout.endswith(b"\x1b[?2004h"))
        records = self.log_records()
        self.assertEqual((records[0]["argv"], records[0]["cwd"]), ([FAKE, *args], self.cwd))
        self.assertEqual(records[0]["env"]["FAKE_CODEX_LOG"], self.log)
        self.assertEqual("".join(r["stdin"] for r in records if "stdin" in r).encode("latin-1"), b"hi\xff\r")
        [run] = [r for r in records if "hook" in r]
        self.assertEqual({k: run[k] for k in ("hook", "command", "code", "stdout", "stderr")},
                         {"hook": "Stop", "command": command, "code": 3, "stdout": "out\n", "stderr": "err\n"})
        self.assertEqual(records[-1]["exit"], 3)

    def test_rejects_a_bad_scenario_and_needs_one_outside_app_server(self):
        bad = self.run_fake([{"hook": "Stop"}, {"hook": "Notification"}])
        env = {k: v for k, v in os.environ.items() if not k.startswith("FAKE_CODEX_")}
        missing = subprocess.run([FAKE], capture_output=True, env=env, timeout=30)

        self.assertEqual((bad.returncode, missing.returncode), (2, 2))
        self.assertIn(b"fake codex: scenario step 2", bad.stderr)
        self.assertIn(b"FAKE_CODEX_SCENARIO", missing.stderr)
        self.assertEqual(self.hook_calls(), [])

    def test_codex_home_defaults_to_dot_codex_in_home(self):
        result = self.run_fake([{"hook": "Stop"}])

        self.assertEqual(result.returncode, 0, result.stderr)
        [(_, payload)] = self.hook_calls()
        self.assertTrue(payload["transcript_path"].startswith(self.dir + "/.codex/sessions/"))

    def test_an_mcp_call_passes_the_variables_env_vars_names_from_the_fakes_environment(self):
        server = toml({"command": sys.executable, "args": [os.path.join(HERE, "mcp_stub.py")],
                       "env_vars": ["GROVE_SESSION_ID", "GROVE_SESSION_TOKEN"]})
        args = ["--no-daemon", *self.hooks(), "-c", f"mcp_servers.grove={server}"]

        result = self.run_fake([{"mcp_call": "grove", "tool": "subagent_spawn", "arguments": {"agent": "codex"}}],
                               args=args, env={"GROVE_SESSION_ID": "3", "GROVE_SESSION_TOKEN": "t",
                                               "GROVE_OTHER": "kept out"})

        self.assertEqual(result.returncode, 0, result.stderr)
        [call] = [r for r in self.log_records() if "mcp_call" in r]
        reply = json.loads(call["reply"]["result"]["content"][0]["text"])
        self.assertEqual(reply["params"], {"name": "subagent_spawn", "arguments": {"agent": "codex"}})
        self.assertEqual((reply["env"]["GROVE_SESSION_ID"], reply["env"]["GROVE_SESSION_TOKEN"]), ("3", "t"))
        self.assertNotIn("GROVE_OTHER", reply["env"])

    def test_a_shell_step_gets_the_fakes_environment_plus_the_policy_set_and_logs_the_output(self):
        # 0.158.0 passes its whole environment to shell commands by default, then adds set.
        policy = ["-c", 'shell_environment_policy.set.GROVE_SESSION_ID="from-policy"']
        command = 'echo "$GROVE_SESSION_ID $GROVE_SESSION_TOKEN"; pwd; echo oops >&2; exit 3'

        result = self.run_fake([{"shell": command}], ["--no-daemon", *policy],
                               env={"GROVE_SESSION_ID": "from-env", "GROVE_SESSION_TOKEN": "t"})

        self.assertEqual(result.returncode, 0, result.stderr)
        [run] = [r for r in self.log_records() if "shell" in r]
        self.assertEqual((run["shell"], run["code"]), (command, 3))
        self.assertEqual((run["stdout"], run["stderr"]), (f"from-policy t\n{self.cwd}\n", "oops\n"))

    def test_runs_a_shell_step_through_a_login_shell_from_shell(self):
        shell_log = os.path.join(self.dir, "shell.log")
        shell = self.write("shell", f'#!/bin/sh\necho "$@" >> {shlex.quote(shell_log)}\nexec /bin/sh "$@"\n')
        os.chmod(shell, 0o755)

        result = self.run_fake([{"shell": "echo hi"}], ["--no-daemon"], env={"SHELL": shell})

        self.assertEqual(result.returncode, 0, result.stderr)
        with open(shell_log) as f:
            self.assertEqual(f.read(), "-lc echo hi\n")


if __name__ == "__main__":
    unittest.main()
