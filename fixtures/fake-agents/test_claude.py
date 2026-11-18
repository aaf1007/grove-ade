# Tests for the fake Claude Code agent. Run: python3 -m unittest discover fixtures/fake-agents
import json
import os
import pty
import select
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FAKE = os.path.join(HERE, "claude")
PAYLOADS = os.path.join(HERE, "claude-code", "payloads")
# The events issue #17 asks for.
ISSUE_17_PAYLOADS = ["SessionStart-startup", "SessionStart-resume", "SessionStart-clear", "UserPromptSubmit",
                     "PreToolUse-Bash", "PostToolUse-Bash", "PermissionRequest-Bash", "PreToolUse-AskUserQuestion",
                     "Stop", "StopFailure", "SubagentStop", "SessionEnd-prompt_input_exit"]
SESSION = "6f1c2a9e-0b7d-4c3e-9a51-2d8e4f6b7c10"
# Writes one JSON line per call: the hook's extra args and its stdin.
STUB = 'import json,sys; open(sys.argv[1],"a").write(json.dumps({"args":sys.argv[2:],"stdin":sys.stdin.read()})+"\\n")'


class FakeClaudeTest(unittest.TestCase):
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

    def settings(self, hooks=None):
        """Settings file whose hooks record each call, tagged with the event name."""
        if hooks is None:
            hooks = {ev: [{"hooks": [self.hook(ev)]}] for ev in ("SessionStart", "Stop")}
        return self.write("settings.json", {"hooks": hooks})

    def run_fake(self, steps, args=None, stdin=b"", env=None):
        if args is None:
            args = ["--settings", self.settings(), "--session-id", SESSION]
        env = dict(os.environ, FAKE_CLAUDE_SCENARIO=self.write("scenario.json", {"steps": steps}),
                   FAKE_CLAUDE_LOG=self.log, **(env or {}))
        return subprocess.run([FAKE, *args], input=stdin, capture_output=True, cwd=self.cwd,
                              env=env, timeout=30)

    def hook_calls(self):
        if not os.path.exists(self.calls):
            return []
        with open(self.calls) as f:
            calls = [json.loads(line) for line in f]
        return [(c["args"], json.loads(c["stdin"])) for c in calls]

    def log_records(self):
        with open(self.log) as f:
            return [json.loads(line) for line in f]

    def test_fires_a_hook_from_the_settings_file_with_the_recorded_payload(self):
        result = self.run_fake([{"hook": "Stop"}])

        self.assertEqual(result.returncode, 0, result.stderr)
        [(args, payload)] = self.hook_calls()
        self.assertEqual(args, ["Stop"])
        self.assertEqual(payload["hook_event_name"], "Stop")
        self.assertEqual(payload["session_id"], SESSION)
        self.assertEqual(payload["cwd"], self.cwd)

    def test_every_recorded_payload_fires_with_its_placeholders_filled(self):
        names = sorted(f[:-5] for f in os.listdir(PAYLOADS) if f.endswith(".json"))
        self.assertLessEqual(set(ISSUE_17_PAYLOADS), set(names))
        events = {name.split("-")[0] for name in names}
        settings = self.settings({ev: [{"matcher": "*", "hooks": [self.hook(ev)]}] for ev in events})

        result = self.run_fake([{"hook": n} for n in names], ["--settings", settings, "--session-id", SESSION])

        self.assertEqual(result.returncode, 0, result.stderr)
        with open(self.calls) as f:
            stdins = [json.loads(line)["stdin"] for line in f]
        self.assertEqual(len(stdins), len(names))
        for name, stdin in zip(names, stdins):
            self.assertNotIn("{{", stdin, name)
            self.assertEqual(json.loads(stdin)["hook_event_name"], name.split("-")[0])

    def test_matchers_pick_hooks_by_tool_name_and_by_session_start_source(self):
        settings = self.settings({
            "PreToolUse": [{"matcher": "Bash", "hooks": [self.hook("bash only")]},
                           {"matcher": "*", "hooks": [self.hook("any tool")]}],
            "SessionStart": [{"matcher": "clear", "hooks": [self.hook("clear only")]}],
        })
        steps = [{"hook": "PreToolUse-AskUserQuestion"}, {"hook": "PreToolUse-Bash"},
                 {"hook": "SessionStart-startup"}, {"hook": "SessionStart-clear"}]

        result = self.run_fake(steps, ["--settings", settings, "--session-id", SESSION])

        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [(args, p.get("tool_name") or p.get("source")) for args, p in self.hook_calls()]
        self.assertEqual(calls, [(["any tool"], "AskUserQuestion"), (["bash only"], "Bash"),
                                 (["any tool"], "Bash"), (["clear only"], "clear")])

    def test_clear_ends_the_old_session_and_starts_one_with_a_new_id(self):
        new = "0d5a7e3b-9c41-4f2a-8b6d-1e2f3a4b5c6d"
        settings = self.settings({ev: [{"hooks": [self.hook(ev)]}] for ev in ("SessionStart", "SessionEnd")})
        steps = [{"hook": "SessionEnd-clear"}, {"session_id": new}, {"hook": "SessionStart-clear"}]

        result = self.run_fake(steps, ["--settings", settings, "--session-id", SESSION])

        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [(args, p["session_id"], p.get("reason") or p.get("source")) for args, p in self.hook_calls()]
        self.assertEqual(calls, [(["SessionEnd"], SESSION, "clear"), (["SessionStart"], new, "clear")])

    def test_user_prompt_submit_carries_the_argv_prompt_unless_set_overrides_it(self):
        settings = self.settings({"UserPromptSubmit": [{"hooks": [self.hook()]}]})
        steps = [{"hook": "UserPromptSubmit"},
                 {"hook": "UserPromptSubmit", "set": {"prompt": "typed later"}}]

        result = self.run_fake(steps, ["--settings", settings, "--session-id", SESSION, "fix the bug"])

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([p["prompt"] for _, p in self.hook_calls()], ["fix the bug", "typed later"])

    def test_accepts_the_flags_grove_passes_on_launch_and_on_resume(self):
        settings = self.settings()
        mcp = self.write("mcp.json", {"mcpServers": {"grove": {"command": "grove-mcp", "env": {}}}})
        launches = [["--settings", settings, f"--mcp-config={mcp}", "--session-id", SESSION, "fix it"],
                    ["--settings", settings, f"--mcp-config={mcp}", "--resume", SESSION,
                     "--model", "haiku", "--effort", "low"]]

        for args in launches:
            result = self.run_fake([{"hook": "Stop"}], args)
            self.assertEqual(result.returncode, 0, result.stderr)

        self.assertEqual([p["session_id"] for _, p in self.hook_calls()], [SESSION, SESSION])

    def test_records_what_the_user_typed_into_the_conversations_record(self):
        record = os.path.join(self.dir, "record")
        os.mkdir(record)
        steps = [{"wait_input": "\r"}, {"record": {"type": "user", "text": "{{text}}"}},
                 {"wait_input": "\r"}, {"record": {"type": "user", "text": "{{text}}"}}]

        result = self.run_fake(steps, stdin=b"one\rtwo\r", env={"FAKE_CLAUDE_RECORD": record})

        self.assertEqual(result.returncode, 0, result.stderr)
        with open(os.path.join(record, f"{SESSION}.jsonl")) as f:
            self.assertEqual([json.loads(line) for line in f],
                             [{"type": "user", "text": "one"}, {"type": "user", "text": "two"}])

    def test_fails_loudly_on_any_other_argv(self):
        settings = self.settings()
        mcp = self.write("mcp.json", {"mcpServers": {}})
        bad = {
            "unknown flag": ["--settings", settings, "--verbose"],
            "mcp-config with a space": ["--settings", settings, "--mcp-config", mcp, "hi"],
            "settings with =": [f"--settings={settings}"],
            "missing value": ["--settings"],
            "two prompts": ["--settings", settings, "one", "two"],
            "session id not a uuid": ["--settings", settings, "--session-id", "abc"],
            "session id and resume": ["--settings", settings, "--session-id", SESSION, "--resume", SESSION],
            "settings not json": ["--settings", self.write("bad.json", "{nope")],
            "settings hook not a command": ["--settings", self.write("s.json", {"hooks": {"Stop": [{"hooks": [{"type": "http"}]}]}})],
            "mcp config without servers": ["--settings", settings, "--mcp-config=" + self.write("m.json", {})],
            "settings twice": ["--settings", settings, "--settings", settings],
            "model twice": ["--settings", settings, "--model", "haiku", "--model", "opus"],
            "effort without a value": ["--settings", settings, "--effort"],
        }
        for name, args in bad.items():
            with self.subTest(name):
                result = self.run_fake([{"hook": "Stop"}], args)
                self.assertEqual(result.returncode, 2)
                self.assertIn(b"fake claude:", result.stderr)
        self.assertEqual(self.hook_calls(), [])
        self.assertEqual(len([r for r in self.log_records() if "error" in r]), len(bad))

    def test_prints_text_and_recorded_screens_with_the_cwd_filled_in(self):
        result = self.run_fake([{"print": "hello\n"}, {"screen": "trust-dialog"}])

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(result.stdout.startswith(b"hello\n"))
        self.assertIn(b"Accessing", result.stdout)
        self.assertIn(self.cwd.encode(), result.stdout)
        self.assertNotIn(b"{{", result.stdout)

    def test_turns_terminal_modes_on_and_off(self):
        steps = [{"mode": "bracketed_paste", "on": True}, {"mode": "bracketed_paste", "on": False},
                 {"mode": "focus_events", "on": True}]

        result = self.run_fake(steps)

        self.assertEqual(result.stdout, b"\x1b[?2004h\x1b[?2004l\x1b[?1004h")

    def test_sleeps_then_exits_with_the_scenario_code(self):
        start = time.monotonic()
        result = self.run_fake([{"sleep": 0.3}, {"exit": 3}, {"print": "never"}])

        self.assertGreaterEqual(time.monotonic() - start, 0.3)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(result.stdout, b"")

    def test_dies_from_the_signal_even_one_python_handles(self):
        for name in ("INT", "PIPE", "TERM", "KILL"):
            with self.subTest(name):
                result = self.run_fake([{"signal": name}, {"print": "never"}])

                self.assertEqual(result.returncode, -getattr(signal, "SIG" + name))
                self.assertEqual((result.stdout, result.stderr), (b"", b""))
                last = self.log_records()[-1]
                self.assertEqual(last["signal"], name)
                os.remove(self.log)

    def test_stops_with_an_error_if_the_signal_does_not_kill_it(self):
        result = self.run_fake([{"signal": "WINCH"}, {"print": "never"}])

        self.assertEqual((result.returncode, result.stdout), (1, b""))
        self.assertIn(b"still running after SIGWINCH", result.stderr)

    def test_rejects_a_bad_scenario_before_running_any_step(self):
        bad = {
            "unknown step": {"jump": 1},
            "two actions": {"print": "a", "sleep": 1},
            "unknown option": {"print": "a", "on": True},
            "unknown payload": {"hook": "NoSuchEvent"},
            "unknown screen": {"screen": "no-such-screen"},
            "unknown mode": {"mode": "blink", "on": True},
            "mode without on": {"mode": "bracketed_paste"},
            "exit not an int": {"exit": "1"},
            "unknown signal": {"signal": "NOPE"},
        }
        for name, step in bad.items():
            with self.subTest(name):
                result = self.run_fake([{"hook": "Stop"}, step])
                self.assertEqual(result.returncode, 2)
                self.assertIn(b"fake claude: scenario step 2", result.stderr)
        self.assertEqual(self.hook_calls(), [])

    def test_logs_argv_cwd_env_files_and_every_stdin_byte(self):
        settings = self.settings()
        mcp = self.write("mcp.json", {"mcpServers": {"grove": {"command": "grove-mcp"}}})
        args = ["--settings", settings, f"--mcp-config={mcp}", "--session-id", SESSION, "fix it"]
        typed = b"\x1b[200~hi\xff\x1b[201~\r\x03"

        result = self.run_fake([{"wait_input": "\x03"}], args, stdin=typed)

        self.assertEqual(result.returncode, 0, result.stderr)
        records = self.log_records()
        self.assertEqual(records[0]["argv"], [FAKE, *args])
        self.assertEqual(records[0]["cwd"], self.cwd)
        self.assertEqual(records[0]["env"]["FAKE_CLAUDE_LOG"], self.log)
        files = {r["file"]: r["content"] for r in records if "file" in r}
        with open(settings) as s, open(mcp) as m:
            self.assertEqual(files, {settings: s.read(), mcp: m.read()})
        self.assertEqual("".join(r["stdin"] for r in records if "stdin" in r).encode("latin-1"), typed)
        self.assertEqual(records[-1]["exit"], 0)

    def test_logs_each_hook_run_with_its_output_and_exit_code_and_keeps_going(self):
        command = "echo out; echo err >&2; exit 3"
        settings = self.settings({"Stop": [{"hooks": [{"type": "command", "command": command}]}]})

        result = self.run_fake([{"hook": "Stop"}, {"print": "after"}],
                               ["--settings", settings, "--session-id", SESSION])

        self.assertEqual((result.returncode, result.stdout), (0, b"after"))
        [run] = [r for r in self.log_records() if "hook" in r]
        self.assertEqual({k: run[k] for k in ("hook", "command", "code", "stdout", "stderr")},
                         {"hook": "Stop", "command": command, "code": 3, "stdout": "out\n", "stderr": "err\n"})

    def test_wait_input_reads_keys_raw_with_no_echo_on_a_terminal(self):
        master, slave = pty.openpty()
        env = dict(os.environ, FAKE_CLAUDE_LOG=self.log, FAKE_CLAUDE_SCENARIO=self.write(
            "scenario.json", {"steps": [{"print": "ready"}, {"wait_input": "go"}, {"print": "after"}]}))
        proc = subprocess.Popen([FAKE], stdin=slave, stdout=slave, stderr=slave, cwd=self.cwd, env=env)
        os.close(slave)
        self.addCleanup(os.close, master)
        self.addCleanup(proc.kill)

        self.assertIn(b"ready", read_until(master, b"ready"))
        os.write(master, b"g")
        self.assertEqual(read_until(master, b"after", timeout=0.3), b"")
        os.write(master, b"o")
        self.assertIn(b"after", read_until(master, b"after"))
        self.assertEqual(proc.wait(timeout=10), 0)

    def test_stdin_closing_while_waiting_fails_loudly(self):
        result = self.run_fake([{"wait_input": "zzz"}, {"print": "never"}], stdin=b"no match")

        self.assertEqual((result.returncode, result.stdout), (1, b""))
        self.assertIn(b"stdin closed", result.stderr)

    def test_fails_loudly_without_a_scenario(self):
        env = {k: v for k, v in os.environ.items() if k != "FAKE_CLAUDE_SCENARIO"}
        result = subprocess.run([FAKE], capture_output=True, env=env, timeout=30)

        self.assertEqual(result.returncode, 2)
        self.assertIn(b"FAKE_CLAUDE_SCENARIO", result.stderr)

    def test_a_shell_step_runs_in_the_fakes_folder_with_its_environment_and_logs_the_output(self):
        command = 'echo "$GROVE_SESSION_TOKEN {{session_id}}"; pwd; echo oops >&2; exit 3'

        result = self.run_fake([{"shell": command}, {"print": "after"}], env={"GROVE_SESSION_TOKEN": "t"})

        self.assertEqual((result.returncode, result.stdout), (0, b"after"), result.stderr)
        [run] = [r for r in self.log_records() if "shell" in r]
        self.assertEqual(run["shell"], command.replace("{{session_id}}", SESSION))
        self.assertEqual((run["code"], run["stdout"], run["stderr"]), (3, f"t {SESSION}\n{self.cwd}\n", "oops\n"))

    def test_a_shell_step_that_times_out_ends_what_its_command_started(self):
        import fake_agent
        pid_file = os.path.join(self.dir, "sleep.pid")
        command = f"sleep 600 & echo $! > {shlex.quote(pid_file)}; wait"

        fake_agent.run_shell(command, ["/bin/sh", "-c"], dict(os.environ), fake_agent.Log(self.log), timeout=1)

        [run] = [r for r in self.log_records() if "shell" in r]
        self.assertIsNone(run["code"])
        with open(pid_file) as f:
            pid = int(f.read())
        deadline = time.monotonic() + 10
        while alive(pid):
            self.assertLess(time.monotonic(), deadline, f"sleep {pid} still runs")
            time.sleep(0.05)


def alive(pid):
    """Whether pid still runs. A zombie, killed and waiting for whoever adopted it to reap it,
    does not."""
    state = subprocess.run(["ps", "-o", "stat=", "-p", str(pid)], capture_output=True, text=True).stdout
    return bool(state.strip()) and not state.strip().startswith("Z")


def read_until(fd, needle, timeout=10):
    """Reads from fd until needle shows up or timeout passes."""
    out = b""
    deadline = time.monotonic() + timeout
    while needle not in out:
        left = deadline - time.monotonic()
        if left <= 0 or not select.select([fd], [], [], left)[0]:
            break
        try:
            out += os.read(fd, 4096)
        except OSError:
            break
    return out

    def mcp_stub_reply(self):
        """What the stub MCP server answered the one tool call, from the log."""
        [call] = [r for r in self.log_records() if "mcp_call" in r]
        return json.loads(call["reply"]["result"]["content"][0]["text"])

    def test_an_mcp_call_starts_the_server_from_the_mcp_config_with_its_env(self):
        server = {"command": sys.executable, "args": [os.path.join(HERE, "mcp_stub.py")],
                  "env": {"GROVE_SESSION_ID": "3"}}
        mcp = self.write("mcp.json", {"mcpServers": {"grove": server}})
        args = ["--settings", self.settings(), f"--mcp-config={mcp}", "--session-id", SESSION]

        result = self.run_fake([{"mcp_call": "grove", "tool": "subagent_spawn",
                                 "arguments": {"prompt": "{{session_id}}"}}],
                               args=args, env={"GROVE_SESSION_TOKEN": "kept out"})

        self.assertEqual(result.returncode, 0, result.stderr)
        reply = self.mcp_stub_reply()
        self.assertEqual(reply["params"], {"name": "subagent_spawn", "arguments": {"prompt": SESSION}})
        self.assertEqual(reply["env"]["GROVE_SESSION_ID"], "3")
        # Only what the config passes, beyond a few basics such as PATH.
        self.assertNotIn("GROVE_SESSION_TOKEN", reply["env"])
        self.assertIn("PATH", reply["env"])

    def test_an_mcp_call_to_a_server_not_in_the_config_exits_2(self):
        mcp = self.write("mcp.json", {"mcpServers": {}})
        args = ["--settings", self.settings(), f"--mcp-config={mcp}", "--session-id", SESSION]

        result = self.run_fake([{"mcp_call": "grove", "tool": "subagent_spawn"}], args=args)

        self.assertEqual(result.returncode, 2)
        self.assertIn("no MCP server grove", result.stderr.decode())


if __name__ == "__main__":
    unittest.main()
