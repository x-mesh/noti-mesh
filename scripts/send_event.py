import json
import os
import sys
import uuid
from datetime import datetime, timezone

from send_test import request, settings


def main():
    config = settings()
    token = config['NOTI_CLIENT_TOKEN']
    if not token:
        raise ValueError('.env에 NOTI_CLIENT_TOKEN을 설정하세요.')
    base = f'http://127.0.0.1:{os.environ.get("DEV_PORT", "8080")}'
    identity = request(f'{base}/v1/client', token)
    service = identity.get('service') or config['NOTI_CLIENT_SERVICE'] or 'checkout'
    event_id = str(uuid.uuid4())
    incident_key = f'make-event-{event_id}'
    payload = {
        'event_id': event_id,
        'service': service,
        'environment': 'development',
        'incident_key': incident_key,
        'sequence': 1,
        'severity': 2,
        'recovered': False,
        'occurred_at': datetime.now(timezone.utc).isoformat(),
        'summary': 'make event로 생성한 테스트 사건입니다.',
    }
    result = request(f'{base}/v1/events', token, payload)
    print(f'테스트 사건 접수: {result["incident_id"]}')
    print(f'사건 키: {incident_key} · 서비스: {service} · 중요도: P2')
    print('관리자 화면의 사건 목록에서 확인하세요.')


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, KeyError, json.JSONDecodeError) as error:
        print(f'이벤트 전송 실패: {error}', file=sys.stderr)
        sys.exit(1)
