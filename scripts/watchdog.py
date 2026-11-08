import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request


def check_health(url, timeout):
    try:
        with urllib.request.urlopen(url, timeout=timeout) as response:
            return 200 <= response.status < 300
    except (urllib.error.URLError, TimeoutError, OSError):
        return False


def send_webhook(url, text, timeout):
    request = urllib.request.Request(
        url,
        data=json.dumps({"text": text}).encode(),
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return 200 <= response.status < 300
    except (urllib.error.URLError, TimeoutError, OSError):
        return False


def step(health_url, webhook_url, label, timeout, reported):
    healthy = check_health(health_url, timeout)
    if healthy and reported is None:
        return True, True
    if healthy != reported:
        text = f"{label}: {'health check recovered' if healthy else 'health check failed'}"
        if send_webhook(webhook_url, text, timeout):
            return healthy, healthy
        print("watchdog notification failed; it will retry", file=sys.stderr, flush=True)
    return healthy, reported


def main():
    parser = argparse.ArgumentParser(description="Monitor noti-mesh from an independent host")
    parser.add_argument("--health-url", required=True)
    parser.add_argument("--webhook-env", default="NOTI_WATCHDOG_WEBHOOK")
    parser.add_argument("--label", default="noti-mesh")
    parser.add_argument("--interval", type=float, default=30)
    parser.add_argument("--timeout", type=float, default=5)
    parser.add_argument("--once", action="store_true")
    args = parser.parse_args()
    if args.interval <= 0 or args.timeout <= 0:
        parser.error("interval and timeout must be positive")
    health = urllib.parse.urlsplit(args.health_url)
    if health.scheme not in ("http", "https") or not health.hostname or health.username or health.password or health.query or health.fragment:
        parser.error("health URL must be HTTP(S) without credentials, query, or fragment")
    webhook_url = os.environ.get(args.webhook_env, "")
    webhook = urllib.parse.urlsplit(webhook_url)
    local = webhook.hostname in ("127.0.0.1", "localhost", "::1")
    if not webhook.hostname or webhook.username or webhook.password or webhook.fragment or (webhook.scheme != "https" and not (local and webhook.scheme == "http")):
        parser.error("webhook environment variable must contain an HTTPS URL")
    reported = None
    try:
        while True:
            healthy, reported = step(args.health_url, webhook_url, args.label, args.timeout, reported)
            if args.once:
                return 0 if healthy else 1
            time.sleep(args.interval)
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
