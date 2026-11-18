# Fake model for the load recording, built on #17's fake_api.py. It streams
# long replies with code blocks, tool calls and diffs in small deltas, so the
# real CLI redraws a lot, like a busy session.
# Run: python3 -I load_api.py PORT LOG REPO
#
# A prompt with GROVE_LOAD starts the turn in PLAN. Each request after it gets
# the next step, picked by how many assistant messages follow the prompt.
# File paths in tool calls are under REPO. Requests without tools (titles,
# Bash prefix checks) get "none", as in fake_api.py.
import json
import os
import sys
import time
from http.server import ThreadingHTTPServer

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                "..", "fake-agents", "claude-code", "record"))
import fake_api  # noqa: E402

MARKER = "GROVE_LOAD"
REPO = sys.argv[3] if len(sys.argv) > 3 else "."
CHUNK = 8  # characters per text delta, about 70 tokens/s with DELAY
DELAY = 0.03  # seconds between deltas
THINK = 2.5  # seconds before each reply, so the spinner runs

LIMITER = '''"""Token bucket rate limiter."""

import threading
import time


class TokenBucket:
    """Allows `rate` actions per second, with bursts up to `capacity`."""

    def __init__(self, rate: float, capacity: int, clock=time.monotonic):
        if rate <= 0 or capacity <= 0:
            raise ValueError("rate and capacity must be positive")
        self.rate = rate
        self.capacity = capacity
        self._clock = clock
        self._tokens = float(capacity)
        self._last = clock()
        self._lock = threading.Lock()

    def _refill(self) -> None:
        now = self._clock()
        elapsed = now - self._last
        self._tokens = self._tokens + elapsed * self.rate
        self._last = now

    def try_acquire(self, n: int = 1) -> bool:
        """Spends `n` tokens if there are enough. Never blocks."""
        with self._lock:
            self._refill()
            if self._tokens >= n:
                self._tokens -= n
                return True
            return False
'''

TESTS = '''import unittest

from limiter import TokenBucket


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def advance(self, seconds):
        self.now += seconds


class TokenBucketTest(unittest.TestCase):
    def setUp(self):
        self.clock = FakeClock()
        self.bucket = TokenBucket(rate=2, capacity=5, clock=self.clock)

    def drain(self):
        while self.bucket.try_acquire():
            pass

    def test_starts_full(self):
        self.assertTrue(all(self.bucket.try_acquire() for _ in range(5)))
        self.assertFalse(self.bucket.try_acquire())

    def test_refills_at_rate(self):
        self.drain()
        self.clock.advance(1.0)
        self.assertTrue(self.bucket.try_acquire(2))
        self.assertFalse(self.bucket.try_acquire())

    def test_never_holds_more_than_capacity(self):
        self.clock.advance(60)
        self.assertTrue(all(self.bucket.try_acquire() for _ in range(5)))
        self.assertFalse(self.bucket.try_acquire())

    def test_wait_time(self):
        self.drain()
        self.assertAlmostEqual(self.bucket.wait_time(1), 0.5)
        self.clock.advance(0.5)
        self.assertEqual(self.bucket.wait_time(1), 0.0)

    def test_rejects_bad_config(self):
        with self.assertRaises(ValueError):
            TokenBucket(rate=0, capacity=1)
'''

README = '''# limiter

A token bucket rate limiter (レート制限). Thread safe, no dependencies.

```python
from limiter import TokenBucket

bucket = TokenBucket(rate=10, capacity=20)  # 10/s, bursts of 20
if bucket.try_acquire():
    send_request()
else:
    time.sleep(bucket.wait_time())
```

| Parameter | Meaning |
|---|---|
| `rate` | Tokens added per second. Must be > 0. |
| `capacity` | Most tokens the bucket holds, so the largest burst. |
| `clock` | Seconds as a float. Defaults to `time.monotonic`. |

Guarantees: at most `capacity` actions in a burst, and at most
`capacity + rate × t` actions in any window of `t` seconds.
'''

WAIT_TIME = '''            return False

    def wait_time(self, n: int = 1) -> float:
        """Seconds until `n` tokens are free. 0.0 if they are free now."""
        with self._lock:
            self._refill()
            return max(0.0, (n - self._tokens) / self.rate)
'''

RUN_TESTS = {"command": "python3 -m unittest -v test_limiter", "description": "Run the limiter tests"}


def path(name):
    return os.path.join(REPO, name)


# One (text, tool name, tool input) per step. The last step has no tool.
PLAN = [
    ("I'll start by checking the repo state so I know what I'm building on.",
     "Bash", {"command": "git status --short --branch && git log --oneline -3",
              "description": "Show repo status and recent commits"}),
    ("""The repo only has its first commit, so this is a fresh module. The plan:

1. **`limiter.py`**: a `TokenBucket` class. It holds up to `capacity` tokens and refills at `rate` tokens per second.
2. **`test_limiter.py`**: unit tests driven by a fake clock, so they never sleep.
3. **`README.md`**: usage and the guarantees it makes.

The core idea: on every call, add `elapsed × rate` tokens, then spend `n` if there are enough.

```python
tokens = min(capacity, tokens + elapsed * rate)
if tokens >= n:
    tokens -= n
```

I'll write the class first.""",
     "Write", {"file_path": path("limiter.py"), "content": LIMITER}),
    ("Now the tests. A fake clock keeps them deterministic: time only moves when a test calls `advance()`.",
     "Write", {"file_path": path("test_limiter.py"), "content": TESTS}),
    ("Let me run them.", "Bash", RUN_TESTS),
    ("""Two failures, and both match what I'd expect from the first draft:

- `test_never_holds_more_than_capacity` fails. After 60 s idle the bucket holds **125** tokens, not 5. `_refill()` adds `elapsed × rate` with no upper bound, so a long pause turns into one huge burst. That defeats the point of a limiter.
- `test_wait_time` errors with `AttributeError`, since `wait_time()` doesn't exist yet.

Here's the trace for the first one, with `rate=2` and `capacity=5`:

| Step | Elapsed | Tokens before | Tokens after |
|---|---|---|---|
| start | 0 s | 5 | 5 |
| advance(60) | 60 s | 5 | 125 ✗ |
| expected | 60 s | 5 | 5 ✓ |

The fix is to clamp at `capacity` when refilling. I'll also guard against a clock that steps backwards. `time.monotonic()` can't, but a fake or custom clock could, so `elapsed` should never be negative.""",
     "Edit", {"file_path": path("limiter.py"),
              "old_string": "        elapsed = now - self._last\n"
                            "        self._tokens = self._tokens + elapsed * self.rate\n",
              "new_string": "        elapsed = max(0.0, now - self._last)\n"
                            "        self._tokens = min(self.capacity, self._tokens + elapsed * self.rate)\n"}),
    ("""Now `wait_time()`. It answers "how long until `n` tokens are free?", so callers can sleep instead of spinning:

```python
while not bucket.try_acquire():
    time.sleep(bucket.wait_time())
```

It refills first, so the answer is exact at the moment of the call.""",
     "Edit", {"file_path": path("limiter.py"), "old_string": "            return False\n",
              "new_string": WAIT_TIME}),
    ("Running the tests again.", "Bash", RUN_TESTS),
    ("All 5 pass ✓. Last, a short README so the next person knows the guarantees.",
     "Write", {"file_path": path("README.md"), "content": README}),
    ("Staging it and checking the size of the change.",
     "Bash", {"command": "git add -A && git diff --cached --stat", "description": "Stage and show the diff stat"}),
    ("""## Summary

Added a token bucket rate limiter with tests and docs. **3 files, all 5 tests pass.**

### What changed

- `limiter.py`: new `TokenBucket` class
  - `try_acquire(n=1)` spends `n` tokens if they're there, without blocking
  - `wait_time(n=1)` returns the seconds until `n` tokens are free
  - refill is clamped to `capacity` and ignores a clock that steps back
- `test_limiter.py`: 5 tests on a fake clock, so they run in µs and never sleep
- `README.md`: usage, parameters and guarantees

### The bug the tests caught

The first version let tokens pile up while idle. The fix:

```diff
-        elapsed = now - self._last
-        self._tokens = self._tokens + elapsed * self.rate
+        elapsed = max(0.0, now - self._last)
+        self._tokens = min(self.capacity, self._tokens + elapsed * self.rate)
```

### Notes

| Topic | Choice | Why |
|---|---|---|
| Clock | `time.monotonic` | wall clock jumps (NTP, DST) can't create or eat tokens |
| Locking | one `threading.Lock` | calls are short, so contention stays low below ~10⁵ calls/s |
| Fractional tokens | kept as `float` | a rate of 0.5/s works without rounding drift |

Not done: an async variant (`async with bucket:`) and per-key buckets for multi-tenant use. Happy to add either next.

That's the whole change.""", None, None),
]


def step(req):
    """Index into PLAN for this request, or None if no prompt has the marker."""
    messages = req.get("messages") or []
    starts = [i for i, m in enumerate(messages) if m.get("role") == "user" and MARKER in text_of(m)]
    if not starts:
        return None
    return sum(m.get("role") == "assistant" for m in messages[starts[-1]:])


def text_of(message):
    content = message.get("content")
    if isinstance(content, str):
        return content
    return " ".join(b.get("text", "") for b in content if b.get("type") == "text")


def reply(req):
    """Returns (blocks, stop_reason)."""
    if not req.get("tools"):
        return [{"type": "text", "text": "none"}], "end_turn"
    i = step(req)
    if i is None or i >= len(PLAN):
        return [{"type": "text", "text": "Hi."}], "end_turn"
    text, tool, tool_input = PLAN[i]
    blocks = [{"type": "text", "text": text}]
    if tool is None:
        return blocks, "end_turn"
    return blocks + [fake_api.tool_use(tool, tool_input)], "tool_use"


def events(model, blocks, stop_reason):
    """SSE events, with each text block split into CHUNK-sized deltas."""
    yield {"type": "message_start", "message": {
        "id": f"msg_fixture_{fake_api.counter}", "type": "message", "role": "assistant", "model": model,
        "content": [], "stop_reason": None, "stop_sequence": None,
        "usage": {"input_tokens": 1, "output_tokens": 1}}}
    for i, block in enumerate(blocks):
        if block["type"] == "text":
            yield {"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}}
            for at in range(0, len(block["text"]), CHUNK):
                yield {"type": "content_block_delta", "index": i,
                       "delta": {"type": "text_delta", "text": block["text"][at:at + CHUNK]}}
        else:
            yield {"type": "content_block_start", "index": i, "content_block": {**block, "input": {}}}
            yield {"type": "content_block_delta", "index": i,
                   "delta": {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}}
        yield {"type": "content_block_stop", "index": i}
    yield {"type": "message_delta", "delta": {"stop_reason": stop_reason, "stop_sequence": None},
           "usage": {"output_tokens": 1}}
    yield {"type": "message_stop"}


class Handler(fake_api.Handler):
    def do_POST(self):
        fake_api.counter += 1
        req = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        if "count_tokens" in self.path:
            self.send(200, b'{"input_tokens": 1}')
            return
        blocks, stop_reason = reply(req)
        fake_api.log(f"POST {self.path} n={fake_api.counter} step={step(req)} tools={len(req.get('tools') or [])}")
        model = req.get("model", "fixture")
        if not req.get("stream"):
            self.send(200, fake_api.message(model, blocks, stop_reason))
            return
        # HTTP/1.0 with no Content-Length: the body ends when the connection closes.
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        if req.get("tools"):
            time.sleep(THINK)
        for e in events(model, blocks, stop_reason):
            self.wfile.write(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n".encode())
            self.wfile.flush()
            if e["type"] == "content_block_delta":
                time.sleep(DELAY)


if __name__ == "__main__":
    port = int(sys.argv[1])
    fake_api.log(f"listening on {port}")
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
