"""Forward new posts of one Telegram channel to noti-mesh.

Run `login` once to create the session of a dedicated Telegram account, then run
without arguments. The account must already be a member of the channel.
"""

import asyncio
import json
import os
import sys
from pathlib import Path

from telethon import TelegramClient, events
from telethon.tl.types import PeerChannel

import telegram_forward

SYNC_INTERVAL_SECONDS = 300


def env(name, default=None):
    value = os.environ.get(name, default)
    if value is None or value == "":
        sys.exit(f"Set {name}.")
    return value


def client():
    return TelegramClient(
        env("TELEGRAM_SESSION", "/data/forwarder"),
        int(env("TELEGRAM_API_ID")),
        env("TELEGRAM_API_HASH"),
        catch_up=True,
    )


def read_state(path):
    try:
        return json.loads(path.read_text())["last_id"]
    except FileNotFoundError:
        return None


def write_state(path, last_id):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps({"last_id": last_id}))
    temporary.replace(path)


async def channel_entity(telegram, channel_id):
    # A new session cannot resolve a -100… channel until the dialog list is cached.
    await telegram.get_dialogs()
    return await telegram.get_entity(PeerChannel(channel_id))


async def login():
    telegram = client()
    await telegram.start()
    channel = await channel_entity(telegram, int(env("TELEGRAM_CHANNEL_ID")))
    print(f"Logged in. Channel found: {channel.title}")
    await telegram.disconnect()


class Forwarder:
    def __init__(self, telegram, channel):
        self.telegram = telegram
        self.channel = channel
        self.channel_id = int(env("TELEGRAM_CHANNEL_ID"))
        self.state = Path(env("TELEGRAM_STATE", "/data/state.json"))
        self.base_url = env("NOTI_URL", "http://app:8080")
        self.token = env("NOTI_FORWARDER_TOKEN")
        self.service = env("NOTI_SERVICE", "telegram-channel")
        self.environment = env("NOTI_ENVIRONMENT", "mainnet")
        self.lock = asyncio.Lock()

    async def start_position(self):
        last_id = read_state(self.state)
        if last_id is None:
            # The first run starts after the newest post instead of resending the history.
            latest = await self.telegram.get_messages(self.channel, limit=1)
            last_id = latest[0].id if latest else 0
            write_state(self.state, last_id)
            print(f"Starting after post {last_id}", flush=True)
        return last_id

    async def sync(self):
        async with self.lock:
            last_id = read_state(self.state) or 0
            async for post in self.telegram.iter_messages(self.channel, min_id=last_id, reverse=True):
                message = telegram_forward.to_message(self.channel_id, post.id, post.message, self.service, self.environment)
                if message is not None:
                    try:
                        status = await asyncio.to_thread(telegram_forward.post, self.base_url, self.token, message)
                    except OSError as error:
                        print(f"noti-mesh unreachable at post {post.id}: {error}; retrying later", flush=True)
                        return
                    if status in (401, 403):
                        print(f"noti-mesh rejected the token (HTTP {status}); check NOTI_FORWARDER_TOKEN", flush=True)
                        return
                    if status >= 500 or status == 429:
                        print(f"noti-mesh returned HTTP {status} at post {post.id}; retrying later", flush=True)
                        return
                    if status >= 400:
                        print(f"noti-mesh refused post {post.id} with HTTP {status}; skipping it", flush=True)
                    else:
                        print(f"Forwarded post {post.id} as {message['level']}", flush=True)
                write_state(self.state, post.id)

    async def periodic_sync(self):
        while True:
            await asyncio.sleep(SYNC_INTERVAL_SECONDS)
            await self.sync()


async def run():
    telegram = client()
    await telegram.connect()
    if not await telegram.is_user_authorized():
        sys.exit("The Telegram session is not logged in. Run the login command first.")
    channel = await channel_entity(telegram, int(env("TELEGRAM_CHANNEL_ID")))
    forwarder = Forwarder(telegram, channel)
    await forwarder.start_position()
    await forwarder.sync()

    @telegram.on(events.NewMessage(chats=channel))
    async def on_post(_event):
        await forwarder.sync()

    print(f"Watching {channel.title}", flush=True)
    sync_task = asyncio.create_task(forwarder.periodic_sync())
    try:
        await telegram.run_until_disconnected()
    finally:
        sync_task.cancel()


if __name__ == "__main__":
    asyncio.run(login() if sys.argv[1:] == ["login"] else run())
