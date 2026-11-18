# Fake OpenAI Responses API, so recording runs the real Codex CLI with no
# model and no usage. Codex reaches it through a model provider whose
# base_url is http://127.0.0.1:PORT/v1.
# Run: python3 -I fake_model.py PORT LOG
#
# It picks the reply from a marker in the latest user text:
#   GROVE_SHELL gets a shell call that needs approval.
#   GROVE_ASK gets a request_user_input call.
#   Anything else gets the text "Hi.".
# After a tool result it replies "Done.", which ends the turn.
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = sys.argv[2] if len(sys.argv) > 2 else "fake-model.log"
counter = 0


def log(line):
    with open(LOG, "a") as f:
        f.write(f"{time.time():.3f} {line}\n")


def text_of(item):
    content = item.get("content")
    if isinstance(content, str):
        return content
    return " ".join(c.get("text", "") for c in content or [] if isinstance(c, dict))


def last_turn(req):
    """Returns (latest user text, whether a tool output came after it)."""
    items = req.get("input") or []
    text, has_result = "", False
    for item in items:
        if item.get("type") == "message" and item.get("role") == "user":
            text, has_result = text_of(item), False
        elif item.get("type") in ("function_call_output", "custom_tool_call_output"):
            has_result = True
    return text, has_result


def tool_names(req):
    return [t.get("name") or t.get("type") for t in req.get("tools") or []]


def function_call(name, arguments):
    return {"type": "function_call", "id": f"fc_fixture_{counter}", "call_id": f"call_fixture_{counter}",
            "name": name, "arguments": json.dumps(arguments), "status": "completed"}


def message(text):
    return {"type": "message", "id": f"msg_fixture_{counter}", "role": "assistant", "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}]}


def reply(req):
    text, has_result = last_turn(req)
    if has_result:
        return [message("Done.")]
    if "GROVE_SHELL" in text:
        return [function_call("exec_command", {"cmd": "touch note.txt", "sandbox_permissions": "require_escalated",
                                               "justification": "Create note.txt"})]
    if "GROVE_ASK" in text:
        question = {"id": "color", "header": "Color", "question": "Which color?",
                    "options": [{"label": "Red", "description": "Warm"},
                                {"label": "Blue", "description": "Cool"}]}
        return [function_call("request_user_input", {"questions": [question]})]
    return [message("Hi.")]


def sse(items):
    rid = f"resp_fixture_{counter}"
    events = [{"type": "response.created", "response": {"id": rid}}]
    for i, item in enumerate(items):
        events.append({"type": "response.output_item.added", "output_index": i, "item": item})
        if item["type"] == "message":
            events.append({"type": "response.output_text.delta", "output_index": i, "content_index": 0,
                           "item_id": item["id"], "delta": item["content"][0]["text"]})
        events.append({"type": "response.output_item.done", "output_index": i, "item": item})
    usage = {"input_tokens": 1, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 1,
             "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": 2}
    events.append({"type": "response.completed", "response": {"id": rid, "usage": usage}})
    return "".join(f"event: {e['type']}\ndata: {json.dumps(e)}\n\n" for e in events).encode()


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
        text, has_result = last_turn(req)
        log(f"POST {self.path} n={counter} stream={req.get('stream')} tools={tool_names(req)} "
            f"result={has_result} user={text[-60:]!r}")
        self.send(200, sse(reply(req)), "text/event-stream")


if __name__ == "__main__":
    port = int(sys.argv[1])
    log(f"listening on {port}")
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
