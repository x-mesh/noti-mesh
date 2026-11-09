# noti-mesh

noti-mesh is a self-hosted notification gateway for operations teams. Services, monitoring tools, and scripts send incidents and ordinary messages to one HTTP API. noti-mesh routes each item to the responsible group in Slack and follows each incident until recovery. Email and Telegram can receive fallback or parallel copies.

noti-mesh keeps each event, acknowledgment, escalation, and delivery attempt in PostgreSQL. The admin web UI shows incidents, deliveries, maintenance silences, post-incident drafts, and delivery evidence. The server is written in Rust.

![An alert opens a P1 incident in the admin UI, an owner takes it, and a recovery event resolves it](docs/media/incident-lifecycle.gif)

Inputs:

- The noti-mesh API: `POST /v1/events` for incidents and `POST /v1/messages` for ordinary messages.
- Slack incoming webhook payloads, so that existing senders change only the URL.
- Alerts from Prometheus through the Alertmanager API (`POST /api/v2/alerts`), so Prometheus needs no Alertmanager.
- Webhooks from Prometheus Alertmanager, Grafana, Uptime Kuma, Sentry, and Datadog.

Outputs:

- Slack messages with buttons for assignment and confirmation.
- Email through SMTP and Telegram through a bot.
- Optional AI summaries. AI is off by default.

## Quick start with mocks

This path needs Docker and `make`. It uses local Slack, email, and Telegram mocks and sends nothing outside your machine.

1. Run `make dev-mock`.
2. In a second terminal, send a test incident:

   ```sh
   curl -i http://localhost:8080/v1/events \
     -H 'Authorization: Bearer dev-source-token-not-for-production-0001' \
     -H 'Content-Type: application/json' \
     -d '{"event_id":"quickstart-1","service":"checkout","environment":"development","incident_key":"quickstart","sequence":1,"severity":2,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z","summary":"Database calls timed out."}'
   ```

3. Open `http://localhost:8080/admin`.

The API returns HTTP 202 and an `incident_id`. The incident appears in the admin UI as an unacknowledged P2 incident. The Slack mock writes `[DEV MOCK] /api/chat.postMessage: accepted locally; no Slack message sent` to the stack output.

Run `make dev-mock-down` to stop the mock stack.

## Run the tests

Requirements:

- The current stable Rust toolchain, with `rustfmt` and `clippy`. CI uses the latest stable release.
- Docker, for a disposable PostgreSQL database.
- Python 3, for the script tests.

1. Start a disposable PostgreSQL 18 database:

   ```sh
   docker run -d --rm --name noti-mesh-test-db -e POSTGRES_PASSWORD=test -e POSTGRES_DB=noti_mesh_test -p 127.0.0.1:55432:5432 postgres:18
   until docker exec noti-mesh-test-db pg_isready -U postgres >/dev/null 2>&1; do sleep 1; done
   export NOTI_TEST_DATABASE_URL=postgresql://postgres:test@127.0.0.1:55432/noti_mesh_test
   ```

2. Run the Rust checks and tests:

   ```sh
   cargo fmt --all -- --check
   cargo clippy --locked --all-targets --all-features -- -D warnings
   cargo test --locked
   ```

3. Run the Python tests:

   ```sh
   python3 -m pip install telethon==1.45.0
   python3 -m unittest discover -s tests -p 'test_*.py'
   ```

4. Stop the database. The `--rm` option removes the container:

   ```sh
   docker stop noti-mesh-test-db
   ```

Each database test creates and drops its own `noti_mesh_test_*` schema, so the tests can run in parallel. If `NOTI_TEST_DATABASE_URL` is not set, the database tests pass without a check. If the `CI` variable is also set, the database tests fail. Without Telethon, the Python tests skip the Telegram forwarder tests.

Run `make scenarios` to check the main flows end to end. The command starts a separate mock stack on port 18080, runs `scripts/dev_scenarios.py`, and removes the stack. It does not read `.env` and does not contact real Slack, email, or Telegram. Set `SCENARIO_PORT` to use a different port. The scenarios cover incident delivery and acknowledgment, rejected button notices, severity rise mentions, message duplicates, burst delivery time, Slack failures with fallback delivery, `/health/delivery`, and the container healthcheck.

The CI workflow in `.github/workflows/ci.yaml` runs the same checks on each push and pull request. It also builds the Docker image.

## Deploy to production

`compose.yaml` runs noti-mesh and PostgreSQL 18 on one host. Compose publishes port 8080 on `127.0.0.1` only.

Requirements:

- A host with Docker and Docker Compose.
- A Slack app in your workspace.
- An HTTPS reverse proxy, if users or Slack connect from other hosts.

### Prepare the Slack app

1. Create a Slack app and install it in your workspace.
2. Add the bot scopes `chat:write`, `channels:read`, and `groups:read`. The delivery check uses `channels:read` and `groups:read` to confirm channel membership.
3. Copy the bot token and the signing secret.
4. Connect the Slack buttons. For Socket Mode, turn on Socket Mode and create an app-level token with `connections:write`. For HTTP delivery, turn on Interactivity and set the request URL to `https://<your host>/v1/slack/actions`.
5. Invite the bot to each target channel.

### Configure

1. Copy `config.example.toml` to `config.toml`. Compose mounts this file into the container.
2. Replace each `REPLACE_*` value with your Slack workspace ID, user IDs, and channel IDs.
3. Set the service routes in the `[[routes]]` entries.
4. Create a `.env` file next to `compose.yaml`. Docker Compose reads this file.

   ```sh
   POSTGRES_PASSWORD=<URL-safe password>
   NOTI_API_TOKEN=<random value>
   NOTI_ADMIN_TOKEN=<different random value>
   SLACK_BOT_TOKEN=<bot token>
   SLACK_SIGNING_SECRET=<signing secret>
   SLACK_APP_TOKEN=<app-level token, for Socket Mode only>
   ```

Use at least 32 characters for each token, and use a different value for each token. If a token is shorter or two tokens are equal, the server does not start. `openssl rand -hex 32` makes a suitable value. Use URL-safe characters in `POSTGRES_PASSWORD`, because Compose puts it into `DATABASE_URL`. If you set `DATABASE_URL` yourself, use percent-encoded characters for the password.

If no admin token is set, the admin API returns HTTP 503. For individual administrator accounts, see [Named administrators](#named-administrators).

### Start and check

```sh
docker compose up -d --build
curl -i http://127.0.0.1:8080/health
curl -i http://127.0.0.1:8080/health/delivery
```

The server applies the database migrations at startup. `/health` returns HTTP 200 when PostgreSQL and the workers run. `/health/delivery` returns HTTP 200 when Slack accepts the token and the bot can reach the configured channels. Otherwise, it returns HTTP 503 and a `problems` list. Then [send an incident event](#send-an-incident-event) with `NOTI_API_TOKEN`, and look for the message in Slack.

Put the HTTPS reverse proxy in front of `127.0.0.1:8080`. Use HTTPS for the admin UI, the API, and the Slack action URL outside localhost.

Docker restarts both containers after a host or daemon restart. Each container keeps at most three 10 MB log files. Docker marks the app container unhealthy if `/health` fails, but it does not restart an unhealthy container. For checks from another host, see [Health checks and watchdog](#health-checks-and-watchdog).

### Upgrade

Back up the database before an upgrade. Then rebuild and restart:

```sh
git pull
docker compose up -d --build
```

The new version applies its new migrations at startup.

### Back up the database

The `postgres` volume keeps the database. This command writes a SQL dump:

```sh
docker compose exec -T db pg_dump -U noti_mesh noti_mesh > noti-mesh-backup.sql
```

To restore a dump, load it into an empty `noti_mesh` database with `psql`.

### AI summaries

The example configuration disables AI. To enable AI, set `ai.enabled`, select `ai.model`, and add `OPENAI_API_KEY` to `.env`.

## Local development with real Slack

The `make dev` stack sends messages to a real Slack channel. It uses a separate PostgreSQL volume from the mock stack.

1. Start Docker.
2. Add `SLACK_BOT_TOKEN` and `SLACK_SIGNING_SECRET` to `.env`.
3. Invite the Slack app to the channel.
4. Run `make dev`.
5. Open `http://localhost:8080/admin`.
6. Open the group and policy page.
7. Select a Slack channel and user, then save the settings.
8. Restart `make dev`.

The app uses the selected user for Slack confirmation and assignment. Grant `channels:read` and `users:read` to list public channels and users. Grant `groups:read` to list private channels.

The development UI accepts admin requests without authentication on all network interfaces. Use this mode only on a trusted network.

To receive Slack button actions in Socket Mode, add `SLACK_APP_TOKEN` to `.env`. Create an app-level token with `connections:write` in Slack. The app log shows the number of open Socket Mode connections for the Slack app and each received button action.

If Socket Mode is disabled, expose `/v1/slack/actions` at an HTTPS URL that Slack can reach. Set that URL in the Slack app under Interactivity & Shortcuts.

The `make dev` stack does not configure email or Telegram. Both development stacks use port 8080, so stop one stack before you start the other. If port 8080 is occupied, run `make dev DEV_PORT=8081`.

Use `dev-source-token-not-for-production-0001` for event requests. Use `development` as the event environment for the example service route.

To send a test message with a client token, create a token for the `checkout` service in the admin UI. Add the token to `.env` as `NOTI_CLIENT_TOKEN`. Run `make send`. The command uses the service assigned to the client token. For a source token, set `NOTI_CLIENT_SERVICE` in `.env`. The command waits for Slack to accept the message. Run `make event` to create a P2 test incident with the same client token. Each run creates a new incident.

Press Ctrl+C to stop the stack. Run `make dev-down` to remove its containers and network. Database contents persist between runs.

## Send an incident event

```sh
curl http://localhost:8080/v1/events \
  -H "Authorization: Bearer $NOTI_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"event_id":"checkout-001","service":"checkout","environment":"production","incident_key":"outage-001","sequence":1,"severity":2,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z","error_code":"DB_UNAVAILABLE","correlation_id":"deployment-001","summary":"Database calls timed out.","source_url":"https://example.com/traces/checkout-001"}'
```

Use severity 1 for critical incidents and severity 4 for low-priority incidents. Increase the sequence for each new event within an incident. Add `summary` and `source_url` to provide context.

Reuse the event ID only for an identical retry. Send `recovered: true` to close an incident. Use a new incident key after recovery.

## Send an ordinary message

Send a message without an incident lifecycle:

```sh
curl http://localhost:8080/v1/messages \
  -H "Authorization: Bearer $NOTI_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"event_id":"report-001","service":"checkout","environment":"development","title":"Report complete","body":"The daily report is ready.","level":"warn"}'
```

Set `level` to `crit`, `error`, `warn`, or `info` to classify a message. If you omit `level`, the message uses `info`. The levels match incident severity 1 to 4. In Slack, `crit`, `error`, and `warn` messages start with a level marker such as `🔴 CRIT`. The marker is also in the notification preview. `info` messages have no marker.

Identical messages from the same client within 60 seconds share one delivery. The server records each duplicate event ID and updates the Slack message with the received count. The server applies the service route and sends the message to the configured destinations. Set `notify_users` to `true` to mention the configured Slack group members. A mention request does not prove device delivery. Reuse an event ID only for an identical retry.

The Slack confirmation button records the account, action time, channel, message ID, and message snapshot. A confirmation records a button action. It does not prove that the recipient read or understood the content. After a confirmation, the Slack message shows the accounts that confirmed it below the button.

## Move a Slack incoming webhook sender

A sender that posts to a Slack incoming webhook can post the same payload to noti-mesh. Change only the URL:

```sh
curl "http://localhost:8080/hooks/slack/$NOTI_CLIENT_TOKEN?environment=production" \
  -H 'Content-Type: application/json' \
  -d '{"text":"Nightly export failed","attachments":[{"color":"danger","text":"3 rows rejected"}]}'
```

1. Create a client token for the service in the admin UI.
2. Copy the Slack-compatible URL that the admin UI shows with the new token.
3. If the sender uses an environment other than `production`, add `?environment=<name>` to the URL.
4. Replace the `hooks.slack.com` URL in the sender with this URL.

The URL contains the token. Keep it secret like the token. Rotate the token in the admin UI if the URL leaks.

The server converts the payload into an ordinary message. The service comes from the client token, so the service route and duplicate handling apply as for `POST /v1/messages`:

- The first line of text becomes the title. The other lines become the body.
- If the payload has `blocks`, the server reads the text of `header`, `section`, and `context` blocks. Otherwise it reads `text`.
- The server adds the `pretext`, `title`, `title_link`, `text`, and `fields` of each attachment to the body. The `title_link` URL goes on its own line after the title.
- An attachment color of `danger` sets the level to `error`. A color of `warning` sets `warn`. Other colors keep `info`.
- `<!here>`, `<!channel>`, and `<!everyone>` set `notify_users` to `true`. The server removes these mentions from the text.
- Slack references such as `<https://example.com|label>` and `<@U123>` become plain text.
- The server removes the `*bold*`, `_italic_`, and `~strike~` markers. It keeps the characters in words such as `snake_case`, in `2 * 3`, and in code spans.

The server keeps the original payload with the message. It does not copy the layout, buttons, `username`, `icon_emoji`, or `channel` of the payload. The service route selects the Slack channel.

The responses match Slack. A stored message returns `200` with the body `ok`. An unknown or revoked token returns `404 no_service`. A payload that is not a JSON object returns `400 invalid_payload`. A payload without text returns `400 no_text`. The server also accepts the form body `payload=<json>`.

## Receive alerts from Prometheus

Prometheus can send alerts to noti-mesh directly. noti-mesh accepts the Alertmanager API that Prometheus uses, so you do not need Alertmanager. Create a client token for the service in the admin UI, then set noti-mesh as the Alertmanager of Prometheus:

```yaml
global:
  external_labels:
    env: production
alerting:
  alertmanagers:
    - api_version: v2
      authorization:
        credentials: <client token>
      static_configs:
        - targets: ["noti-mesh:8080"]
```

Prometheus sends each active alert again at each resend interval, with a new end time (`endsAt`). The server uses these rules:

- Each alert opens one incident for the service of the token. Alerts with the same labels are the same alert.
- A resent alert only updates the end time. It does not create a new incident or a new message.
- If the end time is in the past, the alert is resolved. Prometheus sends this when the rule stops firing.
- If no resend arrives before the end time, the server resolves the incident. This occurs when Prometheus stops or cannot reach noti-mesh.
- If an alert has no end time, the server uses 5 minutes after it receives the alert.
- If the same alert fires again after it resolves, it opens a new incident.

The `severity`, `environment`, and `env` labels and the annotations work as for Alertmanager webhooks below. Prometheus adds `external_labels` to each alert, so `env` in `external_labels` sets the environment.

The response is `200` when the server accepts all alerts. If an alert has no labels, the server rejects that alert and returns `400` with the reason. It still accepts the other alerts.

noti-mesh does not group or inhibit alerts yet. Each alert opens its own incident.

## Receive Alertmanager alerts

Alertmanager can send alerts to noti-mesh as incident events. Create a client token for the service in the admin UI, then add a webhook receiver:

```yaml
receivers:
  - name: noti-mesh
    webhook_configs:
      - url: http://noti-mesh:8080/hooks/alertmanager?environment=production
        send_resolved: true
        http_config:
          authorization:
            credentials: <client token>
```

The token goes in the `Authorization` header, not in the URL. A configured source token returns `403`, because an alert does not name a service.

Each alert becomes one incident for the service of the token:

- The incident key is `<alertname>/<fingerprint>/<startsAt>`. If the same alert fires again after it resolves, it opens a new incident.
- A `firing` alert opens the incident. A `resolved` alert resolves it. Keep `send_resolved: true`, or incidents stay open.
- The `severity` label sets the priority. `critical`, `page`, `emergency`, `fatal`, and `p1` map to P1. `error`, `high`, `major`, and `p2` map to P2. `info`, `low`, `none`, and `p4` map to P4. Other values, and a missing label, map to P3.
- The `environment` or `env` label sets the environment. If the alert has neither label, the server uses the `environment` URL parameter. The default is `production`.
- The `summary`, `description`, or `message` annotation becomes the incident summary. `generatorURL` becomes the source link.
- Alerts in one notification share the `groupKey` as their correlation ID.

Alertmanager sends a firing alert again at each repeat interval. The server stores the first copy and ignores the repeats. The service route, escalation, and Slack buttons apply as for `POST /v1/events`.

The response lists the incident IDs. If an alert has no valid `startsAt`, or a resolved alert has no valid `endsAt`, the server rejects that alert and returns `422` with the reason. It still accepts the other alerts in the notification. Alertmanager does not retry a `4xx` response. It retries a `5xx` response, and the retry does not create duplicates.

A resolved alert for an alert that noti-mesh never received creates a resolved incident. The server does not send a message for it, because nobody received the alert. This can happen after Alertmanager restarts.


## Receive Grafana alerts

Grafana webhook contact points send the same alert fields as Alertmanager. noti-mesh handles them with the same rules. Create a client token for the service in the admin UI, then create a webhook contact point:

1. Set **URL** to `http://noti-mesh:8080/hooks/grafana?environment=production`.
2. Set **HTTP Method** to `POST` or `PUT`.
3. Set **Authorization Header - Scheme** to `Bearer`.
4. Set **Authorization Header - Credentials** to the client token.
5. Leave **Custom Payload** off.

Do not put the token in the URL. Grafana stores the URL in plain text. Do not use basic authentication, because noti-mesh reads only a Bearer token.

If the alert has a `panelURL` or `dashboardURL`, the incident links to it. Otherwise the incident links to `generatorURL`, the rule page. Grafana-managed rules have no `severity` label by default. Add a `severity` label to the rule, or the incident uses P3.

A custom payload template replaces the alert fields. noti-mesh rejects such a notification with a `4xx` status.

## Receive Uptime Kuma notifications

Create a client token for the service in the admin UI. In Uptime Kuma, add a notification:

1. Set **Notification Type** to `Webhook`.
2. Set **Post URL** to `http://noti-mesh:8080/hooks/uptime-kuma?environment=production&severity=critical`.
3. Set **Request Body** to `application/json`.
4. Turn on **Additional Headers** and enter `{"Authorization": "Bearer <client token>"}`.

A DOWN notification opens an incident. Later DOWN notifications for the same monitor go to that incident. An UP notification resolves it. PENDING, MAINTENANCE, and the test notification create nothing, and the response gives the reason in `ignored`.

Uptime Kuma has no severity, so the `severity` URL parameter sets the priority. It uses the same values as the Alertmanager `severity` label. The default is P3. The Uptime Kuma message becomes the incident summary, and the monitor URL becomes the source link.

## Receive Sentry alerts

Sentry integrations cannot add an `Authorization` header. The token goes in the URL, as for the Slack-compatible webhook. Keep the URL secret.

1. Create a client token for the service in the admin UI.
2. In Sentry, create an internal integration. Set **Webhook URL** to `http://noti-mesh:8080/hooks/sentry/<client token>?environment=production`.
3. Turn on **Alert Rule Action**, and select the `issue` webhook.
4. Add the integration as an action to the issue alert rules that must page.

An alert rule notification (`event_alert`) opens an incident for the issue. More alerts for the same issue go to that incident. If Sentry resolves the issue, the incident resolves. The issue webhook has no environment, so the server resolves the open incident for the issue in any environment. The server ignores other issue changes and resolutions of issues without an open incident. The event `level` sets the priority: `fatal` is P1, `error` is P2, `info` and `debug` are P4, and other values are P3. The event environment replaces the URL parameter when the event has one.

The server does not check the Sentry signature. The tests use the payload examples from the Sentry documentation. They do not use a live Sentry organization.

## Receive Datadog alerts

Datadog sends the payload that you write in the webhook integration. Use this payload, so that noti-mesh can tell a recovery from a new alert:

```json
{"id":"$ID","cycle":"$ALERT_CYCLE_KEY","transition":"$ALERT_TRANSITION","priority":"$ALERT_PRIORITY","date":"$LAST_UPDATED","title":"$EVENT_TITLE","link":"$LINK"}
```

1. Create a client token for the service in the admin UI.
2. In the Datadog webhook integration, set the URL to `http://noti-mesh:8080/hooks/datadog?environment=production`.
3. Set the payload to the template above.
4. Set the custom headers to `{"Authorization": "Bearer <client token>"}`.

All notifications with the same `$ALERT_CYCLE_KEY` go to one incident. `Recovered` resolves the incident. `P1` maps to P1, `P2` maps to P2, and `P4` and `P5` map to P4. If the monitor has no priority, warnings are P3 and other notifications are P2.

The tests use this template with sample values. They do not use a live Datadog account. Datadog does not document whether it escapes quotes in `$EVENT_TITLE`. If a title contains a quote, check the delivery with `dry_run=true` first.

## Behavior

- The source token identifies the sender. Sources have separate event and incident keys.
- PostgreSQL commits each event, incident change, and delivery job in one transaction.
- Duplicate retries do not increase the count. Different content with the same event ID returns HTTP 409.
- Late events remain in the event history. They do not reverse the current state.
- Priorities can rise before recovery. Repeat count alone does not raise priority.
- Summary intervals increase from one minute to 30 minutes. P3 and P4 incidents share channel summaries.
- P1 and P2 incidents escalate after five and 15 minutes without an owner. Later escalation attempts use 30-minute intervals.
- The green Slack button `담당하기` assigns an authorized group member and stops escalation. The `확인만 하기` button records a confirmation only.
- If the service rejects a button press, the person who pressed it receives a message that only that person can see. The message gives the reason, for example an unregistered account or a current owner.
- A severity rise requires a new acknowledgment. For P1 and P2, the service posts a new Slack message and mentions the group again.
- Recovery closes the incident and cancels escalation. If the first event of an incident is a recovery, the server records the incident and sends no message. Unknown routes use the default group and display an unclassified marker.
- Delivery jobs survive restarts. An uncertain Slack response can cause a duplicate message on retry.
- The delivery flow shows provider acceptance, retry state, and Slack confirmations. SMTP acceptance does not prove inbox delivery. Telegram API acceptance does not prove that a user read the message.
- Permanent provider errors stop automatic retries. After you correct the cause, an administrator can retry the failed delivery. The delivery flow records the administrator and retry time.
- AI uses selected structured fields. AI cannot suppress delivery, assign an owner, or close an incident.

## API

```text
POST /v1/events
POST /v1/messages
GET /v1/messages/{id}
POST /hooks/slack/{token}
POST /api/v2/alerts
POST /hooks/alertmanager
POST|PUT /hooks/grafana
POST /hooks/uptime-kuma
POST /hooks/sentry/{token}
POST /hooks/datadog
GET /v1/client
GET /v1/incidents
GET /v1/incidents/{id}
GET /v1/operations
GET /health
GET /health/delivery
POST /v1/slack/actions
```

API routes require a configured source bearer token or a client token, except the health routes, the Slack action route, the Slack-compatible webhook, and the Sentry webhook. The Slack-compatible webhook and the Sentry webhook take a client token in the URL. Slack actions require a valid Slack signature. `GET /v1/client` returns the service that a client token belongs to.

Configured source tokens permit organization-wide incident reads. Tokens from the admin UI only read incidents from their own client. The list returns up to 100 incidents.

The detail response contains events, delivery attempts, and incidents with a shared correlation ID. A correlation ID does not prove causation.

## API reference and test console

Open `/admin/api`, or select **API 문서·테스트** in the admin UI. The page shows the public API from `/openapi.json` and can send test requests. Add `?lang=en` or `?lang=ko` to select the language of the document. Without it, the page follows the browser language. The admin UI link uses the language selected in the admin UI.

1. Enter a client token or a source token in **Authentication**. In development mode, the page fills in the development source token.
2. Open an operation and select **Test Request**.
3. Keep the `dry_run=true` query parameter to preview the request.
4. Select **Send**.

With `dry_run=true`, the server authenticates, validates, and converts the request. It returns the stored form and the route, but it does not store or send anything. `POST /v1/events`, `POST /v1/messages`, and the `/hooks/*` routes accept `dry_run`. Use it to check how a payload from Slack or a monitoring tool converts. If you remove `dry_run`, the request creates a real incident or message and sends notifications.

The page loads Scalar from the server, with no CDN. The page allows scripts and connections only to the noti-mesh origin. The admin API is not in the document.

## Policy

Add a `[timing]` section to override these values:

```toml
[timing]
summary_seconds = [60, 120, 240, 480, 960, 1800]
digest_seconds = 1800
p1_ack_seconds = 300
p2_ack_seconds = 900
escalation_seconds = 1800
```

The first route that matches sets the responsible group and the priority floor. Unmatched events use the default group.

Set the top-level `language` key to `ko` or `en` to select the language of the text that the server writes. This includes Slack messages, Slack buttons and notices, email and Telegram bodies, post-incident drafts, and CSV headers. The default is `ko`. The admin UI has its own language selector.

Restart the service after a configuration change. Invalid group references or nonpositive intervals prevent startup.

## Maintenance silences

Open **점검 무음** in the admin UI to hold notifications for a service during planned work. Select the service, the environment, the duration, and the reason. If you leave the environment empty, the silence applies to all environments of the service. A silence starts at once and lasts from 30 minutes to 24 hours in the UI. The API accepts up to seven days.

During a silence:

- A new or reopened incident for the service is recorded, but the server sends no Slack, email, or Telegram message.
- When the silence ends or an administrator ends it, the server announces each held incident that is still open. Escalation starts at that time.
- A held incident that resolves during the silence is recorded only. Nobody receives a message for it.
- An incident that the server announced before the silence keeps its normal messages. This includes priority rises, escalation, and the resolution.
- Ordinary messages for the service are recorded and marked as held. The server does not send them later.

The admin UI lists each silence with its author, its reason, and the number of held incidents and messages. The incident detail shows a note while an incident is held.

```text
GET /v1/admin/silences
POST /v1/admin/silences {"service":"checkout","environment":"production","minutes":60,"reason":"DB maintenance"}
POST /v1/admin/silences/{id}/cancel
```

Read-only administrators can see silences, but they cannot start or end them.

## Email and Telegram destinations

Add provider settings and group destinations to the server configuration. The application reads credentials from the named environment variables.

```toml
[channels.email]
host = "smtp.example.com"
port = 587
from = "noti-mesh@example.com"
security = "starttls"
username_env = "SMTP_USERNAME"
password_env = "SMTP_PASSWORD"

[channels.telegram]
token_env = "TELEGRAM_BOT_TOKEN"
api_base = "https://api.telegram.org"

[[groups.operations.destinations]]
channel = "email"
address = "operations@example.com"
mode = "fallback"

[[groups.operations.destinations]]
channel = "telegram"
address = "-1001234567890"
mode = "fallback"
```

Use `security = "tls"` for SMTP over TLS or `security = "starttls"` for mandatory STARTTLS. Omit both SMTP credential fields for an unauthenticated relay.

The `local` SMTP mode permits plaintext only on loopback in development mode. Telegram uses HTTPS outside local development.

A `fallback` destination waits until Slack reports a failure. A `parallel` destination sends alongside Slack. An `unacknowledged` destination sends when an incident escalates to that destination's group.

Set `unacknowledged` on the backup group to send after a P1 or P2 acknowledgment deadline. Ordinary messages do not use the `unacknowledged` mode.

If a group does not use Slack, set `slack_enabled = false` and configure at least one `parallel` destination. Do not configure `fallback` for that group.

The SMTP username, password, and Telegram token use environment variables. The default Compose file forwards `SMTP_USERNAME`, `SMTP_PASSWORD`, and `TELEGRAM_BOT_TOKEN`.

If Slack accepts a message but its response is lost, fallback delivery can overlap with a later Slack retry. The delivery flow records each channel result.

## Forward a Telegram channel

The Telegram forwarder reads new posts of one Telegram channel and sends them to `/v1/messages`. Use it for a channel that you can read but cannot manage, for example a private announcement channel. A Telegram bot cannot read such a channel, so the forwarder logs in as a user account.

1. Create a separate Telegram account for the forwarder. Do not use your personal account.
2. Join the channel with that account.
3. In that account, set the account self-destruct period to the maximum value.
4. Get the `api_id` and `api_hash` of that account at `https://my.telegram.org`.
5. In the admin UI, create a client token for the service that the forwarder uses.
6. Add `TELEGRAM_API_ID`, `TELEGRAM_API_HASH`, `TELEGRAM_CHANNEL_ID`, and `NOTI_FORWARDER_TOKEN` to `.env`.
7. Log in once. Telegram sends the login code to the app of the forwarder account.
8. Start the forwarder.

For the `make dev` stack, use these commands. Add `-f compose.telegram.yaml` to the Compose command of other stacks.

```sh
docker compose --project-name noti-mesh-slack-test -f compose.dev.yaml -f compose.slack-test.yaml -f compose.telegram.yaml run --rm -it telegram-forwarder login
docker compose --project-name noti-mesh-slack-test -f compose.dev.yaml -f compose.slack-test.yaml -f compose.telegram.yaml up -d --build telegram-forwarder
```

The login command prints the channel title when the account can read the channel. The channel ID is the number in a post link `https://t.me/c/<channel ID>/<post>`.

| Variable | Default | Use |
|---|---|---|
| `NOTI_SERVICE` | `telegram-channel` | Message service. It must match the service of the client token. |
| `NOTI_ENVIRONMENT` | `mainnet` | Message environment. Add a route for this service and environment to select the destination group. |
| `NOTI_URL` | `http://app:8080` | noti-mesh address inside the Compose network. |

The first line of a post becomes the message title. The other lines become the body, followed by a link to the original post. The title keeps at most 200 characters, and the body keeps at most 2,000 characters. The event ID is `tg-<channel ID>-<post ID>`, so a post is never delivered twice.

The forwarder sets the message level from the post text. The match ignores case.

| Level | Text in the post |
|---|---|
| `crit` | `halt`, `halted`, `emergency`, `exploit`, `critical` as whole words |
| `warn` | `action required`, a word that starts with `upgrade`, or `⚠️` |
| `info` | Any other text |

The first start records the newest post and forwards only later posts. After a restart, the forwarder sends the posts that arrived while it was stopped. A network error, HTTP 429, HTTP 5xx, HTTP 401, or HTTP 403 stops the current pass. The forwarder tries again at the next post or after five minutes. Other HTTP 4xx responses skip that post. Posts without text and edited posts are not forwarded.

The `telegram-session` volume keeps the login session and the last forwarded post ID. The session gives full access to the forwarder account, so protect the volume like a password. `/health/delivery` does not check the forwarder. Read `docker compose logs telegram-forwarder` to see its state.

## Health checks and watchdog

The `/health` endpoint checks PostgreSQL and the required worker heartbeats. The container healthcheck uses this endpoint.

The `/health/delivery` endpoint also checks the delivery path. It returns HTTP 503 and a `problems` list for these conditions:

- The Slack token fails `auth.test` or belongs to a different workspace.
- The bot is not a member of a configured channel, or the channel is archived.
- Slack Socket Mode is configured and stays disconnected for more than one minute.
- A delivery failed permanently in the last 24 hours and was not retried successfully.
- A delivery failed three or more times, or waits more than five minutes past its due time.

The service checks Slack at startup and every five minutes. A Slack check failure does not stop the service, so event intake continues during a Slack outage. If the bot cannot read channel information, the response shows a warning and stays healthy.

A successful check does not prove that a recipient read a message. Email and Telegram provider credentials are only checked when a delivery runs. Inspect `/v1/operations` for overdue jobs and delivery failures.

Run the watchdog on a different host or monitoring system. Set `NOTI_WATCHDOG_WEBHOOK` to an independent HTTPS webhook that accepts a JSON `text` field.

```sh
python3 scripts/watchdog.py --health-url https://noti.example.com/health/delivery
```

The watchdog checks every 30 seconds by default. It reports the first failure and the next recovery. It retries a failed webhook on the next check. Each watchdog restart can report a current outage again. Use `--once` for a single check. A failed check returns exit code 1.

## Admin web UI

Open `/admin` and enter an admin token. The browser keeps the token in memory only. A page reload requires another connection.

Use the selectors at the top right to change the language and the theme. The UI is available in Korean and English, and it follows the browser language until you select one. The theme follows the operating system until you select Light or Dark. The browser keeps both choices. Text from senders, such as incident summaries, stays as sent.

The UI supports incident search, state and priority filters, pagination, event history, delivery history, and responsible user assignment.

The incident view shows the received events and messages for each level in the last hour and the last 24 hours. It also shows the open and unacknowledged incidents for each level. Select a level to filter the incident list by that priority. Received counts use the severity that the source sent. Open incident counts use the priority after the route rules. The UI refreshes the visible view every two seconds.

The admin token permits assignment to registered responsible group members. Each assignment records the selected Slack user and its admin origin.

The server rejects assignment if the incident state or sequence changes. Refresh the incident before another assignment attempt.

The incident detail can produce a post-incident draft in Korean Markdown. Select **초안 복사** or **Markdown 받기**. The draft lists the received events, Slack posts, email and Telegram acceptance, assignments, and confirmations in time order. It also gives the time from the first event to the first Slack post, the assignment, and the resolution. Write the cause, the impact, and the follow-up actions yourself. The draft does not use AI. `GET /v1/admin/incidents/{id}/report?offset_minutes=540` returns the same draft. The `offset_minutes` parameter sets the time zone of the draft in minutes east of UTC. The default is UTC.

Group membership and policy settings are read-only in the UI. Change the TOML file and restart the service to apply policy changes.

The delivery view can export delivery evidence as CSV. Select a date range of up to 92 days and select **CSV 받기**. The file has one row for each incident that started in the range, up to 10,000 rows. If you need more rows, export a shorter range. Each row tells whether a maintenance silence covered the start of the incident. It also gives the first event, the first Slack post, the Slack post and failure counts, the mentioned users, the email and Telegram acceptance counts, the first confirmation, the first assignment, and the resolution. Email and Telegram counts show provider acceptance only. They do not prove that a person read the message. The file starts with a UTF-8 byte order mark for spreadsheet apps. A cell that starts with `=`, `+`, `-`, or `@` gets a leading `'`, so that a spreadsheet does not run sender text as a formula. `GET /v1/admin/evidence.csv?from=2026-11-01&to=2026-11-30&offset_minutes=540` returns the same file.


## Client tokens

Use the admin UI to create a client token for one service. Copy the token at creation. Select `복사 완료` after you copy it. The server stores only its hash.

Revoke or rotate a token in the admin UI. Rotation stops the old token and preserves the client identity. The configured source tokens remain available.

The server records token actions and accepted messages for each client. A shared admin credential does not identify the person who performs an action.

New events record their receipt time. Events from older versions do not have a receipt time.

## Named administrators

Use named administrator tokens to record individual actions. Add an entry for each administrator to the server configuration.

```toml
[admins.alice]
token_env = "NOTI_ADMIN_ALICE_TOKEN"
role = "admin"

[admins.bob]
token_env = "NOTI_ADMIN_BOB_TOKEN"
role = "viewer"
```

Set each named token in its environment variable. Use at least 32 characters and a distinct value for each token.

An `admin` can manage client tokens and assign incidents. A `viewer` can read admin pages but cannot perform those actions.

If you need a shared administrator credential, set `NOTI_ADMIN_TOKEN`. Shared and development actions do not identify an individual administrator.

Pass each named token variable to the application container. For Docker Compose, add a local override file:

```yaml
services:
  app:
    environment:
      NOTI_ADMIN_ALICE_TOKEN: ${NOTI_ADMIN_ALICE_TOKEN:?Set NOTI_ADMIN_ALICE_TOKEN}
      NOTI_ADMIN_BOB_TOKEN: ${NOTI_ADMIN_BOB_TOKEN:?Set NOTI_ADMIN_BOB_TOKEN}
```

Use HTTPS for admin access outside localhost.

## Limits

The first version supports one organization. On-call rotation schedules remain outside this version.

The API serializes event changes through a PostgreSQL transaction lock. Measure throughput before a large deployment.

Slack, email, Telegram, and AI credentials require live acceptance tests. The tests use local mocks and do not prove live permissions or network access.

## License

noti-mesh is available under the [MIT License](LICENSE).

The repository includes these third-party assets. Each keeps its own MIT license:

- jstyle kit in `web/vendor/`, with the license in `web/vendor/LICENSE`.
- Scalar API Reference 1.75.1 in `web/vendor/scalar/`, with the license in `web/vendor/scalar/LICENSE`.
