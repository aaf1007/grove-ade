# Stub MCP server for recording. record.py puts it on PATH as grove-mcp.
# Run: python3 -I mcp_stub.py LOG
# Appends its environment to LOG as one JSON line, then answers MCP over
# stdio with no tools, so Codex starts it without an error on screen.
import json
import os
import sys

with open(sys.argv[1], "a") as f:
    f.write(json.dumps({"env": dict(os.environ)}) + "\n")

for line in sys.stdin:
    msg = json.loads(line)
    if "id" not in msg:
        continue
    if msg.get("method") == "initialize":
        result = {"protocolVersion": msg["params"]["protocolVersion"], "capabilities": {"tools": {}},
                  "serverInfo": {"name": "grove", "version": "0"}}
        reply = {"jsonrpc": "2.0", "id": msg["id"], "result": result}
    elif msg.get("method") == "tools/list":
        reply = {"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": []}}
    else:
        reply = {"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32601, "message": "not supported"}}
    sys.stdout.write(json.dumps(reply) + "\n")
    sys.stdout.flush()
