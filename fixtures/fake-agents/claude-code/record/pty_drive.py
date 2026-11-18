# Drive a TUI in a pseudo-terminal. Adapted from the spike #9 scripts.
# Steps are tuples:
#   ("wait", regex, timeout_s)  wait until output since the last mark, ANSI
#                               stripped, matches; returns False on timeout
#   ("send", text)              write text (\r is Enter, \x1b is Esc)
#   ("sleep", seconds)
#   ("mark",)                   later waits only look at output after this
#   ("snap", name)              save all raw output so far as snaps[name]
# Answers cursor position and color queries so the TUI does not stall.
import fcntl
import os
import pty
import re
import select
import signal
import struct
import termios
import time

ANSI = re.compile(rb"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][A-Za-z0-9]|\x1b[=>78NOMDEHc]")
ROWS, COLS = 40, 120


def drive(cmd, env, cwd, steps, log=print):
    """Runs cmd through steps. Returns (raw output, snaps, failed waits)."""
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(cwd)
        os.execve(cmd[0], cmd, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    raw = bytearray()
    snaps = {}
    failed = []
    mark = 0

    def pump(timeout):
        r, _, _ = select.select([fd], [], [], timeout)
        if not r:
            return True
        try:
            data = os.read(fd, 65536)
        except OSError:
            return False
        if not data:
            return False
        raw.extend(data)
        if b"\x1b[6n" in data:
            os.write(fd, b"\x1b[1;1R")
        if b"\x1b]10;?" in data:
            os.write(fd, b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\")
        if b"\x1b]11;?" in data:
            os.write(fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")
        if b"\x1b[c" in data:
            os.write(fd, b"\x1b[?62;22c")
        return True

    def text_since(m):
        return ANSI.sub(b"", bytes(raw[m:])).decode("utf-8", "replace")

    alive = True
    for step in steps:
        kind = step[0]
        log(f"[{time.strftime('%H:%M:%S')}] {step}")
        if kind == "wait":
            pat = re.compile(step[1])
            deadline = time.time() + step[2]
            ok = False
            while time.time() < deadline and alive:
                alive = pump(0.2)
                if pat.search(text_since(mark)):
                    ok = True
                    break
            if not ok:
                log("    wait TIMEOUT")
                failed.append(step)
        elif kind == "send":
            if not alive:
                log("    process gone")
                break
            os.write(fd, step[1].encode())
            alive = pump(0.1)
        elif kind == "sleep":
            end = time.time() + step[1]
            while time.time() < end and alive:
                alive = pump(0.2)
        elif kind == "mark":
            mark = len(raw)
        elif kind == "snap":
            snaps[step[1]] = bytes(raw)
    end = time.time() + 2
    while time.time() < end and alive:
        alive = pump(0.2)
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.kill(pid, sig)
        except ProcessLookupError:
            break
        time.sleep(0.5)
    # Close first: a child that exits with unread output blocks until it drains.
    os.close(fd)
    os.waitpid(pid, 0)
    return bytes(raw), snaps, failed
