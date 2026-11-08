import importlib.util
import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

spec = importlib.util.spec_from_file_location("watchdog", Path(__file__).parents[1] / "scripts" / "watchdog.py")
watchdog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(watchdog)


class Handler(BaseHTTPRequestHandler):
    healthy = True
    webhook_ok = True
    posts = []

    def do_GET(self):
        self.send_response(200 if self.healthy else 503)
        self.end_headers()

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.webhook_ok:
            self.posts.append(body)
        self.send_response(200 if self.webhook_ok else 503)
        self.end_headers()

    def log_message(self, *_args):
        pass


class WatchdogTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()
        cls.base = f"http://127.0.0.1:{cls.server.server_port}"

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls.thread.join()

    def setUp(self):
        Handler.healthy = True
        Handler.webhook_ok = True
        Handler.posts = []

    def test_failure_recovery_and_notification_retry(self):
        healthy, state = watchdog.step(self.base + "/health", self.base + "/hook", "test", 1, None)
        self.assertTrue(healthy)
        self.assertTrue(state)
        self.assertEqual(Handler.posts, [])
        Handler.healthy = False
        Handler.webhook_ok = False
        healthy, state = watchdog.step(self.base + "/health", self.base + "/hook", "test", 1, state)
        self.assertFalse(healthy)
        self.assertTrue(state)
        Handler.webhook_ok = True
        healthy, state = watchdog.step(self.base + "/health", self.base + "/hook", "test", 1, state)
        self.assertFalse(healthy)
        self.assertFalse(state)
        self.assertEqual(len(Handler.posts), 1)
        _, state = watchdog.step(self.base + "/health", self.base + "/hook", "test", 1, state)
        self.assertEqual(len(Handler.posts), 1)
        Handler.healthy = True
        healthy, state = watchdog.step(self.base + "/health", self.base + "/hook", "test", 1, state)
        self.assertTrue(healthy)
        self.assertTrue(state)
        self.assertEqual(len(Handler.posts), 2)
        self.assertEqual(Handler.posts[0]["text"], "test: health check failed")
        self.assertEqual(Handler.posts[1]["text"], "test: health check recovered")


if __name__ == "__main__":
    unittest.main()
