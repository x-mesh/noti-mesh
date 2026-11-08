"""End-to-end scenarios against the local mock stack (`make dev-mock`).

The script sends events and signed Slack button actions over HTTP, then checks the
admin API, the health endpoints, and the requests recorded by the Slack mock.
It never contacts real Slack, email, or Telegram.
"""

import argparse
import hashlib
import hmac
import json
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

SOURCE_TOKEN = "dev-source-token-not-for-production-0001"
SIGNING_SECRET = "development-only"
WORKSPACE = "TDEV"
OPERATOR = "UDEVOPERATOR"
BACKUP = "UDEVBACKUP"
OUTSIDER = "UOUTSIDER"
GUIDE = "재알림을 멈추려면 담당하기를 누르세요"


class Runner:
    def __init__(self, base, compose):
        self.base = base.rstrip("/")
        self.compose = compose
        self.run_id = uuid.uuid4().hex[:6]
        self.results = []

    def http(self, method, path, body=None, headers=None, raw=None):
        data = raw if raw is not None else (json.dumps(body).encode() if body is not None else None)
        request = urllib.request.Request(self.base + path, data=data, method=method)
        if body is not None and raw is None:
            request.add_header("Content-Type", "application/json")
        for key, value in (headers or {}).items():
            request.add_header(key, value)
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                text = response.read().decode()
                status = response.status
        except urllib.error.HTTPError as error:
            text = error.read().decode()
            status = error.code
        try:
            return status, json.loads(text) if text else None
        except ValueError:
            return status, text

    def event(self, key, sequence, severity, recovered=False):
        body = {
            "event_id": f"{key}-{sequence}",
            "service": "checkout",
            "environment": "development",
            "incident_key": key,
            "sequence": sequence,
            "severity": severity,
            "recovered": recovered,
            "occurred_at": "2026-11-07T00:00:00Z",
            "summary": f"scenario {key}",
        }
        return self.http("POST", "/v1/events", body, {"Authorization": f"Bearer {SOURCE_TOKEN}"})

    def message(self, event_id, title):
        body = {"event_id": event_id, "service": "checkout", "environment": "development", "title": title, "body": "scenario message"}
        return self.http("POST", "/v1/messages", body, {"Authorization": f"Bearer {SOURCE_TOKEN}"})

    def press(self, actor, action, incident, notice):
        payload = {
            "team": {"id": WORKSPACE},
            "user": {"id": actor, "name": actor},
            "actions": [{"action_id": action, "value": incident, "action_ts": f"{time.time():.6f}"}],
            "channel": {"id": "CDEVELOPMENT"},
            "message": {"ts": "0"},
            "response_url": f"http://slack:8090/response/{notice}",
        }
        body = urllib.parse.urlencode({"payload": json.dumps(payload)}).encode()
        timestamp = str(int(time.time()))
        digest = hmac.new(SIGNING_SECRET.encode(), b"v0:" + timestamp.encode() + b":" + body, hashlib.sha256).hexdigest()
        headers = {
            "Content-Type": "application/x-www-form-urlencoded",
            "X-Slack-Request-Timestamp": timestamp,
            "X-Slack-Signature": f"v0={digest}",
        }
        return self.http("POST", "/v1/slack/actions", headers=headers, raw=body)[0]

    def detail(self, incident):
        return self.http("GET", f"/v1/admin/incidents/{incident}")[1]

    def slack(self, path, body=None):
        script = (
            "import json,sys,urllib.request;"
            f"r=urllib.request.Request('http://127.0.0.1:8090{path}',data={body!r},method={'POST' if body is not None else 'GET'!r});"
            "sys.stdout.write(urllib.request.urlopen(r).read().decode())"
        )
        output = subprocess.run(
            [*self.compose, "exec", "-T", "slack", "python", "-c", script],
            check=True, capture_output=True, text=True,
        ).stdout
        return json.loads(output)

    def slack_requests(self):
        return self.slack("/__dev/requests")["requests"]

    def posts_for(self, needle, path="/api/chat.postMessage"):
        return [r["body"] for r in self.slack_requests() if r["path"] == path and needle in json.dumps(r["body"], ensure_ascii=False)]

    def notice(self, name):
        found = [r["body"] for r in self.slack_requests() if r["path"] == f"/response/{name}"]
        return found[-1] if found else None

    def wait(self, probe, timeout=15):
        deadline = time.time() + timeout
        while True:
            value = probe()
            if value or time.time() > deadline:
                return value
            time.sleep(0.2)

    def wait_quiet(self, seconds=2.0, timeout=30):
        deadline = time.time() + timeout
        last = len(self.slack_requests())
        stable_since = time.time()
        while time.time() < deadline:
            time.sleep(0.3)
            count = len(self.slack_requests())
            if count != last:
                last, stable_since = count, time.time()
            elif time.time() - stable_since >= seconds:
                return

    def check(self, scenario, name, ok, detail=""):
        self.results.append((scenario, name, bool(ok), detail))
        mark = "PASS" if ok else "FAIL"
        print(f"  [{mark}] {name}" + (f" — {detail}" if detail and not ok else ""), flush=True)

    def new_incident(self, label, severity=2):
        key = f"{label}-{self.run_id}"
        status, body = self.event(key, 1, severity)
        if status != 202:
            raise RuntimeError(f"event for {key} returned {status}: {body}")
        return key, body["incident_id"]


def sent_delivery(runner, incident):
    return [d for d in runner.detail(incident)["deliveries"] if d.get("sent_at")]


def scenario_health(r, label):
    print(f"\n{label}")
    r.check(label, "/health returns 200", r.http("GET", "/health")[0] == 200)
    status, body = r.http("GET", "/health/delivery")
    r.check(label, "/health/delivery returns 200 with no problems", status == 200 and body["problems"] == [], json.dumps(body, ensure_ascii=False))


def scenario_lifecycle(r):
    label = "S1 사건 기본 흐름"
    print(f"\n{label}")
    key, incident = r.new_incident("s1")
    posts = r.wait(lambda: r.posts_for(incident))
    r.check(label, "P2 사건이 Slack에 게시된다", posts)
    post = posts[0] if posts else {}
    r.check(label, "게시 메시지가 담당 그룹을 멘션한다", f"<@{OPERATOR}>" in post.get("text", ""), post.get("text", ""))
    r.check(label, "버튼 의미 설명이 붙는다", GUIDE in json.dumps(post, ensure_ascii=False))
    pressed = r.press(OPERATOR, "ack", incident, f"s1-ack-{r.run_id}")
    if pressed == 401:
        sys.exit("Slack signature rejected: start the stack without .env so it uses the development signing secret.")
    r.check(label, "담당자의 담당하기가 200을 받는다", pressed == 200)
    state = r.wait(lambda: r.detail(incident)["incident"]["state"] == "acknowledged")
    r.check(label, "사건이 담당 상태가 되고 담당자가 기록된다", state and r.detail(incident)["incident"]["owner"] == OPERATOR)
    status, _ = r.event(key, 2, 2, recovered=True)
    r.check(label, "복구 이벤트가 202를 받는다", status == 202)
    resolved = r.wait(lambda: r.detail(incident)["incident"]["state"] == "resolved")
    r.check(label, "사건이 해결 상태가 된다", resolved)
    # A resolved message has no buttons, so it carries the incident key but not the incident ID.
    updated = r.wait(lambda: [b for b in r.posts_for(key, "/api/chat.update") if "[resolved]" in b.get("text", "")])
    r.check(label, "Slack 원본 메시지가 해결 상태로 갱신된다", updated)


def scenario_rejection(r):
    label = "S2 거부된 버튼 안내"
    print(f"\n{label}")
    _, incident = r.new_incident("s2")
    r.wait(lambda: r.posts_for(incident))
    outsider = f"s2-outsider-{r.run_id}"
    r.check(label, "등록되지 않은 사용자의 담당하기는 403", r.press(OUTSIDER, "ack", incident, outsider) == 403)
    notice = r.wait(lambda: r.notice(outsider))
    r.check(label, "누른 사람에게만 보이는 거부 사유가 전송된다", notice and notice["response_type"] == "ephemeral" and "등록되지 않은" in notice["text"], json.dumps(notice, ensure_ascii=False))
    r.check(label, "담당자의 담당하기는 200", r.press(OPERATOR, "ack", incident, f"s2-operator-{r.run_id}") == 200)
    late = f"s2-backup-{r.run_id}"
    r.check(label, "백업 그룹 사용자의 늦은 담당하기는 200", r.press(BACKUP, "ack", incident, late) == 200)
    notice = r.wait(lambda: r.notice(late))
    expected = f"이미 <@{OPERATOR}> 님이 담당하고 있습니다."
    r.check(label, "늦게 누른 사람에게 현재 담당자를 알린다", notice and notice["text"] == expected, json.dumps(notice, ensure_ascii=False))
    r.check(label, "담당자는 바뀌지 않는다", r.detail(incident)["incident"]["owner"] == OPERATOR)


def scenario_rise(r):
    label = "S3 긴급도 상승 재멘션"
    print(f"\n{label}")
    key, incident = r.new_incident("s3")
    r.wait(lambda: r.posts_for(incident))
    r.press(OPERATOR, "ack", incident, f"s3-ack-{r.run_id}")
    r.wait(lambda: r.detail(incident)["incident"]["state"] == "acknowledged")
    r.wait_quiet()
    status, _ = r.event(key, 2, 1)
    r.check(label, "P1 상승 이벤트가 202를 받는다", status == 202)
    detail = r.detail(incident)["incident"]
    r.check(label, "사건이 다시 열리고 담당자가 해제된다", detail["state"] == "open" and detail["owner"] is None and detail["severity"] == 1)
    posts = r.wait(lambda: (lambda p: p if len(p) >= 2 else None)(r.posts_for(incident)))
    r.check(label, "새 메시지가 게시된다(수정이 아님)", posts and len(posts) == 2, f"postMessage {len(posts or [])}건")
    newest = posts[-1] if posts else {}
    r.check(label, "새 메시지가 P1으로 담당 그룹을 다시 멘션한다", f"<@{OPERATOR}> P1 " in newest.get("text", ""), newest.get("text", ""))


def scenario_message_duplicates(r):
    label = "S4 일반 메시지 중복 묶음"
    print(f"\n{label}")
    title = f"scenario report {r.run_id}"
    first_status, first = r.message(f"s4-a-{r.run_id}", title)
    r.check(label, "첫 메시지가 202를 받는다", first_status == 202, str(first))
    message_id = first["message_id"]
    r.wait(lambda: r.posts_for(message_id))
    second_status, second = r.message(f"s4-b-{r.run_id}", title)
    r.check(label, "같은 내용의 두 번째 메시지는 같은 메시지로 묶인다", second_status == 200 and second["message_id"] == message_id, str(second))
    updated = r.wait(lambda: [b for b in r.posts_for(message_id, "/api/chat.update") if "수신 2회" in b.get("text", "")])
    r.check(label, "Slack 메시지가 수신 2회로 갱신된다", updated)
    r.check(label, "Slack 게시는 한 번뿐이다", len(r.posts_for(message_id)) == 1)


def scenario_throughput(r, count):
    label = f"S5 P1 {count}건 동시 발생 처리 시간"
    print(f"\n{label}")
    keys = [f"s5-{i}-{r.run_id}" for i in range(count)]
    incidents = {}
    started = time.time()

    def send(key):
        status, body = r.event(key, 1, 1)
        if status == 202:
            incidents[key] = body["incident_id"]

    threads = [threading.Thread(target=send, args=(key,)) for key in keys]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    r.check(label, f"{count}건 모두 접수된다", len(incidents) == count)

    def all_posted():
        posted = {json.dumps(r["body"]) for r in r.slack_requests() if r["path"] == "/api/chat.postMessage"}
        return all(any(i in p for p in posted) for i in incidents.values())

    done = r.wait(all_posted, timeout=60)
    elapsed = time.time() - started
    r.check(label, f"{count}건 첫 알림이 모두 게시된다", done)
    r.check(label, f"모두 게시되기까지 {elapsed:.1f}초 (기준 {count / 2:.0f}초 미만)", done and elapsed < count / 2)


def scenario_transient_failure(r):
    label = "S6 Slack 일시 장애와 대체 발송"
    print(f"\n{label}")
    r.wait_quiet()
    r.slack("/__dev/fail-next", b"{}")
    _, incident = r.new_incident("s6")
    failed = r.wait(lambda: [d for d in r.detail(incident)["deliveries"] if d["attempts"] >= 1 and d.get("last_error")])
    r.check(label, "첫 Slack 발송이 실패로 기록되고 재시도를 기다린다", failed and failed[0]["failed_at"] is None and failed[0]["sent_at"] is None, json.dumps(failed, ensure_ascii=False)[:300])
    channels = r.wait(lambda: (lambda c: c if len(c) >= 2 else None)([c for c in r.detail(incident)["channels"] if c["state"] == "sent"]))
    r.check(label, "메일과 Telegram 대체 발송이 나간다", channels and {c["channel"] for c in channels} == {"email", "telegram"}, json.dumps(r.detail(incident)["channels"], ensure_ascii=False)[:300])
    status, body = r.http("GET", "/health/delivery")
    r.check(label, "재시도 1회는 알림 경로 장애로 보지 않는다", status == 200, json.dumps(body, ensure_ascii=False))


def scenario_permanent_failure(r):
    label = "S7 Slack 영구 실패, 상태 확인, 관리자 재시도"
    print(f"\n{label}")
    r.wait_quiet()
    r.slack("/__dev/fail-next", json.dumps({"error": "is_archived"}).encode())
    _, incident = r.new_incident("s7")
    failed = r.wait(lambda: [d for d in r.detail(incident)["deliveries"] if d.get("failed_at")])
    r.check(label, "보관된 채널 오류는 즉시 영구 실패가 된다", failed and failed[0]["attempts"] == 1, json.dumps(failed, ensure_ascii=False)[:300])
    status, body = r.http("GET", "/health/delivery")
    r.check(label, "/health/delivery가 503과 영구 실패 사유를 반환한다", status == 503 and any("failed permanently" in p for p in body["problems"]), json.dumps(body, ensure_ascii=False))
    channels = r.wait(lambda: (lambda c: c if len(c) >= 2 else None)([c for c in r.detail(incident)["channels"] if c["state"] == "sent"]))
    r.check(label, "영구 실패에도 대체 발송이 나간다", channels)
    if failed:
        retry_status, _ = r.http("POST", f"/v1/admin/slack-deliveries/{failed[0]['id']}/retry")
        r.check(label, "관리자 재시도가 성공 응답(204)을 받는다", retry_status == 204, str(retry_status))
        sent = r.wait(lambda: [d for d in r.detail(incident)["deliveries"] if d["id"] == failed[0]["id"] and d.get("sent_at")])
        r.check(label, "재시도한 발송이 성공한다", sent)
    status, body = r.http("GET", "/health/delivery")
    r.check(label, "재시도 성공 후 /health/delivery가 200으로 돌아온다", status == 200, json.dumps(body, ensure_ascii=False))


def scenario_healthcheck(r):
    label = "S8 컨테이너 상태 확인 명령"
    print(f"\n{label}")
    result = subprocess.run([*r.compose, "exec", "-T", "app", "noti-mesh", "healthcheck"], capture_output=True, text=True)
    r.check(label, "noti-mesh healthcheck가 종료 코드 0을 반환한다", result.returncode == 0, result.stderr.strip())


def main():
    parser = argparse.ArgumentParser(description="Run end-to-end scenarios against the local mock stack")
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--project", default="noti-mesh-dev", help="Docker Compose project name of the mock stack")
    parser.add_argument("--burst", type=int, default=15, help="P1 incidents sent at once in the throughput scenario")
    args = parser.parse_args()
    compose = ["docker", "compose", "--project-name", args.project, "--file", "compose.dev.yaml"]
    runner = Runner(args.base_url, compose)
    deadline = time.time() + 120
    while True:
        try:
            if runner.http("GET", "/health")[0] == 200:
                break
        except OSError:
            pass
        if time.time() > deadline:
            sys.exit("The stack did not become healthy within two minutes.")
        time.sleep(1)
    status, mode = runner.http("GET", "/v1/admin-mode")
    if status != 200 or not mode.get("development"):
        sys.exit("The target is not a development stack; scenarios run only against `make dev-mock`.")
    print(f"run {runner.run_id} → {args.base_url}")
    scenario_health(runner, "S0 시작 상태")
    scenario_lifecycle(runner)
    scenario_rejection(runner)
    scenario_rise(runner)
    scenario_message_duplicates(runner)
    scenario_throughput(runner, args.burst)
    scenario_transient_failure(runner)
    scenario_permanent_failure(runner)
    scenario_healthcheck(runner)
    failed = [r for r in runner.results if not r[2]]
    print(f"\n{len(runner.results) - len(failed)}/{len(runner.results)} checks passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
