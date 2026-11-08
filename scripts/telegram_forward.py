"""Convert Telegram channel posts into noti-mesh ordinary messages.

This module uses only the standard library so that the mapping can be tested
without a Telegram account. scripts/telegram_forwarder.py receives the posts.
"""

import json
import re
import urllib.error
import urllib.request

TITLE_LIMIT = 200
BODY_LIMIT = 2000

# Keep crit narrow: announcements often mention "security patch" for routine upgrades.
CRIT = re.compile(r"\b(halt|halted|emergency|exploit|critical)\b", re.IGNORECASE)
WARN = re.compile(r"action required|\bupgrade|⚠️", re.IGNORECASE)


def level_for(text):
    if CRIT.search(text):
        return "crit"
    if WARN.search(text):
        return "warn"
    return "info"


def post_link(channel_id, message_id):
    return f"https://t.me/c/{channel_id}/{message_id}"


def _clip(text, limit):
    return text if len(text) <= limit else text[: limit - 1].rstrip() + "…"


def to_message(channel_id, message_id, text, service, environment):
    """Return the /v1/messages payload, or None for a post without text."""
    lines = [line.strip() for line in (text or "").splitlines()]
    lines = [line for line in lines if line]
    if not lines:
        return None
    link = post_link(channel_id, message_id)
    rest = "\n".join(lines[1:])
    # The link stays even when the body is cut, so readers can open the full post.
    suffix = f"\n\n원문: {link}" if rest else f"원문: {link}"
    body = _clip(rest, BODY_LIMIT - len(suffix)) + suffix
    return {
        "event_id": f"tg-{channel_id}-{message_id}",
        "service": service,
        "environment": environment,
        "title": _clip(lines[0], TITLE_LIMIT),
        "body": body,
        "level": level_for(text),
    }


def post(base_url, token, message, timeout=10):
    """Send one message. Return the HTTP status; raise on network failure."""
    request = urllib.request.Request(
        base_url.rstrip("/") + "/v1/messages",
        data=json.dumps(message).encode(),
        headers={"Content-Type": "application/json", "Authorization": f"Bearer {token}"},
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code
