# A stand-in MCP server for the fakes' own tests. It answers initialize, and answers every tool
# call with the call's params and its own environment, as JSON text. Standard library only.
import json
import os
import sys

for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    if message["method"] == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "stub", "version": "0"}}
    else:
        text = json.dumps({"params": message["params"], "env": dict(os.environ)})
        result = {"content": [{"type": "text", "text": text}]}
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
