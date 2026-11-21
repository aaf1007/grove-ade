# The parts the fake agents share: the scenario format, the log, raw stdin and
# running command hooks. README.md describes them. Python 3 standard library only.
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import termios
import threading
import time

# For each scenario step action: its value type and the option keys it allows.
ACTIONS = {"hook": (str, {"set"}), "print": (str, set()), "screen": (str, set()), "mode": (str, {"on"}),
           "wait_input": (str, set()), "sleep": ((int, float), set()), "session_id": (str, set()),
           "exit": (int, set()), "signal": (str, set()), "send": (dict, set()), "wait_message": (str, set()),
           "replay": (str, {"speed"}), "record": (dict, set()), "mcp_call": (str, {"tool", "arguments"}),
           "shell": (str, set())}
# The actions the agent CLI fakes take. The sidecar fake takes SIDECAR_ACTIONS instead.
CLI_ACTIONS = set(ACTIONS) - {"send", "wait_message", "replay"}
SIDECAR_ACTIONS = {"send", "wait_message", "replay", "sleep", "exit", "signal", "record", "mcp_call"}
# How long a shell step's command may run.
SHELL_TIMEOUT = 60
# DEC private modes a "mode" step can turn on or off.
MODES = {"bracketed_paste": 2004, "focus_events": 1004, "alternate_screen": 1049, "show_cursor": 25}


class Fail(Exception):
    """Ends the fake with a message on stderr and exit code 1."""
    code = 1


class Usage(Fail):
    """Bad argv, config or scenario."""
    code = 2


class Log:
    """Appends one JSON object per line to the log file. No-op when path is empty."""

    def __init__(self, path):
        self.path = path
        self.lock = threading.Lock()
        self.start = time.monotonic()

    def write(self, **record):
        if not self.path:
            return
        line = json.dumps({"t": round(time.monotonic() - self.start, 3), "pid": os.getpid(), **record})
        with self.lock, open(self.path, "a") as f:
            f.write(line + "\n")


class Input:
    """Reads stdin on a thread and logs every byte. A terminal goes into raw
    mode first, as the real CLIs do: no echo, no line buffering, Ctrl-C is a byte.
    on_line, if given, gets each whole line as it arrives, before any wait sees it."""

    def __init__(self, log, on_line=None):
        self.log = log
        self.on_line = on_line
        self.buf = b""
        self.pos = 0
        # Where the next line for on_line starts.
        self.line_start = 0
        self.closed = False
        self.cond = threading.Condition()
        self.saved = termios.tcgetattr(0) if os.isatty(0) else None
        if self.saved:
            mode = termios.tcgetattr(0)
            mode[0] &= ~(termios.BRKINT | termios.ICRNL | termios.INPCK | termios.ISTRIP | termios.IXON)
            mode[3] &= ~(termios.ECHO | termios.ICANON | termios.IEXTEN | termios.ISIG)
            mode[6][termios.VMIN] = 1
            mode[6][termios.VTIME] = 0
            termios.tcsetattr(0, termios.TCSANOW, mode)
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        while True:
            try:
                data = os.read(0, 4096)
            except OSError:
                data = b""
            if data:
                self.log.write(stdin=data.decode("latin-1"))
            with self.cond:
                if self.on_line:
                    whole = (self.buf + data)[self.line_start:].split(b"\n")[:-1]
                    for line in whole:
                        self.on_line(line)
                        self.line_start += len(line) + 1
                self.buf += data
                self.closed = not data
                self.cond.notify_all()
            if not data:
                return

    def wait_for(self, text):
        """Returns once text arrives after the end of the last match, with what came
        before it since then."""
        needle = text.encode()
        with self.cond:
            while True:
                i = self.buf.find(needle, self.pos)
                if i >= 0:
                    before = self.buf[self.pos:i]
                    self.pos = i + len(needle)
                    return before.decode(errors="replace")
                if self.closed:
                    raise Fail(f"stdin closed while waiting for {text!r}")
                self.cond.wait()

    def wait_message(self, kind):
        """Returns the next whole line after the last match that is a JSON object
        of this type. Lines before it are skipped."""
        with self.cond:
            while True:
                end = self.buf.find(b"\n", self.pos)
                if end < 0:
                    if self.closed:
                        raise Fail(f"stdin closed while waiting for a {kind} message")
                    self.cond.wait()
                    continue
                line = self.buf[self.pos:end]
                self.pos = end + 1
                try:
                    message = json.loads(line)
                except ValueError:
                    continue
                if isinstance(message, dict) and message.get("type") == kind:
                    return message

    def restore(self):
        if self.saved:
            termios.tcsetattr(0, termios.TCSADRAIN, self.saved)


def record_path(variable, conversation):
    """The record of one conversation: <folder>/<id>.jsonl, the folder named by the
    environment variable. None when it is unset."""
    folder = os.environ.get(variable)
    return os.path.join(folder, f"{conversation}.jsonl") if folder else None


def append_record(variable, conversation, message):
    """Adds a message to the conversation's record, as the real agents keep one."""
    path = record_path(variable, conversation)
    if not path:
        raise Usage(f"set {variable} to a folder for the record step")
    with open(path, "a") as f:
        f.write(json.dumps(message, ensure_ascii=False) + "\n")


def read_record(variable, conversation):
    """The conversation's record, oldest first. Empty when there is none."""
    path = record_path(variable, conversation)
    if not path or not os.path.exists(path):
        return []
    with open(path) as f:
        return [json.loads(line) for line in f]


def read_json(path, what, log=None):
    """Parses a JSON file. With a log, logs the text first."""
    try:
        with open(path) as f:
            text = f.read()
    except OSError as e:
        raise Usage(f"{what} {path}: {e}")
    if log:
        log.write(file=path, content=text)
    try:
        return json.loads(text)
    except ValueError as e:
        raise Usage(f"{what} {path}: {e}")


def data_path(data, kind, name):
    return os.path.join(data, kind, name + (".json" if kind == "payloads" else ".ansi"))


def check_claude_settings(settings):
    """A Claude Code settings file: every hook a command, with a number timeout."""
    try:
        for event, groups in settings.get("hooks", {}).items():
            for group in groups:
                for hook in group["hooks"]:
                    if hook["type"] != "command" or not isinstance(hook["command"], str):
                        raise Usage(f"settings: a {event} hook is not a command: {hook}")
                    if not isinstance(hook.get("timeout", 60), (int, float)):
                        raise Usage(f"settings: a {event} hook timeout is not a number: {hook}")
    except (AttributeError, KeyError, TypeError) as e:
        raise Usage(f"settings: unexpected shape ({e!r})")


def check_mcp_config(config):
    if not isinstance(config, dict) or not isinstance(config.get("mcpServers"), dict):
        raise Usage("mcp config: no mcpServers object")


# What an MCP server gets from the agent's own environment, besides what its config passes. The
# MCP SDK and Codex pass about this much, so a test shows the config passes what grove-mcp needs.
MCP_BASE_ENV = ("HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER")
# How long an MCP call may take, start to reply.
MCP_TIMEOUT = 10


def mcp_env(extra):
    """The environment an MCP server starts with: MCP_BASE_ENV from ours, plus extra."""
    env = {k: os.environ[k] for k in MCP_BASE_ENV if k in os.environ}
    env.update(extra)
    return env


def mcp_call(command, args, env, tool, arguments, log):
    """Starts an MCP server on stdio, as an agent does, and calls one of its tools. Logs the
    tools/call reply, or the error, and the server's stderr."""
    messages = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "fake-agent", "version": "0"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool, "arguments": arguments}},
    ]
    with tempfile.TemporaryFile() as stderr:
        try:
            proc = subprocess.Popen([command, *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                    stderr=stderr, env=env)
        except OSError as e:
            log.write(mcp_call=tool, error=f"start {command}: {e}")
            return
        timer = threading.Timer(MCP_TIMEOUT, proc.kill)
        timer.start()
        reply = None
        try:
            for message in messages:
                proc.stdin.write(json.dumps(message).encode() + b"\n")
                proc.stdin.flush()
                if "id" not in message:
                    continue
                for line in proc.stdout:
                    answer = json.loads(line)
                    if answer.get("id") == message["id"]:
                        reply = answer
                        break
                else:
                    reply = {"error": "the server closed stdout"}
                    break
            proc.stdin.close()
            proc.wait()
        except (OSError, ValueError) as e:
            reply = {"error": f"talk to the server: {e}"}
            proc.kill()
            proc.wait()
        finally:
            timer.cancel()
        stderr.seek(0)
        log.write(mcp_call=tool, reply=reply, server_exit=proc.returncode,
                  stderr=stderr.read().decode(errors="replace"))


def check_step(step, data, allowed):
    actions = [k for k in step if k in allowed] if isinstance(step, dict) else []
    if len(actions) != 1 or set(step) & (set(ACTIONS) - allowed):
        raise Usage(f"want exactly one of {sorted(allowed)}")
    action = actions[0]
    kind, options = ACTIONS[action]
    value = step[action]
    if not isinstance(value, kind) or isinstance(value, bool):
        raise Usage(f"{action} has a bad value {value!r}")
    if set(step) - {action} - options:
        raise Usage(f"unknown options {sorted(set(step) - {action} - options)}")
    if action == "hook" and (not os.path.exists(data_path(data, "payloads", value))
                             or not isinstance(step.get("set", {}), dict)):
        raise Usage(f"no payload {value}, or set is not an object")
    if action == "screen" and not os.path.exists(data_path(data, "screens", value)):
        raise Usage(f"no screen {value}")
    if action == "mode" and (value not in MODES or not isinstance(step.get("on"), bool)):
        raise Usage(f"mode wants one of {sorted(MODES)} and on: true or false")
    if action in ("wait_input", "wait_message") and not value:
        raise Usage(f"{action} wants some text")
    if action == "signal" and signal_number(value) is None:
        raise Usage(f"unknown signal {value}")
    if action == "mcp_call" and (not isinstance(step.get("tool"), str)
                                 or not isinstance(step.get("arguments", {}), dict)):
        raise Usage("mcp_call wants a tool name and arguments as an object")
    if action == "replay":
        speed = step.get("speed", 1)
        if not isinstance(speed, (int, float)) or isinstance(speed, bool) or speed <= 0:
            raise Usage(f"replay speed must be a number above 0, not {speed!r}")
        read_recording(value)


def read_recording(path):
    """The lines of a recorded protocol stream: one JSON object per line, with
    t (seconds since the recording started, never going back), from ("core" or
    "sidecar") and message (a protocol message, an object with a type)."""
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
    except OSError as e:
        raise Usage(f"recording {path}: {e}")
    lines = []
    for n, raw in enumerate(text.splitlines(), 1):
        try:
            line = json.loads(raw)
        except ValueError as e:
            raise Usage(f"recording {path} line {n}: {e}")
        if (not isinstance(line, dict) or set(line) != {"t", "from", "message"}
                or not isinstance(line["t"], (int, float)) or isinstance(line["t"], bool)
                or line["from"] not in ("core", "sidecar") or not isinstance(line["message"], dict)
                or not isinstance(line["message"].get("type"), str)):
            raise Usage(f"recording {path} line {n}: want t, from (core or sidecar) and a message with a type")
        if lines and line["t"] < lines[-1]["t"]:
            raise Usage(f"recording {path} line {n}: t goes back")
        lines.append(line)
    return lines


def replay(lines, speed, slots, stdin, log):
    """Sends the sidecar's recorded lines at their recorded times divided by
    speed, and waits for each of the core's recorded lines by its type. The
    clock restarts when a core line arrives, so waiting for the core never
    makes later lines rush. Logs replay_times once it stops: the Unix time it
    heard each core line or had sent each sidecar line, in the recording's
    order. One record at the end, so logging never slows the replay."""
    start, base = time.monotonic(), 0
    times = []
    try:
        for line in lines:
            message = line["message"]
            if line["from"] == "core":
                stdin.wait_message(message["type"])
                start, base = time.monotonic(), line["t"]
                times.append(time.time())
                continue
            delay = start + (line["t"] - base) / speed - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            send(message, slots)
            times.append(time.time())
    finally:
        log.write(replay_times=times)


def send(message, slots):
    """Writes a protocol message as one compact JSON line, placeholders filled."""
    write_out(json.dumps(fill(message, slots), ensure_ascii=False, separators=(",", ":")).encode() + b"\n")


def load_scenario(variable, data, allowed=CLI_ACTIONS):
    """Reads and checks the scenario file named by the environment variable.
    allowed is the set of actions the fake takes."""
    path = os.environ.get(variable)
    if not path:
        raise Usage(f"set {variable} to a scenario file")
    scenario = read_json(path, "scenario")
    if not isinstance(scenario, dict) or set(scenario) != {"steps"} or not isinstance(scenario["steps"], list):
        raise Usage("scenario: want an object with only a steps list")
    for n, step in enumerate(scenario["steps"], 1):
        try:
            check_step(step, data, allowed)
        except Usage as e:
            raise Usage(f"scenario step {n}: {e}")
    return scenario["steps"]


def signal_number(name):
    return signal.Signals.__members__.get(name if name.startswith("SIG") else "SIG" + name)


def write_out(data):
    while data:
        data = data[os.write(1, data):]


def fill(value, slots):
    if isinstance(value, str):
        for name, text in slots.items():
            value = value.replace("{{" + name + "}}", text)
        return value
    if isinstance(value, dict):
        return {k: fill(v, slots) for k, v in value.items()}
    if isinstance(value, list):
        return [fill(v, slots) for v in value]
    return value


def matches(matcher, value):
    """"*" or empty matches all, "A|B" matches names exactly, anything else is a regex."""
    if matcher in ("", "*"):
        return True
    if re.fullmatch(r"[A-Za-z0-9_|]+", matcher):
        return value in matcher.split("|")
    try:
        return re.search(matcher, value) is not None
    except re.error:
        return False


def run_hooks(groups, payload, field, log, shell, env, timeout):
    """Runs each command hook of the matching groups in order, through the
    shell argv prefix, with the payload as compact JSON on stdin, as the real
    CLIs send it. field is the payload field matchers are checked against, or
    None. timeout is the default for hooks without one. Output is logged, never
    shown."""
    event = payload["hook_event_name"]
    stdin = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode()
    for group in groups:
        if field and not matches(group.get("matcher", ""), payload.get(field, "")):
            continue
        for hook in group["hooks"]:
            try:
                done = subprocess.run([*shell, hook["command"]], input=stdin, capture_output=True,
                                      env=env, timeout=hook.get("timeout", timeout))
                code, out, err = done.returncode, done.stdout, done.stderr
            except subprocess.TimeoutExpired as e:
                code, out, err = None, e.stdout or b"", e.stderr or b""
            log.write(hook=event, command=hook["command"], code=code,
                      stdout=out.decode(errors="replace"), stderr=err.decode(errors="replace"))


def run_shell(command, shell, env, log, timeout=SHELL_TIMEOUT):
    """Runs a command as the agent's shell tool does, through the shell argv prefix in the
    fake's folder, and logs its exit code and output. code is null when it timed out, and then
    everything the command started is killed too."""
    proc = subprocess.Popen([*shell, command], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, env=env, start_new_session=True)
    try:
        out, err = proc.communicate(timeout=timeout)
        code = proc.returncode
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        out, err = proc.communicate()
        code = None
    log.write(shell=command, code=code, stdout=out.decode(errors="replace"), stderr=err.decode(errors="replace"))


def play(steps, data, slots, stdin, log, fire, on_send=None, record=None, call=None, shell=None):
    """Runs the scenario steps. fire(payload) runs the hooks for a hook step.
    call(server, tool, arguments) calls an MCP tool for an mcp_call step.
    shell(command) runs a shell step's command.
    on_send, if given, gets each send step's message before its placeholders fill.
    record(message) adds a record step's message to the conversation's record."""
    for step in steps:
        if "hook" in step:
            payload = fill(read_json(data_path(data, "payloads", step["hook"]), "payload"), slots)
            fire({**payload, **step.get("set", {})})
        elif "print" in step:
            write_out(fill(step["print"], slots).encode())
        elif "screen" in step:
            write_out(screen(data, step["screen"], slots))
        elif "mode" in step:
            write_out(f"\x1b[?{MODES[step['mode']]}{'h' if step['on'] else 'l'}".encode())
        elif "wait_input" in step:
            slots["text"] = stdin.wait_for(step["wait_input"])
        elif "send" in step:
            if on_send:
                on_send(step["send"])
            send(step["send"], slots)
        elif "wait_message" in step:
            message = stdin.wait_message(step["wait_message"])
            if isinstance(message.get("text"), str):
                slots["text"] = message["text"]
        elif "record" in step:
            record(fill(step["record"], slots))
        elif "replay" in step:
            replay(read_recording(step["replay"]), step.get("speed", 1), slots, stdin, log)
        elif "mcp_call" in step:
            call(step["mcp_call"], step["tool"], fill(step.get("arguments", {}), slots))
        elif "shell" in step:
            shell(fill(step["shell"], slots))
        elif "sleep" in step:
            time.sleep(step["sleep"])
        elif "session_id" in step:
            slots["session_id"] = step["session_id"]
        elif "exit" in step:
            return step["exit"]
        elif "signal" in step:
            sig = signal_number(step["signal"])
            log.write(signal=step["signal"])
            # Python raises KeyboardInterrupt on SIGINT and ignores SIGPIPE.
            if sig not in (signal.SIGKILL, signal.SIGSTOP):
                signal.signal(sig, signal.SIG_DFL)
            os.kill(os.getpid(), sig)
            time.sleep(0.5)
            raise Fail(f"still running after {sig.name}")
    return 0


def screen(data, name, slots):
    """A recorded screen's bytes, with placeholders filled."""
    with open(data_path(data, "screens", name), "rb") as f:
        raw = f.read()
    for slot, text in slots.items():
        raw = raw.replace(b"{{" + slot.encode() + b"}}", text.encode())
    return raw


def main(name, prefix, run):
    """Runs run(args, log) and returns the exit code. The log path comes from
    <prefix>_LOG. A Fail prints "fake <name>: ..." on stderr."""
    log = Log(os.environ.get(prefix + "_LOG"))
    log.write(argv=sys.argv, cwd=os.getcwd(), env=dict(os.environ))
    try:
        code = run(sys.argv[1:], log)
    except Fail as e:
        print(f"fake {name}: {e}", file=sys.stderr)
        log.write(error=str(e))
        code = e.code
    log.write(exit=code)
    return code
