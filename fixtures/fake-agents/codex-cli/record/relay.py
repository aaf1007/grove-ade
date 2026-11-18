# Stub hook relay for recording. record.py puts it on PATH as grove-hook.
# Run: python3 -I relay.py LOG [ARGS...]
# Appends one JSON line per call to LOG: the time, its extra args, its stdin
# and its environment. Like grove-hook it prints nothing and always exits 0.
import json
import os
import sys
import time

record = {"t": time.time(), "args": sys.argv[2:], "stdin": sys.stdin.read(), "env": dict(os.environ)}
with open(sys.argv[1], "a") as f:
    f.write(json.dumps(record) + "\n")
