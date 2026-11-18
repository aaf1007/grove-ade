# Fake Anthropic Messages API, so recording runs the real CLI with no model
# and no usage. Point the CLI at it with ANTHROPIC_BASE_URL.
# Run: python3 -I fake_api.py PORT LOG
#
# It picks the main agent's reply from a marker in the latest user text:
#   GROVE_BASH gets a Bash tool call that needs approval.
#   GROVE_ASK gets an AskUserQuestion tool call.
#   GROVE_AGENT gets a subagent tool call.
#   GROVE_FAIL gets HTTP 400, which ends the turn on an error.
#   Anything else gets the text "Hi.".
# After a tool result it replies "Done.", which ends the turn.
# Requests without tools (titles, Bash prefix checks) get the text "none".
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = sys.argv[2] if len(sys.argv) > 2 else "fake-api.log"
counter = 0


def log(line):
    with open(LOG, "a") as f:
        f.write(f"{time.time():.3f} {line}\n")


def sse(model, blocks, stop_reason):
    events = [{"type": "message_start", "message": {
        "id": f"msg_fixture_{counter}", "type": "message", "role": "assistant", "model": model,
        "content": [], "stop_reason": None, "stop_sequence": None,
        "usage": {"input_tokens": 1, "output_tokens": 1}}}]
    for i, block in enumerate(blocks):
        if block["type"] == "text":
            events.append({"type": "content_block_start", "index": i,
                           "content_block": {"type": "text", "text": ""}})
            events.append({"type": "content_block_delta", "index": i,
                           "delta": {"type": "text_delta", "text": block["text"]}})
        else:
            events.append({"type": "content_block_start", "index": i,
                           "content_block": {**block, "input": {}}})
            events.append({"type": "content_block_delta", "index": i,
                           "delta": {"type": "input_json_delta",
                                     "partial_json": json.dumps(block["input"])}})
        events.append({"type": "content_block_stop", "index": i})
    events.append({"type": "message_delta",
                   "delta": {"stop_reason": stop_reason, "stop_sequence": None},
                   "usage": {"output_tokens": 1}})
    events.append({"type": "message_stop"})
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


def message(model, blocks, stop_reason):
    return json.dumps({
        "id": f"msg_fixture_{counter}", "type": "message", "role": "assistant", "model": model,
        "content": blocks, "stop_reason": stop_reason, "stop_sequence": None,
        "usage": {"input_tokens": 1, "output_tokens": 1}}).encode()


def last_user(req):
    # The CLI can put system messages after the latest user message.
    users = [m for m in req.get("messages") or [] if m.get("role") == "user"]
    if not users:
        return "", False
    content = users[-1].get("content")
    if isinstance(content, str):
        return content, False
    texts = [b.get("text", "") for b in content if b.get("type") == "text"]
    has_result = any(b.get("type") == "tool_result" for b in content)
    return " ".join(texts), has_result


def tool_use(name, tool_input):
    return {"type": "tool_use", "id": f"toolu_fixture_{counter}", "name": name, "input": tool_input}


def reply(req):
    """Returns (blocks, stop_reason), or None to answer with HTTP 400."""
    tools = [t.get("name") for t in req.get("tools") or []]
    text, has_result = last_user(req)
    if not tools:
        return [{"type": "text", "text": "none"}], "end_turn"
    if has_result:
        return [{"type": "text", "text": "Done."}], "end_turn"
    if "GROVE_FAIL" in text:
        return None
    if "GROVE_BASH" in text:
        return [tool_use("Bash", {"command": "touch note.txt", "description": "Create note.txt"})], "tool_use"
    if "GROVE_ASK" in text:
        question = {"question": "Which color?", "header": "Color", "multiSelect": False,
                    "options": [{"label": "Red", "description": "Warm"},
                                {"label": "Blue", "description": "Cool"}]}
        return [tool_use("AskUserQuestion", {"questions": [question]})], "tool_use"
    if "GROVE_AGENT" in text:
        name = "Agent" if "Agent" in tools else "Task"
        return [tool_use(name, {"description": "Say hi", "prompt": "Reply with the word hi.",
                                "subagent_type": "general-purpose"})], "tool_use"
    return [{"type": "text", "text": "Hi."}], "end_turn"


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, code, body, content_type="application/json"):
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        log(f"GET {self.path}")
        self.send(404, b"{}")

    def do_HEAD(self):
        log(f"HEAD {self.path}")
        self.send(200, b"")

    def do_POST(self):
        global counter
        counter += 1
        body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
        try:
            req = json.loads(body)
        except ValueError:
            log(f"POST {self.path} undecodable body len={len(body)}")
            self.send(400, b"{}")
            return
        if "count_tokens" in self.path:
            log(f"POST {self.path}")
            self.send(200, b'{"input_tokens": 1}')
            return
        text, has_result = last_user(req)
        tools = [t.get("name") for t in req.get("tools") or []]
        log(f"POST {self.path} n={counter} stream={req.get('stream')} tools={len(tools)} "
            f"result={has_result} user={text[-60:]!r}")
        if counter == 1 or "GROVE_AGENT" in text:
            log(f"  tool names: {tools}")
        picked = reply(req)
        if picked is None:
            err = {"type": "error", "error": {"type": "invalid_request_error",
                                              "message": "fixture: forced failure"}}
            self.send(400, json.dumps(err).encode())
            return
        blocks, stop_reason = picked
        model = req.get("model", "fixture")
        if req.get("stream"):
            self.send(200, sse(model, blocks, stop_reason), "text/event-stream")
        else:
            self.send(200, message(model, blocks, stop_reason))


if __name__ == "__main__":
    port = int(sys.argv[1])
    log(f"listening on {port}")
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
