import json
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, HTTPServer


class SlackMock(BaseHTTPRequestHandler):
    failures_remaining = 0
    failure_error = None
    # Scenario checks read recorded requests back through /__dev/requests.
    recorded = []
    @classmethod
    def record(cls, path, body):
        cls.recorded.append({"path": path, "body": body})
        del cls.recorded[:-500]

    def respond(self, status, payload):
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(payload).encode())

    def do_GET(self):
        path, _, query = self.path.partition("?")
        if path == "/health":
            self.respond(200, {"development_mock": True})
        elif path == "/__dev/requests":
            self.respond(200, {"requests": SlackMock.recorded})
        elif path == "/api/auth.test":
            self.respond(200, {"ok": True, "team_id": "TDEV", "development_mock": True})
        elif path == "/api/conversations.info":
            channel = urllib.parse.parse_qs(query).get("channel", [""])[0]
            self.respond(200, {
                "ok": True,
                "development_mock": True,
                "channel": {"id": channel, "is_member": True, "is_archived": False},
            })
        else:
            self.respond(404, {"ok": False, "error": "unknown_mock_endpoint"})

    def do_POST(self):
        if self.path == "/__dev/fail-next":
            body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
            try:
                error = json.loads(body or b"{}").get("error")
            except ValueError:
                error = None
            SlackMock.failures_remaining = 1
            SlackMock.failure_error = error if isinstance(error, str) else None
            print("[DEV MOCK] The next Slack request will fail", flush=True)
            self.respond(200, {"development_mock": True})
            return
        if self.path == "/__dev/reset":
            self.rfile.read(int(self.headers.get("Content-Length", "0")))
            SlackMock.recorded.clear()
            self.respond(200, {"development_mock": True})
            return
        if self.path.startswith("/response"):
            length = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(length) if 0 < length <= 1_048_576 else b"{}"
            try:
                SlackMock.record(self.path, json.loads(body))
            except ValueError:
                SlackMock.record(self.path, None)
            print(f"[DEV MOCK] {self.path}: action notice accepted locally", flush=True)
            self.respond(200, {"ok": True, "development_mock": True})
            return
        if self.path not in ("/api/chat.postMessage", "/api/chat.update"):
            self.respond(404, {"ok": False, "error": "unknown_mock_endpoint"})
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 1_048_576:
                self.respond(413, {"ok": False, "error": "invalid_mock_payload_size"})
                return
            payload = json.loads(self.rfile.read(length))
            if not isinstance(payload, dict) or not payload.get("channel"):
                self.respond(400, {"ok": False, "error": "missing_channel"})
                return
        except (ValueError, UnicodeDecodeError):
            self.respond(400, {"ok": False, "error": "invalid_json"})
            return
        SlackMock.record(self.path, payload)
        if SlackMock.failures_remaining:
            SlackMock.failures_remaining -= 1
            print("[DEV MOCK] Simulated Slack delivery failure", flush=True)
            if SlackMock.failure_error:
                self.respond(200, {"ok": False, "error": SlackMock.failure_error})
            else:
                self.respond(503, {"ok": False, "error": "development_failure"})
            return
        print(f"[DEV MOCK] {self.path}: accepted locally; no Slack message sent", flush=True)
        self.respond(200, {
            "ok": True,
            "development_mock": True,
            "channel": payload["channel"],
            "ts": payload.get("ts") or f"{time.time():.6f}",
        })

    def log_message(self, *_args):
        pass


if __name__ == "__main__":
    print("[DEV MOCK] Local Slack transport on :8090; external delivery disabled", flush=True)
    HTTPServer(("0.0.0.0", 8090), SlackMock).serve_forever()
