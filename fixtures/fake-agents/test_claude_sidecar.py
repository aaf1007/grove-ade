# Tests for the fake Claude sidecar. Run: python3 -m unittest discover fixtures/fake-agents
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
FAKE = os.path.join(HERE, "claude-sidecar")
SESSION = "6f1c2a9e-0b7d-4c3e-9a51-2d8e4f6b7c10"


class FakeSidecarTest(unittest.TestCase):
    def setUp(self):
        self.dir = os.path.realpath(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir)
        self.cwd = os.path.join(self.dir, "worktree")
        os.mkdir(self.cwd)
        self.log = os.path.join(self.dir, "fake.log")
        self.settings = self.write("settings.json", {"hooks": {}})
        self.mcp = self.write("mcp.json", {"mcpServers": {}})

    def write(self, name, value):
        path = os.path.join(self.dir, name)
        with open(path, "w") as f:
            f.write(json.dumps(value))
        return path

    def args(self):
        return ["--settings", self.settings, "--mcp-config", self.mcp, "--session-id", SESSION,
                "--claude", "claude"]

    def env(self, steps, extra=None):
        env = {k: v for k, v in os.environ.items() if k != "FAKE_CLAUDE_RECORD"}
        return dict(env, FAKE_CLAUDE_SIDECAR_SCENARIO=self.write("scenario.json", {"steps": steps}),
                    FAKE_CLAUDE_SIDECAR_LOG=self.log, **(extra or {}))

    def run_fake(self, steps, args=None, stdin=b"", env=None):
        return subprocess.run([FAKE, *(self.args() if args is None else args)], input=stdin,
                              capture_output=True, cwd=self.cwd, env=self.env(steps, env), timeout=30)

    def log_records(self):
        with open(self.log) as f:
            return [json.loads(line) for line in f]

    def test_sends_each_message_as_one_json_line_with_placeholders_filled(self):
        steps = [{"send": {"type": "text_delta", "text": "in {{cwd}} for {{session_id}}"}},
                 {"send": {"type": "turn_end"}}]

        result = self.run_fake(steps)

        self.assertEqual(result.returncode, 0, result.stderr)
        lines = result.stdout.decode().split("\n")
        self.assertEqual(lines[-1], "")
        self.assertEqual([json.loads(line) for line in lines[:-1]],
                         [{"type": "text_delta", "text": f"in {self.cwd} for {SESSION}"}, {"type": "turn_end"}])

    def test_wait_message_skips_other_lines_until_one_of_its_type(self):
        steps = [{"wait_message": "user_message"}, {"send": {"type": "turn_start"}}]
        stdin = b'not json\n{"type":"other"}\n{"type":"user_message","text":"hi"}\n'

        result = self.run_fake(steps, stdin=stdin)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b'{"type":"turn_start"}\n')

    def test_wait_message_waits_for_a_whole_line(self):
        steps = [{"wait_message": "user_message"}, {"send": {"type": "turn_start"}}]
        proc = subprocess.Popen([FAKE, *self.args()], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                cwd=self.cwd, env=self.env(steps))
        proc.stdin.write(b'{"type":"user_message"')
        proc.stdin.flush()
        time.sleep(0.2)
        self.assertIsNone(proc.poll())

        proc.stdin.write(b',"text":"hi"}\n')
        proc.stdin.close()

        self.assertEqual(proc.stdout.read(), b'{"type":"turn_start"}\n')
        self.assertEqual(proc.wait(timeout=10), 0)
        proc.stdout.close()

    def test_stdin_closing_while_waiting_exits_1(self):
        result = self.run_fake([{"wait_message": "user_message"}])

        self.assertEqual(result.returncode, 1)
        self.assertIn(b"fake claude-sidecar: stdin closed", result.stderr)

    def test_set_model_takes_effect_from_the_next_turn_start(self):
        says = {"send": {"type": "text_delta", "text": "{{model}} {{effort}}"}}
        turn = [{"wait_message": "user_message"}, {"send": {"type": "turn_start"}}, says]
        proc = subprocess.Popen([FAKE, *self.args()], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                stderr=subprocess.PIPE, cwd=self.cwd, env=self.env(turn * 3))
        self.addCleanup(proc.kill)

        def turn_says(*lines):
            for line in lines:
                proc.stdin.write(json.dumps(line).encode() + b"\n")
            proc.stdin.flush()
            self.assertEqual(json.loads(proc.stdout.readline()), {"type": "turn_start"})
            return json.loads(proc.stdout.readline())["text"]

        first = turn_says({"type": "user_message", "text": "one"})
        second = turn_says({"type": "set_model", "model": "haiku", "effort": "low"},
                           {"type": "user_message", "text": "two"})
        third = turn_says({"type": "set_model", "effort": "max"}, {"type": "user_message", "text": "three"})
        proc.stdin.close()
        code = proc.wait(timeout=10)
        proc.stdout.close()
        proc.stderr.close()

        self.assertEqual([first, second, third], ["default default", "haiku low", "haiku max"])
        self.assertEqual(code, 0)

    def test_logs_argv_cwd_env_and_both_files(self):
        result = self.run_fake([{"exit": 3}])

        self.assertEqual(result.returncode, 3, result.stderr)
        records = self.log_records()
        self.assertEqual(records[0]["argv"], [FAKE, *self.args()])
        self.assertEqual(records[0]["cwd"], self.cwd)
        self.assertEqual([(r["file"], json.loads(r["content"])) for r in records if "file" in r],
                         [(self.settings, {"hooks": {}}), (self.mcp, {"mcpServers": {}})])
        self.assertEqual(records[-1]["exit"], 3)

    def test_takes_exactly_the_flags_grove_passes(self):
        base = self.args()
        bad = [base[:-2], base + ["--claude", "claude"], base + ["--resume", "--resume"],
               ["--settings", self.settings, "--mcp-config", self.mcp, "--session-id", "x", "--claude", "c"],
               base[:-1]]
        for args in bad:
            result = self.run_fake([], args)
            self.assertEqual(result.returncode, 2, args)
            self.assertIn(b"fake claude-sidecar: ", result.stderr)

    def test_a_bad_settings_file_exits_2(self):
        self.write("settings.json", {"hooks": {"Stop": [{"hooks": [{"type": "prompt"}]}]}})

        result = self.run_fake([])

        self.assertEqual(result.returncode, 2)

    def recording(self, lines, name="recording.jsonl"):
        path = os.path.join(self.dir, name)
        with open(path, "w") as f:
            f.writelines(json.dumps(line) + "\n" for line in lines)
        return path

    def test_replay_sends_the_sidecar_lines_in_order_with_placeholders_filled_and_waits_for_the_core_lines(self):
        path = self.recording([
            {"t": 0.0, "from": "sidecar", "message": {"type": "account", "account": None}},
            {"t": 0.1, "from": "core", "message": {"type": "user_message", "text": "go"}},
            {"t": 0.2, "from": "sidecar", "message": {"type": "turn_start"}},
            {"t": 0.3, "from": "sidecar", "message": {"type": "text_delta", "text": "in {{cwd}} at {{home}}"}},
            {"t": 0.4, "from": "sidecar", "message": {"type": "turn_end"}},
        ])
        proc = subprocess.Popen([FAKE, *self.args()], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                cwd=self.cwd, env=self.env([{"replay": path, "speed": 10}]))
        self.assertEqual(json.loads(proc.stdout.readline()), {"type": "account", "account": None})
        time.sleep(0.2)
        self.assertIsNone(proc.poll())

        proc.stdin.write(b'{"type":"user_message","text":"anything"}\n')
        proc.stdin.close()

        rest = [json.loads(line) for line in proc.stdout.read().splitlines()]
        self.assertEqual(rest, [{"type": "turn_start"},
                                {"type": "text_delta", "text": f"in {self.cwd} at {os.environ['HOME']}"},
                                {"type": "turn_end"}])
        self.assertEqual(proc.wait(timeout=10), 0)
        proc.stdout.close()

    def test_replay_keeps_the_recorded_timing_divided_by_the_speed(self):
        lines = [{"t": 0.0, "from": "core", "message": {"type": "user_message", "text": "go"}}]
        lines += [{"t": 0.1 * n, "from": "sidecar", "message": {"type": "text_delta", "text": str(n)}}
                  for n in range(1, 11)]
        path = self.recording(lines)
        for speed, span in ((1, 1.0), (10, 0.1)):
            with self.subTest(speed=speed):
                proc = subprocess.Popen([FAKE, *self.args()], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        cwd=self.cwd, env=self.env([{"replay": path, "speed": speed}]))
                proc.stdin.write(b'{"type":"user_message","text":"go"}\n')
                proc.stdin.close()
                sent = time.monotonic()
                arrivals = []
                for line in proc.stdout:
                    arrivals.append((json.loads(line)["text"], time.monotonic() - sent))
                self.assertEqual(proc.wait(timeout=10), 0)
                proc.stdout.close()
                self.assertEqual([text for text, _ in arrivals], [str(n) for n in range(1, 11)])
                # Measured from the first line, since the fake's start-up time lands before it.
                first = arrivals[0][1]
                for n, (_, at) in enumerate(arrivals, 1):
                    at, due = at - first, span * (n - 1) / 10
                    self.assertGreater(at, due - 0.02, arrivals)
                    self.assertLess(at, due + 0.2, arrivals)

    def test_a_bad_replay_step_or_recording_exits_2_before_any_step_runs(self):
        good = {"t": 0, "from": "sidecar", "message": {"type": "turn_start"}}
        bad_recordings = [
            ["not json"],
            [{"t": 0, "from": "sidecar"}],
            [{"t": 0, "from": "user", "message": {"type": "turn_start"}}],
            [{"t": "0", "from": "sidecar", "message": {"type": "turn_start"}}],
            [{"t": 1, "from": "sidecar", "message": {"type": "turn_start"}}, good],
            [{"t": 0, "from": "sidecar", "message": {"text": "no type"}}],
            [{**good, "extra": 1}],
        ]
        steps = [[{"replay": os.path.join(self.dir, "missing.jsonl")}],
                 [{"replay": self.recording([good]), "speed": 0}],
                 [{"replay": self.recording([good]), "speed": "fast"}],
                 [{"replay": self.recording([good]), "loop": True}]]
        for n, lines in enumerate(bad_recordings):
            path = os.path.join(self.dir, f"bad{n}.jsonl")
            with open(path, "w") as f:
                f.writelines((line if isinstance(line, str) else json.dumps(line)) + "\n" for line in lines)
            steps.append([{"replay": path}])
        for scenario in steps:
            with self.subTest(scenario=scenario):
                result = self.run_fake([{"send": {"type": "turn_start"}}, *scenario])
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertEqual(result.stdout, b"")
                self.assertIn(b"fake claude-sidecar: scenario step 2", result.stderr)

    def test_records_each_turn_and_with_resume_sends_the_record_as_history_first(self):
        record = os.path.join(self.dir, "record")
        os.mkdir(record)
        env = {"FAKE_CLAUDE_RECORD": record}
        turn = [{"wait_message": "user_message"},
                {"record": {"type": "user", "text": "{{text}}"}},
                {"send": {"type": "text_delta", "text": "re: {{text}}"}},
                {"record": {"type": "text_delta", "text": "re: {{text}}"}}]

        first = self.run_fake(turn, stdin=b'{"type":"user_message","text":"one"}\n', env=env)
        resumed = self.run_fake([], self.args() + ["--resume"], env=env)
        other = self.run_fake([], self.args()[:-4] + ["--session-id", "0d5a7e3b-9c41-4f2a-8b6d-1e2f3a4b5c6d",
                                                        "--claude", "claude", "--resume"], env=env)

        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(json.loads(first.stdout), {"type": "text_delta", "text": "re: one"})
        self.assertEqual(json.loads(resumed.stdout), {"type": "history", "messages": [
            {"type": "user", "text": "one"}, {"type": "text_delta", "text": "re: one"}]})
        self.assertEqual(json.loads(other.stdout), {"type": "history", "messages": []})

    def test_a_record_step_without_a_record_folder_exits_2(self):
        result = self.run_fake([{"record": {"type": "user", "text": "x"}}])

        self.assertEqual(result.returncode, 2)
        self.assertIn(b"FAKE_CLAUDE_RECORD", result.stderr)

    def test_an_mcp_call_starts_the_server_from_the_mcp_config_with_its_env(self):
        server = {"command": sys.executable, "args": [os.path.join(HERE, "mcp_stub.py")],
                  "env": {"GROVE_SESSION_ID": "3"}}
        self.mcp = self.write("mcp.json", {"mcpServers": {"grove": server}})

        result = self.run_fake([{"mcp_call": "grove", "tool": "subagent_spawn",
                                 "arguments": {"prompt": "{{session_id}}"}}],
                               env={"GROVE_SESSION_TOKEN": "kept out"})

        self.assertEqual(result.returncode, 0, result.stderr)
        [call] = [r for r in self.log_records() if "mcp_call" in r]
        reply = json.loads(call["reply"]["result"]["content"][0]["text"])
        self.assertEqual(reply["params"], {"name": "subagent_spawn", "arguments": {"prompt": SESSION}})
        self.assertEqual(reply["env"]["GROVE_SESSION_ID"], "3")
        self.assertNotIn("GROVE_SESSION_TOKEN", reply["env"])

    def test_refuses_the_steps_only_the_cli_fakes_take(self):
        for step in ({"hook": "Stop"}, {"print": "x"}, {"wait_input": "\r"}, {"send": "not an object"}):
            result = self.run_fake([step])
            self.assertEqual(result.returncode, 2, step)


if __name__ == "__main__":
    unittest.main()
