"""Local TLS fixture server for the live-provider harness's no-network proof.

Usage: python3 -I fixture_server.py <state-dir>

Reads <state-dir>/cert.pem and key.pem, listens on an ephemeral 127.0.0.1 port, prints the port
on its first stdout line, and serves both a self-hosted-shaped (chat completions) and an
Anthropic-shaped (messages) endpoint. Behaviour is steered by files in <state-dir> so the test
can change it between requests:

- credential.txt: the secret both header styles must carry, else 401;
- mode.txt: "ok" (default), "malformed" (200 with a body that is not JSON) or "unavailable" (503);
- reply.json: the proposal object returned in "ok" mode.

Every request appends one line (path, status) to requests.log. No body is ever logged.
"""
import json
import os
import ssl
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = sys.argv[1]
MODEL_TEXT_MARKER = "fixture-model-output-marker"


def read(name, default):
    try:
        with open(os.path.join(STATE, name), "r", encoding="utf-8") as handle:
            return handle.read().strip()
    except FileNotFoundError:
        return default


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("content-length", "0"))
        self.rfile.read(length)
        secret = read("credential.txt", "")
        presented = {
            self.headers.get("authorization", ""),
            "Bearer " + (self.headers.get("x-api-key") or ""),
        }
        if not secret or ("Bearer " + secret) not in presented:
            return self.reply(401, {"error": {"type": "authentication_error"}})
        mode = read("mode.txt", "ok")
        if mode == "unavailable":
            return self.reply(503, {"error": {"type": "overloaded_error"}})
        if mode == "malformed":
            return self.reply_raw(200, b"<<not json at all")
        proposal = json.loads(read("reply.json", '{"operation":{"kind":"annotate"}}'))
        if self.path.endswith("/messages"):
            return self.reply(200, {
                "id": "msg_fixture", "type": "message", "role": "assistant",
                "model": "fixture-anthropic-model",
                "content": [
                    {"type": "text", "text": MODEL_TEXT_MARKER},
                    {"type": "tool_use", "id": "toolu_fixture", "name": "interpret", "input": proposal},
                ],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 41, "output_tokens": 13},
            })
        return self.reply(200, {
            "id": "chatcmpl-fixture", "object": "chat.completion",
            "model": "fixture-self-hosted-model",
            "choices": [{"index": 0, "finish_reason": "stop",
                         "message": {"role": "assistant", "content": json.dumps(proposal)}}],
            "usage": {"prompt_tokens": 37, "completion_tokens": 11},
        })

    def reply(self, status, payload):
        self.reply_raw(status, json.dumps(payload).encode("utf-8"))

    def reply_raw(self, status, body):
        with open(os.path.join(STATE, "requests.log"), "a", encoding="utf-8") as log:
            log.write("%s %d\n" % (self.path, status))
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(os.path.join(STATE, "cert.pem"), os.path.join(STATE, "key.pem"))
server.socket = context.wrap_socket(server.socket, server_side=True)
print(server.server_address[1], flush=True)
server.serve_forever()
