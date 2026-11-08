import asyncio
import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "scripts"))

HAS_TELETHON = importlib.util.find_spec("telethon") is not None


class FakeTelegram:
    def __init__(self, posts):
        self.posts = posts

    async def iter_messages(self, _channel, min_id=0, reverse=False):
        for post in sorted(self.posts, key=lambda p: p.id, reverse=not reverse):
            if post.id > min_id:
                yield post

    async def get_messages(self, _channel, limit=1):
        return sorted(self.posts, key=lambda p: p.id, reverse=True)[:limit]


@unittest.skipUnless(HAS_TELETHON, "telethon is installed only in the forwarder image")
class SyncTest(unittest.TestCase):
    def setUp(self):
        import telegram_forwarder

        self.module = telegram_forwarder
        self.directory = tempfile.TemporaryDirectory()
        self.state = Path(self.directory.name) / "state.json"
        self.env = mock.patch.dict(os.environ, {
            "TELEGRAM_CHANNEL_ID": "1234567890",
            "TELEGRAM_STATE": str(self.state),
            "NOTI_FORWARDER_TOKEN": "token",
        })
        self.env.start()
        self.posts = [SimpleNamespace(id=n, message=f"Post {n}\nbody") for n in (10, 11, 12, 13)]

    def tearDown(self):
        self.env.stop()
        self.directory.cleanup()

    def run_sync(self, statuses, start=None):
        forwarder = self.module.Forwarder(FakeTelegram(self.posts), object())
        if start is not None:
            self.module.write_state(self.state, start)
        sent = []

        def post(_url, _token, message):
            sent.append(message["event_id"])
            status = statuses.pop(0)
            if isinstance(status, Exception):
                raise status
            return status

        with mock.patch.object(self.module.telegram_forward, "post", side_effect=post):
            asyncio.run(forwarder.sync())
        return sent, self.module.read_state(self.state)

    def test_first_run_starts_after_the_newest_post(self):
        forwarder = self.module.Forwarder(FakeTelegram(self.posts), object())
        self.assertEqual(asyncio.run(forwarder.start_position()), 13)
        self.assertEqual(self.module.read_state(self.state), 13)

    def test_forwards_in_order_and_advances(self):
        sent, last = self.run_sync([202, 202], start=11)
        self.assertEqual(sent, ["tg-1234567890-12", "tg-1234567890-13"])
        self.assertEqual(last, 13)

    def test_server_error_stops_without_losing_posts(self):
        sent, last = self.run_sync([202, 503], start=10)
        self.assertEqual(sent, ["tg-1234567890-11", "tg-1234567890-12"])
        self.assertEqual(last, 11)

    def test_network_error_stops_without_losing_posts(self):
        sent, last = self.run_sync([OSError("refused")], start=12)
        self.assertEqual(sent, ["tg-1234567890-13"])
        self.assertEqual(last, 12)

    def test_rejected_token_stops(self):
        _, last = self.run_sync([401], start=12)
        self.assertEqual(last, 12)

    def test_invalid_post_is_skipped(self):
        sent, last = self.run_sync([422, 202], start=11)
        self.assertEqual(len(sent), 2)
        self.assertEqual(last, 13)


if __name__ == "__main__":
    unittest.main()
