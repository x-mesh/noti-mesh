import json
import socketserver
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class SMTP(socketserver.StreamRequestHandler):
    def handle(self):
        self.wfile.write(b"220 localhost ESMTP\r\n")
        in_data = False
        for line in self.rfile:
            if in_data:
                if line == b".\r\n":
                    in_data = False
                    print("[DEV MOCK] Email accepted locally; no email sent", flush=True)
                    self.wfile.write(b"250 queued\r\n")
                continue
            command = line.upper()
            if command.startswith(b"EHLO"):
                self.wfile.write(b"250-localhost\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n")
            elif command.startswith(b"DATA"):
                in_data = True
                self.wfile.write(b"354 end with dot\r\n")
            elif command.startswith(b"QUIT"):
                self.wfile.write(b"221 bye\r\n")
                return
            else:
                self.wfile.write(b"250 ok\r\n")


class Telegram(BaseHTTPRequestHandler):
    def do_POST(self):
        if self.path != "/bot123:development/sendMessage":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 1_048_576:
                self.send_error(413)
                return
            payload = json.loads(self.rfile.read(length))
            if not isinstance(payload, dict) or not payload.get("chat_id") or not payload.get("text"):
                self.send_error(400)
                return
        except (ValueError, UnicodeDecodeError):
            self.send_error(400)
            return
        data = json.dumps({"ok": True, "development_mock": True, "result": {"message_id": 1}}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        print("[DEV MOCK] Telegram accepted locally; no Telegram message sent", flush=True)

    def log_message(self, *_args):
        pass


class SMTPServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True


if __name__ == "__main__":
    smtp = SMTPServer(("127.0.0.1", 1025), SMTP)
    telegram = ThreadingHTTPServer(("127.0.0.1", 8091), Telegram)
    threading.Thread(target=smtp.serve_forever, daemon=True).start()
    print("[DEV MOCK] Local email and Telegram transports; external delivery disabled", flush=True)
    telegram.serve_forever()
