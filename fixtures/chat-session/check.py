#!/usr/bin/env python3
"""Check the recorded chat session.

Run: python3 fixtures/chat-session/check.py
Needs Python 3.9 or later.
"""

import json
import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
RECORDING = HERE / "session.jsonl"

MAX_BYTES = 2 * 1024 * 1024
MIN_SECONDS = 600

# What replay can play without a user: no approvals or questions, which would wait for an answer.
SIDECAR_TYPES = {"account", "turn_start", "text_delta", "tool_call", "tool_result", "turn_end"}
FILE_TOOLS = {"Write", "Edit", "MultiEdit", "NotebookEdit"}

# Generic patterns on purpose: never a real username or hostname.
SECRETS = {
    "home path": r"/Users/|/home/",
    "temp path": r"/private/|/var/folders/",
    "token": r"sk-ant-|\bsk-[A-Za-z0-9_-]{20,}|\bgh[pousr]_[A-Za-z0-9]{20,}|github_pat_|\bxox[abprs]-"
             r"|\bAKIA[0-9A-Z]{16}|Bearer |-----BEGIN [A-Z ]*PRIVATE KEY",
    "session id": r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
    "email": r"[A-Za-z0-9._%+-]+@(?!example\.com\b)[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+",
    "hostname": r"\b[A-Za-z0-9-]+\.local\b",
}


class Recording(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.text = RECORDING.read_text(encoding="utf-8")
        cls.lines = [json.loads(line) for line in cls.text.splitlines()]

    def sidecar(self, kind):
        return [line["message"] for line in self.lines
                if line["from"] == "sidecar" and line["message"]["type"] == kind]

    def test_every_line_is_a_timed_protocol_message_replay_can_play(self):
        last = 0
        for line in self.lines:
            self.assertEqual(set(line), {"t", "from", "message"})
            self.assertGreaterEqual(line["t"], last)
            last = line["t"]
            kind = line["message"]["type"]
            if line["from"] == "core":
                self.assertEqual(kind, "user_message")
            else:
                self.assertEqual(line["from"], "sidecar")
                self.assertIn(kind, SIDECAR_TYPES)

    def test_lasts_at_least_10_minutes(self):
        self.assertGreaterEqual(self.lines[-1]["t"] - self.lines[0]["t"], MIN_SECONDS)

    def test_has_tool_calls_file_changes_and_long_text(self):
        calls = self.sidecar("tool_call")
        self.assertGreaterEqual(len(calls), 20)
        self.assertGreaterEqual(len(self.sidecar("tool_result")), 20)
        self.assertTrue(any(call["tool"] in FILE_TOOLS for call in calls))
        self.assertTrue(any(call["tool"] == "Bash" for call in calls))
        # Long text: some run of deltas between two tool calls, such as a summary.
        runs, run = [], ""
        for line in self.lines:
            message = line["message"]
            if message["type"] == "text_delta":
                run += message["text"]
            else:
                runs.append(run)
                run = ""
        runs.append(run)
        self.assertGreaterEqual(max(map(len, runs)), 2000)
        self.assertEqual(self.lines[-1]["message"]["type"], "turn_end")

    def test_small_enough_to_commit(self):
        self.assertLess(len(self.text.encode()), MAX_BYTES)

    def test_has_nothing_secret_looking(self):
        for name, pattern in SECRETS.items():
            with self.subTest(name):
                self.assertEqual(re.findall(pattern, self.text), [])

    def test_the_account_is_a_placeholder(self):
        for message in self.sidecar("account"):
            account = message["account"]
            if account is not None:
                self.assertIn(account.get("email"), (None, "user@example.com"))
                self.assertIsNone(account.get("organization"))


if __name__ == "__main__":
    unittest.main()
