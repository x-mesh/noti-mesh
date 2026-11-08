// Translates the Korean admin UI to English after it renders.
// Only whole text nodes, fixed sentence shapes, and UI markers are replaced,
// so that Korean text from senders (incident summaries, messages) stays as sent.
(() => {
  const key = 'noti-mesh-lang';
  const EXACT = {
    '운영 워크스페이스': 'Operations workspace', '장애 대응 · 관리자': 'Incident response · admin', '워크스페이스': 'Workspace',
    '사건': 'Incidents', '일반 메시지': 'Messages', '발송 현황': 'Deliveries', '클라이언트 토큰': 'Client tokens',
    '점검 무음': 'Silences', '담당 그룹·정책': 'Groups & policy', 'API 문서·테스트': 'API docs & test',
    '알림은 줄이고,': 'Fewer alerts,', '맥락은 이어지게.': 'connected context.', '같은 문제의 반복을 묶고': 'Group repeats of one problem',
    '필요한 대응을 남깁니다.': 'and keep the response that matters.', '개발 모드': 'Development mode', '관리자 연결됨': 'Admin connected',
    '연결 대기': 'Waiting for connection', '마지막 데이터 표시 중': 'Showing the last data', '서버 연결됨': 'Server connected',
    '갱신 실패': 'Refresh failed', '미연결': 'Not connected', '서버 연결': 'Connect', '연결 관리': 'Connection',
    '시스템': 'System', '라이트': 'Light', '다크': 'Dark', '새로고침': 'Refresh', '본문으로 이동': 'Skip to content',
    '지금 대응이 필요한 사건부터 확인하세요.': 'Start with the incidents that need a response now.',
    '사건으로 묶지 않는 메시지의 전달과 확인을 살펴보세요.': 'Review the delivery and confirmation of messages outside incidents.',
    '예약과 재시도 상태를 확인하세요.': 'Check scheduled jobs and retries.', '서비스별 전송 토큰을 관리하세요.': 'Manage the sending token of each service.',
    '점검·배포 시간 동안 서비스 알림을 보류하세요.': 'Hold service notifications during maintenance and deployments.',
    '누구에게, 어떤 기준으로 전달하는지 확인하세요.': 'Check who receives what, and by which rules.',
    '담당 대기': 'Unassigned', '담당자를 기다리는 사건': 'Incidents that wait for an owner', '대응 중': 'In progress',
    '담당자가 맡은 사건': 'Incidents with an owner', '긴급·높음': 'Critical & high', '해결 전 P1 · P2 사건': 'Open P1 and P2 incidents',
    '해결됨': 'Resolved', '복구가 확인된 사건': 'Incidents with a confirmed recovery', '레벨별 수신': 'Received by level',
    '사건 이벤트와 일반 메시지 · 칸을 누르면 그 중요도의 사건만 표시합니다': 'Incident events and messages · select a cell to show only incidents of that priority',
    'P1 · 긴급': 'P1 · critical', 'P2 · 높음': 'P2 · high', 'P3 · 보통': 'P3 · normal', 'P4 · 낮음': 'P4 · low',
    '최근 1시간': 'Last hour', '24시간': '24 hours', '열린 사건': 'Open', '진행 중': 'Active', '전체': 'All',
    '중요도 · 오래된 순': 'Priority · oldest first', '서비스, 환경, 사건 키 검색': 'Search service, environment, or incident key',
    '서비스·환경·사건 키 검색': 'Search service, environment, or incident key', '모든 중요도': 'All priorities', '검색': 'Search',
    '조건 초기화': 'Clear filters', '검색 조건 초기화': 'Clear filters', '중요도': 'Priority', '사건 / 서비스': 'Incident / service',
    '상태': 'State', '담당자': 'Owner', '이벤트': 'Events', '최초 발생': 'First seen', '미지정': 'Unassigned',
    '이전': 'Previous', '다음': 'Next', '사건 목록': 'Incident list', '사건 상태 필터': 'Incident state filter',
    '전체 사건 현황': 'Incident overview', '관리 메뉴': 'Admin menu', '화면 테마': 'Theme', '화면 언어': 'Language',
    'noti-mesh 관리자 홈': 'noti-mesh admin home', '사건 불러오는 중': 'Loading incidents', '서버 연결 후 사건을 불러옵니다.': 'Incidents load after you connect.',
    '조건에 맞는 사건이 없어요': 'No incidents match', '검색어와 상태·중요도 조건을 바꿔 보세요.': 'Change the search text or the state and priority filters.',
    '지금 대응할 사건이 없어요': 'Nothing needs a response now', '새 사건이 수집되면 이 목록에 나타납니다.': 'New incidents appear in this list.',
    '전체 사건 보기': 'Show all incidents', '사건의 맥락을 한곳에서': 'Incident context in one place',
    '서버에 연결하면 수집된 사건과': 'Connect to the server to see incidents', '담당 상태를 확인할 수 있어요.': 'and their assignment state.',
    '사건 상세 닫기': 'Close incident detail', '사건 정보': 'Incident', '누적 이벤트': 'Events', '마지막 변경': 'Last change',
    '담당 그룹': 'Group', '다음 재알림': 'Next escalation', '발신 클라이언트': 'Sender', '중요도 판단': 'Priority decision',
    '· 미분류 전달': '· unclassified route', '이 사건의 담당자를 정하세요': 'Assign an owner', '담당자를 지정하면 재알림이 멈춥니다.': 'Assigning an owner stops escalation.',
    '해결 상태는 발신 시스템의 복구 이벤트로 확인합니다.': 'The incident resolves when the sender reports a recovery.',
    '담당자를 선택하세요': 'Select an owner', '담당자 지정': 'Assign', '지정 중…': 'Assigning…',
    '읽기 전용 계정입니다. 담당자 지정은 관리자에게 요청하세요.': 'This account is read-only. Ask an administrator to assign an owner.',
    '복구가 확인되어 사건이 해결됐어요.': 'A recovery event resolved this incident.',
    '점검 무음으로 알림을 보류하고 있어요. 무음이 끝날 때 사건이 열려 있으면 그때 알립니다.': 'A maintenance silence holds this notification. If the incident is still open when the silence ends, it is announced then.',
    '회고 초안': 'Post-incident draft', '수신·발송·담당·확인 기록으로 시간순 타임라인과 회고 양식을 만듭니다. 원인과 후속 조치는 직접 작성하세요.': 'Builds a timeline and a review template from the received, delivered, assigned, and confirmed records. Write the cause and follow-up yourself.',
    '초안 복사': 'Copy draft', 'Markdown 받기': 'Download Markdown', '회고 초안을 복사했어요.': 'Copied the draft.',
    '클립보드에 복사하지 못했어요. Markdown 받기를 이용하세요.': 'Could not copy. Use Download Markdown.',
    'AI 맥락 요약': 'AI context summary', '현재 서버에서 AI 요약을 사용하지 않아요.': 'AI summaries are off on this server.',
    '아직 요약이 없어요. 생성된 요약은 다음 조회에 표시됩니다.': 'No summary yet. A new summary appears on the next refresh.',
    '요약 생성에 실패했어요. 규칙에 따른 알림과 담당자 지정은 계속할 수 있어요.': 'The summary failed. Rule-based alerts and assignment still work.',
    'AI가 작성한 보조 정보입니다. 아래 원본 이벤트와 함께 확인하세요.': 'AI-written context. Check it against the original events below.',
    '발송 여부와 담당자는 AI가 결정하지 않습니다.': 'AI does not decide delivery or owners.',
    '이벤트 타임라인': 'Event timeline', '복구 확인': 'Recovery', '원본 열기 ↗': 'Open source ↗', '이전 이벤트': 'Earlier events',
    '발송 이력': 'Delivery history', '담당자 지정 기록': 'Assignment history', '아직 담당 기록이 없어요.': 'No assignments yet.',
    '명시적 확인': 'Confirmations', '아직 확인한 사람이 없어요. 읽음 여부는 알 수 없습니다.': 'Nobody has confirmed yet. Read status is unknown.',
    '연결된 사건': 'Related incidents', '상관관계 ID가 같은 사건입니다. 원인과 결과 관계를 뜻하지는 않습니다.': 'Incidents with the same correlation ID. This does not mean cause and effect.',
    '메일·Telegram 발송 이력': 'Email and Telegram deliveries', '메일·Telegram 발송 기록이 없어요.': 'No email or Telegram deliveries.',
    '메일과 Telegram의 수신함 도착·읽음은 확인할 수 없습니다.': 'Inbox arrival and reading cannot be confirmed for email and Telegram.',
    '이벤트와 발송 이력을 불러오고 있어요.': 'Loading events and deliveries.', '상세를 불러오지 못했어요': 'Could not load the detail',
    '다시 시도': 'Retry', '사건 상세를 자동 갱신하지 못했어요.': 'Could not refresh the incident detail.', '최신 상태 확인': 'Check the latest state',
    '사건 상태가 바뀌었어요. 최신 상태를 확인한 뒤 다시 지정하세요.': 'The incident changed. Check the latest state and assign again.',
    '이 사건을 담당할 수 있는 그룹의 사용자를 선택하세요.': 'Select a user from a group that can own this incident.',
    '최근 메시지 100건입니다. 확인은 Slack 버튼을 누른 기록이며 읽음 여부를 뜻하지 않습니다.': 'The latest 100 messages. A confirmation is a Slack button press, not proof of reading.',
    '최근 메시지 100건입니다.': 'The latest 100 messages.', '수신한 일반 메시지가 없어요.': 'No messages received.',
    '세부 정보': 'Details', 'Slack 게시': 'Posted to Slack', 'Slack 미사용': 'Slack off', '확인 없음': 'Not confirmed',
    '점검 무음으로 보류': 'Held by silence', '메시지 본문을 확인할 수 없어요.': 'The message body is not available.',
    '확인 시점 메시지 보기': 'Show the message at confirmation', '발송 대기': 'Waiting', '재시도 대기': 'Retry waiting', '발송 실패': 'Failed',
    'Slack 게시 성공': 'Posted to Slack', 'Slack 모의 서버 접수': 'Accepted by the Slack mock', '발송 실패 · 조치 필요': 'Failed · action needed',
    'Slack 재시도': 'Retry Slack', '발송 흐름': 'Delivery flow', '최근 발송 작업 100건입니다. 채널별 접수와 명시적 확인을 구분합니다.': 'The latest 100 delivery jobs. Channel acceptance and explicit confirmation are shown separately.',
    '발송 작업이 없어요.': 'No delivery jobs.', '발송 경로': 'Delivery path', '발신': 'From', 'Slack 채널': 'Slack channel',
    '채널 미확정': 'Channel not set', '미사용': 'Off', '취소': 'Cancelled', '일반 메시지 발송': 'Message deliveries', '발송한 일반 메시지가 없어요.': 'No messages sent.',
    '수동 재시도 기록': 'Manual retries', '최근 100건입니다.': 'The latest 100.', '메일·Telegram': 'Email and Telegram', 'Slack 사건': 'Slack incident',
    'Slack 일반 메시지': 'Slack message', '재시도 중': 'Retrying', '가장 이른 발송 예정': 'Next scheduled delivery',
    '예약된 요약과 재알림을 포함합니다.': 'Includes scheduled summaries and escalations.', '실패 후 재전송을 기다리는 작업입니다.': 'Jobs that wait for a retry after a failure.',
    '예약 시각이 지난 작업은 지연 여부를 확인하세요.': 'Check jobs that are past their scheduled time.',
    '전송 실패도 사건의 이력으로': 'Delivery failures are part of the incident history',
    '사건 상세에서 각 발송 작업의 예정 시각, 재시도 횟수와 마지막 오류를 확인할 수 있어요.': 'The incident detail shows the schedule, retry count, and last error of each delivery job.',
    'Slack 전송 제한': 'Slack rate limits', 'Slack이 안내한 재시도 시각을 따릅니다.': 'Retries follow the time that Slack gives.', '발송 작업 유지': 'Jobs are kept',
    '서버 재시작': 'Server restart', '저장된 예약과 중단된 작업을 이어서 처리합니다.': 'Saved schedules and interrupted jobs continue.', 'PostgreSQL에 보관': 'Stored in PostgreSQL',
    '전달 증빙 내보내기': 'Export delivery evidence',
    '기간 안에 생긴 사건마다 한 줄로, 첫 Slack 게시·멘션 대상·메일과 Telegram 접수·확인·담당·해결 시각을 CSV로 받습니다. 메일과 Telegram은 발송사가 접수한 기록이며 수신자가 읽었다는 뜻이 아닙니다. 최대 92일입니다.': 'Download a CSV with one row per incident in the range: first Slack post, mentions, email and Telegram acceptance, confirmation, assignment, and resolution. Email and Telegram show provider acceptance, not reading. Up to 92 days.',
    '시작일': 'From', '종료일': 'To', 'CSV 받기': 'Download CSV', 'CSV를 내려받았어요.': 'Downloaded the CSV.',
    '시작일이 종료일보다 늦지 않게, 92일 이내로 고르세요.': 'Select a start date on or before the end date, within 92 days.',
    '대체 발송': 'Fallback', '동시 발송': 'Parallel', '담당자 없을 때 발송': 'Send when unacknowledged', 'Slack 실패 시 대기': 'Waits for a Slack failure',
    'Slack 실패 시 대체': 'Fallback on Slack failure', 'Slack 게시 성공으로 대체 발송 취소': 'Fallback cancelled after a Slack post',
    '사건 또는 기본 발송 상태 변경으로 취소': 'Cancelled after an incident or primary delivery change', '사건 상태 변경으로 발송 취소': 'Cancelled after an incident change',
    '이전 발송 작업이 취소됨': 'An earlier job was cancelled', '설정에서 대상이 제거됨': 'The destination was removed from the configuration',
    '취소된 발송': 'Cancelled deliveries', '발송 채널': 'Channels', 'SMTP 서버 접수': 'Accepted by SMTP', 'Telegram API 접수': 'Accepted by the Telegram API',
    '모의 서버 접수': 'Accepted by the mock', '메일 제공자 설정 없음': 'Email provider not configured', 'Telegram 제공자 설정 없음': 'Telegram provider not configured',
    'SMTP 영구 거부': 'SMTP permanent rejection', 'SMTP 일시 오류': 'SMTP temporary error', 'SMTP 전송 실패': 'SMTP send failed', 'SMTP 응답 시간 초과': 'SMTP timeout',
    'Telegram 요청 실패': 'Telegram request failed', '발송을 다시 시도합니다.': 'Retrying the delivery.',
    '이 발송을 다시 시도할까요? 다른 채널로 이미 전달됐을 수 있습니다.': 'Retry this delivery? Another channel may have delivered it already.',
    '클라이언트 토큰 발급': 'Create a client token', '발급한 토큰은 한 번만 표시됩니다. 지정한 서비스의 이벤트만 보낼 수 있습니다.': 'A new token is shown once. It can send events only for its service.',
    '클라이언트 이름': 'Client name', '서비스 식별자': 'Service', '토큰 생성': 'Create token', '지금 토큰을 복사하세요. 다시 표시할 수 없습니다.': 'Copy the token now. It is not shown again.',
    'Slack Incoming Webhook을 쓰던 발신자는 아래 URL로 바꾸면 됩니다. 환경은': 'A Slack incoming webhook sender can switch to the URL below. Set the environment with',
    '로 지정하며 기본값은 production입니다.': '; the default is production.', '복사 완료': 'Copied', '발급된 토큰': 'Tokens', '발급된 토큰이 없어요.': 'No tokens.',
    '전송 이력': 'History', '교체': 'Rotate', '폐기': 'Revoke', '폐기됨': 'Revoked', '읽기 전용': 'Read-only', '현재 계정은 읽기 전용입니다.': 'This account is read-only.',
    '토큰 목록을 불러오는 중이에요.': 'Loading tokens.', '이력을 불러오고 있어요.': 'Loading history.', '접수한 기록이 없어요.': 'No accepted records.',
    '최대 100건을 표시합니다. 접수 시각이 있는 기록은 최신순이며, 거부된 요청은 포함하지 않습니다.': 'Shows up to 100 records, newest first. Rejected requests are not included.',
    '장애 이벤트': 'Incident event', '시각 기록 없음': 'No time recorded', '토큰을 복사한 뒤 복사 완료를 눌러 주세요.': 'Copy the token, then select Copied.',
    '새 토큰을 복사한 뒤 복사 완료를 눌러 주세요.': 'Copy the new token, then select Copied.', '표시된 토큰을 먼저 복사한 뒤 복사 완료를 눌러 주세요.': 'Copy the shown token first, then select Copied.',
    '토큰을 교체하면 기존 토큰이 즉시 작동을 멈춥니다. 교체할까요?': 'Rotating stops the current token at once. Rotate it?',
    '이 토큰을 폐기할까요? 클라이언트의 전송이 중단됩니다.': 'Revoke this token? The client can no longer send.',
    '점검 무음 시작': 'Start a silence',
    '무음 중에 열린 사건은 기록만 하고, 무음이 끝날 때 아직 열려 있으면 그때 알립니다. 무음 중에 해결된 사건과 일반 메시지는 보내지 않습니다. 무음 전에 알린 사건의 해결 알림은 그대로 보냅니다.': 'Incidents that open during a silence are recorded and announced when the silence ends if they are still open. Incidents resolved during the silence and messages are not sent. Incidents announced before the silence keep their messages.',
    '서비스': 'Service', '환경 (비우면 모든 환경)': 'Environment (empty for all)', '기간': 'Duration', '사유': 'Reason', 'DB 정기 점검': 'Database maintenance',
    '지금부터 무음': 'Silence now', '무음 기록': 'Silences', '무음 기록이 없어요.': 'No silences.', '지금 종료': 'End now', '모든 환경': 'All environments',
    '점검 무음을 시작했어요.': 'Started the silence.', '무음을 종료했어요.': 'Ended the silence.', '서비스와 사유를 입력하세요.': 'Enter a service and a reason.',
    '무음을 지금 종료할까요? 보류 중인 열린 사건은 곧 알립니다.': 'End the silence now? Held open incidents are announced soon.', '무음 기록을 불러오지 못했어요': 'Could not load silences',
    '30분': '30 min', '1시간': '1 hour', '2시간': '2 hours', '4시간': '4 hours', '8시간': '8 hours',
    'Slack 테스트 설정': 'Slack test settings', '테스트 알림을 보낼 채널과 담당자를 설정하세요.': 'Set the channel and user for test notifications.',
    'Slack 목록 불러오기': 'Load Slack lists', '연결된 워크스페이스': 'Workspace', '봇이 참가한 채널만 표시합니다.': 'Shows only channels that the bot joined.',
    '비공개 채널에는 groups:read 권한이 필요합니다.': 'Private channels need the groups:read scope.', '채널을 선택하세요': 'Select a channel', '담당 사용자': 'User',
    '사건을 맡을 담당자입니다. 메시지 확인은 다른 사용자도 할 수 있어요.': 'The user who owns incidents. Other users can confirm messages too.',
    '설정 저장': 'Save settings', '저장 중…': 'Saving…', '저장 후 서버를 다시 시작하면 적용됩니다.': 'Restart the server after saving to apply.',
    '설정을 저장했어요. 서버를 다시 시작하면 적용됩니다.': 'Saved. Restart the server to apply.', '저장하지 못했어요. 선택한 값과 서버 상태를 확인한 뒤 다시 시도하세요.': 'Could not save. Check the values and the server, then try again.',
    '표시할 채널이 없어요. Slack에서 봇을 초대해 주세요.': 'No channels to show. Invite the bot in Slack.', '목록을 불러오지 못했어요. Slack 앱의 channels:read·users:read 권한과 설치 상태를 확인하세요.': 'Could not load the lists. Check the channels:read and users:read scopes and the app installation.',
    '불러오는 중…': 'Loading…', '서버에서 불러온 현재 설정입니다. 정책을 바꾸려면 서버의 TOML 설정을 수정한 뒤 재시작하세요.': 'Current server settings. To change the policy, edit the TOML file and restart the server.',
    '기본 그룹': 'Default group', '예비 담당': 'Backup', '서비스별 전달 정책': 'Service routes', '환경': 'Environment', '최소 중요도': 'Minimum priority',
    '등록된 정책이 없어요. 기본 그룹으로 전달합니다.': 'No routes. Items go to the default group.', '요약·재알림': 'Summaries & escalation',
    '반복 이벤트 요약 간격': 'Summary interval for repeats', '반복 이벤트는 같은 사건에 누적됩니다.': 'Repeated events add to the same incident.',
    'P3 · P4 묶음 요약': 'P3 · P4 digest', 'P1 담당 대기 기한': 'P1 acknowledgment deadline', 'P2 담당 대기 기한': 'P2 acknowledgment deadline',
    '단계별 재알림 간격': 'Escalation interval', '사용 안 함': 'Off', '사용 중': 'On', '관리자 화면에서 지정': 'Assigned in the admin UI', 'Slack에서 담당': 'Assigned in Slack',
    '관리자가 지정': 'Assigned by an administrator', '작업자 정보 없음': 'No actor recorded', '공유 관리자': 'Shared admin', '운영 관리자': 'Operations admin',
    '서버 연결 설정': 'Server connection', '운영 서버에 연결': 'Connect to the server', '이 서버의 관리자 토큰을 입력하세요.': 'Enter the admin token of this server.',
    '관리자 토큰': 'Admin token', '토큰은 현재 탭의 메모리에만 보관합니다.': 'The token stays in this tab\'s memory only.', '연결': 'Connect', '연결 중…': 'Connecting…',
    '연결 창 닫기': 'Close', '연결 해제': 'Disconnect', '표시된 토큰을 다시 볼 수 없습니다. 연결을 해제할까요?': 'The shown token cannot be shown again. Disconnect?',
    '운영 서버에 연결했어요.': 'Connected.', '서버 연결을 해제했어요.': 'Disconnected.', '관리자 토큰이 올바르지 않아요.': 'The admin token is not valid.',
    '서버에 관리자 접근이 설정되지 않았거나 요청을 처리할 수 없어요.': 'Admin access is not configured, or the server cannot process the request.',
    '서버에 연결하지 못했어요. 네트워크와 서버 상태를 확인하세요.': 'Could not reach the server. Check the network and the server.',
    '서버 상태를 확인하지 못했어요. 연결을 확인한 뒤 다시 시도하세요.': 'Could not check the server. Check the connection and try again.',
    '서버 상태를 확인하고 있어요': 'Checking the server', '서버 연결이 필요해요': 'Connection needed', '상단의 서버 연결 버튼으로 관리자 토큰을 입력하세요.': 'Use Connect at the top to enter an admin token.',
    '새로고침으로 다시 시도하세요.': 'Refresh to try again.', '개발 모드입니다. Slack·메일·Telegram은 모의 서버로만 발송되며 실제 메시지는 전송되지 않습니다.': 'Development mode. Slack, email, and Telegram go to mocks only; no real messages are sent.',
    '방금 전': 'just now', '기록 없음': 'No record', '시각 정보 없음': 'No time', '아직 갱신하지 않음': 'Not refreshed yet', '마지막 갱신': 'Last refresh',
    '발송 안 함': 'Not sent', '발송 전': 'Not sent yet', '없음': 'None', '예정': 'Scheduled', '지정 대상': 'Targets', '멘션': 'Mention', '메일': 'Email',
    '상세': 'Detail', '경로': 'Path', '중복': 'Duplicates', '최근': 'Latest', '발생': 'Occurred', '접수': 'Received', '서버': 'Server', '종료': 'Ended',
    '긴급': 'Critical', '높음': 'High', '보통': 'Normal', '낮음': 'Low', '재시도': 'Retries', '페이지': 'Page', '사건 · noti-mesh': 'Incidents · noti-mesh', '운': 'O', 'Slack 채널 · 예정': 'Slack channel · scheduled',
    '요청을 처리하지 못했어요.': 'Could not process the request.', 'CSV를 만들지 못했어요.': 'Could not create the CSV.', '회고 초안을 만들지 못했어요.': 'Could not create the draft.',
  };
  const TITLES = { '사건': 'Incidents', '일반 메시지': 'Messages', '발송 현황': 'Deliveries', '클라이언트 토큰': 'Client tokens', '점검 무음': 'Silences', '담당 그룹·정책': 'Groups & policy' };
  // Whole-node shapes with variable parts; the captured parts are kept as they are.
  const PATTERNS = [
    [/^(\d+)분 전$/, '$1 min ago'], [/^(\d+)시간 전$/, '$1 h ago'], [/^(\d+)일 전$/, '$1 d ago'],
    [/^(\d+)분$/, '$1 min'], [/^(\d+)시간$/, '$1 h'], [/^([\d,]+)건$/, '$1'],
    [/^최근 (\d+)건$/, 'Last $1'], [/^최근 (\d+)건 · 최신 순$/, 'Last $1 · newest first'], [/^중복 ([\d,]+)건$/, '$1 duplicates'],
    [/^(\d+) 페이지$/, 'Page $1'], [/^(\d+)–(\d+)번째 사건 · 현재 조건 기준$/, 'Incidents $1–$2 · current filter'],
    [/^현재 조건에서 (\d+)건$/, '$1 match the filter'], [/^대체 발송 대기 (\d+)건$/, 'Fallback waiting: $1'],
    [/^조치가 필요한 실패 (\d+)건$/, 'Failures that need action: $1'], [/^세부 정보 · 확인 기록 (\d+)건$/, 'Details · $1 confirmations'],
    [/^확인 ([^가-힣]+)$/, 'Confirmed by $1'], [/^발신 P(\d) → 적용 P(\d)$/, 'Sent P$1 → applied P$2'], [/^P(\d) 이벤트 수집$/, 'P$1 event'],
    [/^([^가-힣]+) · 순번 ([\d,]+)$/, '$1 · sequence $2'], [/^장애 이벤트 · (.+)$/, 'Incident event · $1'],
    [/^(.+) 님이 대응 중이에요\. 복구가 확인되면 해결 상태로 바뀝니다\.$/, '$1 is handling this incident. It resolves when a recovery event arrives.'],
    [/^(.+) 님을 담당자로 지정했어요\.$/, 'Assigned $1.'], [/^(.+) · 미분류 전달$/, '$1 · unclassified route'], [/^선택: (.+)$/, 'Selected: $1'], [/^자동 갱신 · (.+)$/, 'Auto refresh · $1'],
    [/^(.+) · 재시도 (\d+)회$/, '$1 · $2 retries'], [/^Slack 채널 (.+)$/, 'Slack channel $1'], [/^(\d+)건 더 보기$/, 'Show $1 more'], [/^이전 이벤트 ([\d,]+)건 더 보기$/, 'Show $1 earlier events'], [/^담당 대기 ([\d,]+)$/, '$1 unassigned'], [/^일반 메시지 · (.+)$/, 'Message · $1'], [/^취소된 발송 ([\d,]+)건$/, 'Cancelled deliveries: $1'],
    [/^(.+) · 생성 (.+) · 마지막 교체 (.+) · 마지막 사용 (.+)$/, '$1 · created $2 · last rotated $3 · last used $4'],
    [/^(.+) · (.+) ~ (.+) · 작성 (.+)$/, '$1 · $2 – $3 · by $4'], [/^(.+) · noti-mesh$/, (m, a) => `${TITLES[a] ?? a} · noti-mesh`],
  ];
  // Marker words inside composite rows. Each rule runs only on nodes that its guard matches.
  const SCOPED = [
    [/^\d+분( → \d+분)+$/, [[/(\d+)분/g, '$1 min']]],
    [/보류 중 사건|보류한 메시지/, [[/^진행 중/, 'Active'], [/^종료/, 'Ended'], [/^취소 /, 'Ended '], [/· 보류 중 사건 (\d+)건/, '· held incidents $1'], [/· 보류한 메시지 (\d+)건/, '· held messages $1']]],
    [/· 지정 대상|· 멘션 요청|· 메시지 확인 이력/, [[/· 지정 대상 /, '· targets '], [/· 멘션 요청 /, '· mentions '], [/· 메시지 확인 이력 (\d+)건/, '· $1 confirmations'], [/· 중복 (\d+)건 묶음/, '· $1 duplicates grouped'], [/발송 안 함/, 'not sent'], [/발송 전/, 'not sent yet'], [/기록 없음/, 'no record'], [/없음/, 'none'], [/예정/, 'scheduled']]],
    [/\((개발 모드|공유 관리자|작업자 정보 없음)\)|^(폐기|교체|발급) /, [[/(^|· )폐기 /g, '$1revoked '], [/(^|· )교체 /g, '$1rotated '], [/(^|· )발급 /g, '$1created '], [/\(개발 모드\)/g, '(development mode)'], [/\(공유 관리자\)/g, '(shared admin)'], [/\(작업자 정보 없음\)/g, '(no actor)']]],
    [/^기록 .+ · 메시지 /, [[/^기록 /, 'Recorded '], [/· 메시지 /, '· message ']]],
    [/^확인 .+ · 기록 .+ · 메시지 /, [[/^확인 /, 'Confirmed '], [/· 기록 /, '· recorded '], [/· 메시지 /, '· message ']]],
    [/ · 확인 \d{1,2}\/|· 확인 \d{4}\./, [[/ · 확인 /, ' · confirmed ']]],
    [/^(발생|접수) .+ · (발생|접수) /, [[/(^|· )발생 /g, '$1Occurred '], [/(^|· )접수 /g, '$1Received ']]],
    [/^· (재시도|발송 ID|확인 버튼|중복|확인|메시지|발생|접수|작성|예정|순번|생성|기록)\b/, [[/^· 재시도/, '· retries'], [/^· 발송 ID/, '· delivery ID'], [/^· 확인 버튼/, '· confirm button'], [/^· 중복/, '· duplicates'], [/^· 확인/, '· confirmed'], [/^· 메시지/, '· message'], [/^· 발생/, '· occurred'], [/^· 접수/, '· received'], [/^· 작성/, '· by'], [/^· 예정/, '· scheduled'], [/^· 순번/, '· sequence'], [/^· 생성/, '· created'], [/^· 기록/, '· recorded']]],
  ];
  const ATTRS = ['placeholder', 'aria-label', 'title'];
  const original = new WeakMap();
  const read = () => {
    try {
      const saved = localStorage.getItem(key);
      if (saved === 'ko' || saved === 'en') return saved;
    } catch {}
    return (navigator.language || 'ko').toLowerCase().startsWith('ko') ? 'ko' : 'en';
  };
  let lang = read();
  const translate = (text) => {
    const trimmed = text.trim();
    if (!/[가-힣]/.test(trimmed)) return null;
    const pad = (value) => text.replace(trimmed, () => value);
    if (EXACT[trimmed] !== undefined) return pad(EXACT[trimmed]);
    const http = trimmed.match(/^(.+?) \(HTTP (\d+)\)$/);
    if (http && EXACT[http[1]] !== undefined) return pad(`${EXACT[http[1]]} (HTTP ${http[2]})`);
    for (const [pattern, to] of PATTERNS) if (pattern.test(trimmed)) return pad(trimmed.replace(pattern, to));
    let out = trimmed;
    for (const [guard, rules] of SCOPED) if (guard.test(out)) for (const [from, to] of rules) out = out.replace(from, to);
    return out === trimmed ? null : pad(out);
  };
  const visitText = (node) => {
    const store = original.get(node);
    if (lang === 'ko') {
      if (store?.text !== undefined && node.nodeValue === store.done) node.nodeValue = store.text;
      return;
    }
    const value = node.nodeValue;
    if (store?.done === value) return;
    const next = translate(value);
    if (next !== null) {
      original.set(node, { ...store, text: value, done: next });
      node.nodeValue = next;
    }
  };
  const visitElement = (el) => {
    for (const name of ATTRS) {
      const value = el.getAttribute(name);
      if (!value) continue;
      const store = original.get(el) ?? {};
      if (lang === 'ko') {
        if (store[name] && value === store[name].done) el.setAttribute(name, store[name].text);
        continue;
      }
      if (store[name]?.done === value) continue;
      const next = translate(value);
      if (next !== null) {
        store[name] = { text: value, done: next };
        original.set(el, store);
        el.setAttribute(name, next);
      }
    }
  };
  const walk = (root) => {
    if (root.nodeType === Node.TEXT_NODE) return visitText(root);
    if (root.nodeType !== Node.ELEMENT_NODE) return;
    if (root.closest('script,style,[translate="no"]')) return;
    visitElement(root);
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT, {
      acceptNode: (n) => (n.nodeType === Node.ELEMENT_NODE && n.matches('script,style,[translate="no"]') ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
    });
    let n;
    while ((n = walker.nextNode())) n.nodeType === Node.TEXT_NODE ? visitText(n) : visitElement(n);
  };
  const observer = new MutationObserver((records) => {
    if (lang === 'ko') return;
    for (const r of records) {
      if (r.type === 'characterData') visitText(r.target);
      else if (r.type === 'attributes') visitElement(r.target);
      else r.addedNodes.forEach(walk);
    }
  });
  const start = () => {
    document.documentElement.lang = lang;
    walk(document.documentElement);
    observer.observe(document.documentElement, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ATTRS });
  };
  window.notiI18n = {
    get lang() { return lang; },
    locale: () => (lang === 'en' ? 'en-US' : 'ko-KR'),
    t: (text) => (lang === 'en' ? translate(text) ?? text : text),
    set(next) {
      lang = next === 'en' ? 'en' : 'ko';
      try { localStorage.setItem(key, lang); } catch {}
      document.documentElement.lang = lang;
      walk(document.documentElement);
      window.dispatchEvent(new Event('noti-lang'));
    },
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', start, { once: true });
  else start();
})();
