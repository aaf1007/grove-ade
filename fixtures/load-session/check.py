#!/usr/bin/env python3
"""Check the recorded load session and its replay command.

Run: python3 fixtures/load-session/check.py
Needs Python 3.9 or later and pv.
"""

import os
import re
import select
import signal
import subprocess
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
RECORDING = HERE / "session.ansi"
REPLAY = HERE / "replay"

MAX_BYTES = 2 * 1024 * 1024

# SGR with a 256-color or truecolor foreground.
COLOR = re.compile(rb"\x1b\[(?:[0-9]+;)*38;[25];[0-9;]*m")
# CUU, CUD, CUF, CUB, CHA, CUP.
CURSOR_MOVE = re.compile(rb"\x1b\[[0-9;]*[ABCDGH]")
ESCAPE = re.compile(rb"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Za-z0-9]|\x1b[=>78DEHMc]")

# Generic patterns on purpose: never a real username or hostname.
SECRETS = {
    "home path": rb"/Users/|/home/",
    "temp path": rb"/private/|/var/folders/",
    "token": rb"sk-ant-|\bsk-[A-Za-z0-9_-]{20,}|\bgh[pousr]_[A-Za-z0-9]{20,}|github_pat_|\bxox[abprs]-"
             rb"|\bAKIA[0-9A-Z]{16}|Bearer ",
    "session id": rb"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
    "email": rb"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+",
    "hostname": rb"\b[A-Za-z0-9-]+\.local\b",
}

# Replayed in a shell session, a query makes the terminal type its answer into
# the shell. DA1, DA2, XTVERSION, DSR, DECRQM, kitty flags, XTWINOPS, OSC
# color and clipboard queries, XTGETTCAP and DECRQSS.
QUERY = re.compile(rb"\x1b\[[0-9]*c|\x1b\[[>=][0-9;]*[cq]|\x1b\[\??[0-9;]*n|\x1b\[\?[0-9;]*\$p|\x1b\[\?u"
                   rb"|\x1b\[[0-9;]*t|\x1b\](?:[0-9]+;)+\?|\x1b\]52;[^;\x07\x1b]*;\?|\x1bP[$+]q")
# Modes that make the terminal send input of its own (mouse, focus, theme
# reports) or change how keys arrive, so Ctrl-C might not stop the replay.
INPUT_MODE = re.compile(rb"\x1b\[\?(?:[0-9]+;)*(?:9|100[0-6]|101[56]|2031)(?:;[0-9]+)*h"
                        rb"|\x1b\[[>=][0-9;]*u|\x1b\[>4;[1-9][0-9]*m")


class Recording(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.data = RECORDING.read_bytes()

    def test_looks_like_agent_tui_output(self):
        self.assertGreater(len(COLOR.findall(self.data)), 100)
        self.assertGreater(len(CURSOR_MOVE.findall(self.data)), 100)
        text = self.data.decode("utf-8")
        self.assertGreater(len(re.findall(r"[^\x00-\x7f]", text)), 100)

    def test_small_enough_to_commit(self):
        self.assertLess(len(self.data), MAX_BYTES)

    def test_has_nothing_secret_looking(self):
        # Also check with escapes removed, in case a color change splits one.
        for view in (self.data, ESCAPE.sub(b"", self.data)):
            for name, pattern in SECRETS.items():
                with self.subTest(name):
                    self.assertEqual(re.findall(pattern, view), [])

    def test_never_queries_the_terminal_or_changes_its_input(self):
        self.assertEqual(QUERY.findall(self.data), [])
        self.assertEqual(INPUT_MODE.findall(self.data), [])


def replay(args, nbytes, seconds):
    """Reads up to nbytes of replay output for at most `seconds`, then closes
    the pipe. Closing it must stop the whole loop within 5 s."""
    proc = subprocess.Popen([str(REPLAY), *args], stdout=subprocess.PIPE, start_new_session=True)
    out = bytearray()
    deadline = time.monotonic() + seconds
    try:
        while len(out) < nbytes:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([proc.stdout], [], [], left)[0]:
                break
            chunk = os.read(proc.stdout.fileno(), nbytes - len(out))
            if not chunk:
                break
            out += chunk
        proc.stdout.close()
        proc.wait(timeout=5)
    finally:
        # A failed test must not leave the loop running.
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    return bytes(out)


class Replay(unittest.TestCase):
    def test_loops_past_the_end_of_the_recording(self):
        data = RECORDING.read_bytes()
        n = 2 * len(data) + 1000
        self.assertEqual(replay(["100m"], n, 30), (data * 3)[:n])

    def test_streams_at_50_kb_per_second_by_default(self):
        start = time.monotonic()
        out = replay([], 10**9, 2)
        rate = len(out) / (time.monotonic() - start)
        self.assertGreater(rate, 0.75 * 50 * 1024)
        self.assertLess(rate, 1.25 * 50 * 1024)


if __name__ == "__main__":
    unittest.main()
