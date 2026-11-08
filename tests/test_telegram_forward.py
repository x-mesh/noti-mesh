import json
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))

import telegram_forward  # noqa: E402

ANNOUNCEMENT = """📢 [Example Chain v2.3 Upgrade Completed – Action Required]

Hello Validators and Node Operators,

The Example Chain v2.3 upgrade is complete, and the network is producing new blocks.

⚠️ Action Required

Validators and node operators who have not upgraded yet must update their nodes to v2.3.0 as soon as possible.

Example Chain Team"""


class LevelTest(unittest.TestCase):
    def test_sample_announcement_is_warn_not_crit(self):
        self.assertEqual(telegram_forward.level_for(ANNOUNCEMENT), "warn")

    def test_crit_words(self):
        for text in ["Chain HALTED at height 100", "Emergency upgrade", "exploit found", "critical bug"]:
            self.assertEqual(telegram_forward.level_for(text), "crit", text)

    def test_info_by_default(self):
        self.assertEqual(telegram_forward.level_for("Weekly validator call notes"), "info")
        self.assertEqual(telegram_forward.level_for("Halting problem lecture"), "info")


class MessageTest(unittest.TestCase):
    def test_maps_first_line_to_title_and_links_the_post(self):
        message = telegram_forward.to_message(1234567890, 85, ANNOUNCEMENT, "example-validator", "mainnet")
        self.assertEqual(message["event_id"], "tg-1234567890-85")
        self.assertEqual(message["title"], "📢 [Example Chain v2.3 Upgrade Completed – Action Required]")
        self.assertTrue(message["body"].startswith("Hello Validators and Node Operators,"))
        self.assertTrue(message["body"].endswith("원문: https://t.me/c/1234567890/85"))
        self.assertEqual(message["level"], "warn")
        self.assertEqual((message["service"], message["environment"]), ("example-validator", "mainnet"))

    def test_long_post_is_clipped_within_api_limits(self):
        text = "T" * 300 + "\n" + "b" * 5000
        message = telegram_forward.to_message(1, 2, text, "s", "e")
        self.assertLessEqual(len(message["title"]), 200)
        self.assertLessEqual(len(message["body"]), 2000)
        self.assertTrue(message["body"].endswith("원문: https://t.me/c/1/2"))
        self.assertIn("…", message["body"])

    def test_title_only_post_still_has_a_body(self):
        message = telegram_forward.to_message(1, 3, "Upgrade done", "s", "e")
        self.assertEqual(message["body"], "원문: https://t.me/c/1/3")

    def test_post_without_text_is_skipped(self):
        self.assertIsNone(telegram_forward.to_message(1, 4, "", "s", "e"))
        self.assertIsNone(telegram_forward.to_message(1, 5, None, "s", "e"))
        self.assertIsNone(telegram_forward.to_message(1, 6, " \n \n", "s", "e"))


class PostTest(unittest.TestCase):
    def test_posts_json_with_bearer_token(self):
        received = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                received.append((self.path, self.headers["Authorization"], json.loads(self.rfile.read(int(self.headers["Content-Length"])))))
                self.send_response(202)
                self.end_headers()

            def log_message(self, *_args):
                pass

        server = HTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.handle_request, daemon=True).start()
        message = telegram_forward.to_message(1, 7, "Upgrade done", "s", "e")
        status = telegram_forward.post(f"http://127.0.0.1:{server.server_port}/", "token-value", message)
        server.server_close()
        self.assertEqual(status, 202)
        self.assertEqual(received[0][:2], ("/v1/messages", "Bearer token-value"))
        self.assertEqual(received[0][2]["event_id"], "tg-1-7")


if __name__ == "__main__":
    unittest.main()
