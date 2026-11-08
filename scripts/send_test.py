import json
import os
import shlex
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path


def settings():
    values = {}
    path = Path('.env')
    if path.exists():
        for line in path.read_text(encoding='utf-8').splitlines():
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            if line.startswith('export '):
                line = line[7:].lstrip()
            key, separator, raw = line.partition('=')
            key = key.strip()
            if separator and key in ('NOTI_CLIENT_TOKEN', 'NOTI_CLIENT_SERVICE'):
                parts = shlex.split(raw, comments=True)
                if len(parts) > 1:
                    raise ValueError(f'{key}: invalid value in .env')
                values[key] = parts[0] if parts else ''
    return {key: os.environ.get(key, values.get(key, '')) for key in ('NOTI_CLIENT_TOKEN', 'NOTI_CLIENT_SERVICE')}


def request(url, token, payload=None):
    body = json.dumps(payload, ensure_ascii=False).encode() if payload else None
    headers = {'Authorization': f'Bearer {token}'}
    if body is not None:
        headers['Content-Type'] = 'application/json'
    try:
        with urllib.request.urlopen(urllib.request.Request(url, data=body, headers=headers), timeout=10) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        raise RuntimeError(f'HTTP {error.code}: {error.read(1024).decode("utf-8", "replace")}') from None
    except urllib.error.URLError as error:
        raise RuntimeError(f'서버에 연결하지 못했습니다: {error.reason}') from None


def main():
    config = settings()
    token = config['NOTI_CLIENT_TOKEN']
    if not token:
        raise ValueError('.env에 NOTI_CLIENT_TOKEN을 설정하세요.')
    base = f'http://127.0.0.1:{os.environ.get("DEV_PORT", "8080")}'
    identity = request(f'{base}/v1/client', token)
    service = identity.get('service') or config['NOTI_CLIENT_SERVICE'] or 'checkout'
    if identity.get('service') and config['NOTI_CLIENT_SERVICE'] and config['NOTI_CLIENT_SERVICE'] != service:
        print(f'토큰에 연결된 서비스를 사용합니다: {service}', flush=True)
    payload = {'event_id': f'make-send-{uuid.uuid4()}', 'service': service, 'environment': 'development',
               'title': 'noti-mesh 발송 테스트', 'body': 'make send로 보낸 테스트 메시지입니다. 확인 버튼을 눌러 주세요.'}
    result = request(f'{base}/v1/messages', token, payload)
    message_id = result['message_id']
    print(f'{"중복 메시지로 묶음" if result.get("deduplicated") else "메시지 접수"}: {message_id}', flush=True)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        status = request(f'{base}/v1/messages/{message_id}', token)
        message = status['message']
        count = status['duplicate_count'] + 1
        if message.get('refresh_failed_at'):
            raise RuntimeError(f'Slack 수신 횟수 갱신 실패: {message.get("refresh_error") or "원인 확인 필요"}')
        if message.get('sent_at') and message.get('displayed_count', 0) >= count:
            print(f'Slack 게시 성공: {message.get("channel")} · {message.get("provider_message_ts")} · 수신 {count}회')
            return
        if message.get('failed_at'):
            raise RuntimeError(f'Slack 게시 실패: {message.get("last_error") or "원인 확인 필요"}')
        time.sleep(1)
    raise RuntimeError('아직 Slack 게시 결과를 받지 못했습니다. 관리자 화면에서 발송 상태를 확인하세요.')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, KeyError, json.JSONDecodeError) as error:
        print(f'발송 테스트 실패: {error}', file=sys.stderr)
        sys.exit(1)
