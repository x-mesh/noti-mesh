use super::*;
use crate::policy::{
    Channel, Config, DeliveryMode, Destination, EmailConfig, Route, TelegramConfig,
};
use crate::test_support::{Fixture, NAMED_ADMIN, VIEWER, example_config, grant_admins};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt;

fn configure_destinations(config: &mut Config, smtp_port: u16, telegram_url: &str) {
    config.channels.email = Some(EmailConfig {
        host: "127.0.0.1".into(),
        port: smtp_port,
        from: "noti@example.test".into(),
        security: SmtpSecurity::Local,
        username_env: None,
        password_env: None,
    });
    config.channels.telegram = Some(TelegramConfig {
        token_env: "TEST_TELEGRAM_TOKEN".into(),
        api_base: telegram_url.into(),
    });
    let fallback_email = Destination {
        channel: Channel::Email,
        address: "fallback@example.test".into(),
        mode: DeliveryMode::Fallback,
    };
    let fallback_telegram = Destination {
        channel: Channel::Telegram,
        address: "-111".into(),
        mode: DeliveryMode::Fallback,
    };
    let parallel_email = Destination {
        channel: Channel::Email,
        address: "parallel@example.test".into(),
        mode: DeliveryMode::Parallel,
    };
    let parallel_telegram = Destination {
        channel: Channel::Telegram,
        address: "-222".into(),
        mode: DeliveryMode::Parallel,
    };
    let unack_email = Destination {
        channel: Channel::Email,
        address: "unack@example.test".into(),
        mode: DeliveryMode::Unacknowledged,
    };
    let unack_telegram = Destination {
        channel: Channel::Telegram,
        address: "-333".into(),
        mode: DeliveryMode::Unacknowledged,
    };
    config.groups.get_mut("operations").unwrap().destinations = vec![
        fallback_email,
        fallback_telegram,
        parallel_email.clone(),
        parallel_telegram.clone(),
    ];
    config.groups.get_mut("backup").unwrap().destinations = vec![unack_email, unack_telegram];
    let mut no_slack = config.groups["operations"].clone();
    no_slack.slack_enabled = false;
    no_slack.channel.clear();
    no_slack.backup = "operations".into();
    no_slack.destinations = vec![parallel_email, parallel_telegram];
    let mut retry_group = no_slack.clone();
    retry_group.destinations = vec![Destination {
        channel: Channel::Telegram,
        address: "-444".into(),
        mode: DeliveryMode::Parallel,
    }];
    config.groups.insert("retry_only".into(), retry_group);
    config.routes.push(Route {
        service: "telegram-retry".into(),
        environment: "dev".into(),
        group: "retry_only".into(),
        priority_floor: 4,
    });
    let mut reject_group = no_slack.clone();
    reject_group.destinations = vec![Destination {
        channel: Channel::Email,
        address: "reject@example.test".into(),
        mode: DeliveryMode::Parallel,
    }];
    config.groups.insert("reject_only".into(), reject_group);
    config.routes.push(Route {
        service: "email-rejected".into(),
        environment: "dev".into(),
        group: "reject_only".into(),
        priority_floor: 4,
    });
    config.groups.insert("no_slack".into(), no_slack);
    config.routes.push(Route {
        service: "without-slack".into(),
        environment: "dev".into(),
        group: "no_slack".into(),
        priority_floor: 4,
    });
}

type SmtpMessages = Arc<Mutex<Vec<(Vec<String>, String)>>>;
struct Harness {
    fixture: Fixture,
    smtp_messages: SmtpMessages,
    reject_recipient: Arc<AtomicBool>,
    telegram_messages: Arc<Mutex<Vec<Value>>>,
    slack_calls: Arc<AtomicUsize>,
    slack_permanent: Arc<AtomicBool>,
    servers: Vec<tokio::task::JoinHandle<()>>,
}
impl Harness {
    async fn start() -> Option<Self> {
        let smtp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let smtp_port = smtp_listener.local_addr().unwrap().port();
        let smtp_messages = Arc::new(Mutex::new(Vec::<(Vec<String>, String)>::new()));
        let smtp_capture = smtp_messages.clone();
        let reject_recipient = Arc::new(AtomicBool::new(true));
        let reject_capture = reject_recipient.clone();
        let smtp_server = tokio::spawn(async move {
            loop {
                let (socket, _) = smtp_listener.accept().await.unwrap();
                let capture = smtp_capture.clone();
                let reject = reject_capture.clone();
                tokio::spawn(async move {
                    let (read, mut write) = socket.into_split();
                    let mut read = BufReader::new(read);
                    if write.write_all(b"220 localhost ESMTP\r\n").await.is_err() {
                        return;
                    }
                    let mut recipients = Vec::new();
                    let mut data = String::new();
                    let mut in_data = false;
                    loop {
                        let mut line = String::new();
                        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                            return;
                        }
                        if in_data {
                            if line == ".\r\n" {
                                capture
                                    .lock()
                                    .unwrap()
                                    .push((recipients.clone(), data.clone()));
                                in_data = false;
                                if write.write_all(b"250 queued\r\n").await.is_err() {
                                    return;
                                }
                            } else {
                                data.push_str(&line);
                            }
                            continue;
                        }
                        let command = line.to_ascii_uppercase();
                        let response = if command.starts_with("EHLO") {
                            b"250-localhost\r\n250-8BITMIME\r\n250 SMTPUTF8\r\n".as_slice()
                        } else if command.starts_with("RCPT TO:") {
                            if command.contains("REJECT@EXAMPLE.TEST")
                                && reject.load(Ordering::SeqCst)
                            {
                                b"550 recipient rejected\r\n".as_slice()
                            } else {
                                recipients.push(line.clone());
                                b"250 recipient ok\r\n".as_slice()
                            }
                        } else if command.starts_with("DATA") {
                            in_data = true;
                            b"354 end with dot\r\n".as_slice()
                        } else if command.starts_with("QUIT") {
                            let _ = write.write_all(b"221 bye\r\n").await;
                            return;
                        } else {
                            b"250 ok\r\n".as_slice()
                        };
                        if write.write_all(response).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        let telegram_messages = Arc::new(Mutex::new(Vec::<Value>::new()));
        let telegram_capture = telegram_messages.clone();
        let telegram_failures = Arc::new(AtomicUsize::new(0));
        let telegram_failures_capture = telegram_failures.clone();
        let telegram_router = Router::new().route(
            "/bot123:test/sendMessage",
            post(move |Json(body): Json<Value>| {
                let capture = telegram_capture.clone();
                let failures = telegram_failures_capture.clone();
                async move {
                    let fail =
                        body["chat_id"] == "-444" && failures.fetch_add(1, Ordering::SeqCst) == 0;
                    capture.lock().unwrap().push(body);
                    if fail {
                        (
                            StatusCode::TOO_MANY_REQUESTS,
                            Json(json!({"ok":false,"parameters":{"retry_after":3}})),
                        )
                    } else {
                        (
                            StatusCode::OK,
                            Json(json!({"ok":true,"result":{"message_id":42}})),
                        )
                    }
                }
            }),
        );
        let telegram_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let telegram_url = format!("http://{}", telegram_listener.local_addr().unwrap());
        let telegram_server = tokio::spawn(async move {
            axum::serve(telegram_listener, telegram_router)
                .await
                .unwrap();
        });
        let slack_calls = Arc::new(AtomicUsize::new(0));
        let slack_count = slack_calls.clone();
        let slack_permanent = Arc::new(AtomicBool::new(false));
        let slack_permanent_capture = slack_permanent.clone();
        let slack_router = Router::new().route(
            "/{method}",
            post(move || {
                let calls = slack_count.clone();
                let permanent = slack_permanent_capture.clone();
                async move {
                    let count = calls.fetch_add(1, Ordering::SeqCst);
                    if permanent.load(Ordering::SeqCst) {
                        (
                            StatusCode::OK,
                            Json(json!({"ok":false,"error":"invalid_auth"})),
                        )
                    } else if count == 0 {
                        (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"ok":false})))
                    } else {
                        (StatusCode::OK, Json(json!({"ok":true,"ts":"999.001"})))
                    }
                }
            }),
        );
        let slack_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let slack_url = format!("http://{}", slack_listener.local_addr().unwrap());
        let slack_server = tokio::spawn(async move {
            axum::serve(slack_listener, slack_router).await.unwrap();
        });
        let servers = vec![smtp_server, telegram_server, slack_server];
        let Some(fixture) = Fixture::with_app(|app| {
            grant_admins(app);
            app.development_mode = true;
            app.config.api_url = slack_url;
            configure_destinations(&mut app.config, smtp_port, &telegram_url);
            let smtp = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
                .port(smtp_port)
                .timeout(Some(Duration::from_secs(3)))
                .build();
            app.channel_providers = Arc::new(Providers {
                email: Some(EmailSender {
                    transport: smtp,
                    from: "noti@example.test".parse().unwrap(),
                }),
                telegram: Some(TelegramSender {
                    token: "123:test".into(),
                    base: telegram_url.clone(),
                }),
            });
        })
        .await
        else {
            servers.iter().for_each(|server| server.abort());
            return None;
        };
        Some(Self {
            fixture,
            smtp_messages,
            reject_recipient,
            telegram_messages,
            slack_calls,
            slack_permanent,
            servers,
        })
    }
    fn app(&self) -> Arc<App> {
        self.fixture.app.clone()
    }
    fn skip_first_slack_failure(&self) {
        self.slack_calls.store(1, Ordering::SeqCst);
    }
    async fn close(self) {
        self.servers.iter().for_each(|server| server.abort());
        self.fixture.close().await;
    }
}

fn source_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        "Bearer test-token-with-at-least-32-characters"
            .parse()
            .unwrap(),
    );
    headers
}
async fn ingest_message(app: &Arc<App>, event: &str, service: &str) -> String {
    let message = serde_json::from_value::<crate::messages::Message>(json!({"event_id":event,"service":service,"environment":"dev","title":"Test message","body":event})).unwrap();
    let (_, Json(created)) = crate::messages::ingest(
        State(app.clone()),
        axum::extract::Query(Default::default()),
        source_headers(),
        Json(message),
    )
    .await
    .unwrap();
    created["message_id"].as_str().unwrap().to_owned()
}
async fn ingest_alert(app: &Arc<App>, event: Value) -> String {
    let event = serde_json::from_value::<crate::Event>(event).unwrap();
    let (_, Json(created)) = crate::ingest(
        State(app.clone()),
        axum::extract::Query(Default::default()),
        source_headers(),
        Json(event),
    )
    .await
    .unwrap();
    created["incident_id"].as_str().unwrap().to_owned()
}
fn secured_admin(app: &App) -> Router {
    let mut secured = app.clone();
    secured.development_mode = false;
    crate::admin::routes().with_state(Arc::new(secured))
}
async fn admin_call(
    router: &Router,
    method: &str,
    uri: &str,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = axum::http::Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = router
        .clone()
        .oneshot(request.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[test]
fn provider_configuration_rejects_unsafe_transports() {
    let mut config = example_config();
    configure_destinations(&mut config, 2525, "http://127.0.0.1:9");
    config.validate().unwrap();
    assert!(Providers::configure(&config.channels, false).is_err());
    let only_telegram = Channels {
        email: None,
        telegram: config.channels.telegram.clone(),
    };
    assert!(Providers::configure(&only_telegram, false).is_err());
    let mut invalid = config.clone();
    invalid.groups.get_mut("no_slack").unwrap().destinations[0].mode = DeliveryMode::Fallback;
    assert!(invalid.validate().is_err());
}

#[tokio::test]
async fn unreachable_smtp_server_is_retried() {
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let unavailable = Providers {
        email: Some(EmailSender {
            transport: AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous("127.0.0.1")
                .port(closed_port)
                .timeout(Some(Duration::from_secs(1)))
                .build(),
            from: "noti@example.test".parse().unwrap(),
        }),
        telegram: None,
    };
    assert!(matches!(
        send_email(&unavailable, "nobody@example.test", "Test", "Body").await,
        DeliveryOutcome::Retry(_, _)
    ));
}

#[tokio::test]
async fn slack_failure_activates_message_fallback_channels() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    let first = ingest_message(&app, "channels-fallback", "checkout").await;
    let states: Vec<(String, String)> =
        sqlx::query_as("SELECT mode,state FROM channel_deliveries WHERE message_id=$1 ORDER BY id")
            .bind(&first)
            .fetch_all(&app.db)
            .await
            .unwrap();
    assert_eq!(states.len(), 4);
    assert_eq!(
        states
            .iter()
            .filter(|(mode, state)| mode == "fallback" && state == "standby")
            .count(),
        2
    );
    assert_eq!(
        states
            .iter()
            .filter(|(mode, state)| mode == "parallel" && state == "pending")
            .count(),
        2
    );
    tick(&app).await.unwrap();
    tick(&app).await.unwrap();
    crate::messages::deliver(&app).await.unwrap();
    let (active,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE message_id=$1 AND mode='fallback' AND state='pending'").bind(&first).fetch_one(&app.db).await.unwrap();
    assert_eq!(active, 2);
    tick(&app).await.unwrap();
    tick(&app).await.unwrap();
    let (sent,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM channel_deliveries WHERE message_id=$1 AND state='sent'",
    )
    .bind(&first)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(sent, 4);
    let Json(client_status) = crate::messages::status(
        State(app.clone()),
        source_headers(),
        axum::extract::Path(first.clone()),
    )
    .await
    .unwrap();
    assert_eq!(client_status["channels"].as_array().unwrap().len(), 4);
    assert!(
        client_status["channels"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry.get("destination").is_none())
    );
    let router = crate::admin::routes().with_state(app.clone());
    let (status, payload) = admin_call(&router, "GET", "/v1/admin/flows", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(payload["channels"].as_array().unwrap().iter().any(
        |entry| entry["delivery"]["message_id"] == first.as_str()
            && entry["delivery"]["channel"] == "telegram"
            && entry["delivery"]["state"] == "sent"
    ));
    assert!(!payload.to_string().contains("123:test"));
    let (_, payload) = admin_call(&router, "GET", "/v1/admin/messages", None).await;
    let record = payload["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["message"]["id"] == first.as_str())
        .unwrap();
    assert_eq!(record["channels"].as_array().unwrap().len(), 4);
    {
        let smtp = h.smtp_messages.lock().unwrap();
        assert!(
            smtp.iter()
                .any(|(r, _)| r.iter().any(|v| v.contains("fallback@example.test")))
        );
        assert!(
            smtp.iter()
                .any(|(r, _)| r.iter().any(|v| v.contains("parallel@example.test")))
        );
    }
    {
        let telegram = h.telegram_messages.lock().unwrap();
        assert!(telegram.iter().any(|v| v["chat_id"] == "-111"));
        assert!(telegram.iter().any(|v| v["chat_id"] == "-222"));
    }
    h.close().await;
}

#[tokio::test]
async fn successful_slack_delivery_cancels_message_fallback() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.skip_first_slack_failure();
    let app = h.app();
    let second = ingest_message(&app, "channels-success", "checkout").await;
    crate::messages::deliver(&app).await.unwrap();
    let (cancelled,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE message_id=$1 AND mode='fallback' AND state='cancelled'").bind(&second).fetch_one(&app.db).await.unwrap();
    assert_eq!(cancelled, 2);
    h.close().await;
}

#[tokio::test]
async fn permanent_slack_message_failure_waits_for_admin_retry() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    h.slack_permanent.store(true, Ordering::SeqCst);
    let permanent_message = ingest_message(&app, "slack-permanent-message", "checkout").await;
    crate::messages::deliver(&app).await.unwrap();
    let (failed, attempts): (bool, i32) =
        sqlx::query_as("SELECT failed_at IS NOT NULL,attempts FROM gateway_messages WHERE id=$1")
            .bind(&permanent_message)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(failed);
    assert_eq!(attempts, 1);
    let (fallback,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE message_id=$1 AND mode='fallback' AND state='pending'").bind(&permanent_message).fetch_one(&app.db).await.unwrap();
    assert_eq!(fallback, 2);
    sqlx::query("UPDATE gateway_messages SET due_at=now() WHERE id=$1")
        .bind(&permanent_message)
        .execute(&app.db)
        .await
        .unwrap();
    let calls = h.slack_calls.load(Ordering::SeqCst);
    crate::messages::deliver(&app).await.unwrap();
    assert_eq!(h.slack_calls.load(Ordering::SeqCst), calls);
    let router = secured_admin(&app);
    let path = format!("/v1/admin/messages/{permanent_message}/retry");
    assert_eq!(
        admin_call(&router, "POST", &path, Some(VIEWER)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        admin_call(&router, "POST", &path, Some(NAMED_ADMIN))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, audit) = admin_call(&router, "GET", "/v1/admin/retries", Some(NAMED_ADMIN)).await;
    assert!(
        audit["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["target_kind"] == "slack_message"
                && item["target_id"] == permanent_message.as_str()
                && item["actor"] == "alice")
    );
    h.slack_permanent.store(false, Ordering::SeqCst);
    crate::messages::deliver(&app).await.unwrap();
    let (sent, failed): (bool, bool) = sqlx::query_as(
        "SELECT sent_at IS NOT NULL,failed_at IS NOT NULL FROM gateway_messages WHERE id=$1",
    )
    .bind(&permanent_message)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert!(sent && !failed);
    let (cancelled,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE message_id=$1 AND mode='fallback' AND state='cancelled'").bind(&permanent_message).fetch_one(&app.db).await.unwrap();
    assert_eq!(cancelled, 2);
    h.close().await;
}

#[tokio::test]
async fn permanent_slack_incident_failure_waits_for_admin_retry() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    h.slack_permanent.store(true, Ordering::SeqCst);
    let permanent_alert = ingest_alert(&app, json!({"event_id":"slack-permanent-alert","service":"checkout","environment":"production","incident_key":"Permanent alert","sequence":1,"severity":1,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"})).await;
    crate::workflow::tick(&app).await.unwrap();
    let (outbox_id, failed): (i64, bool) =
        sqlx::query_as("SELECT id,failed_at IS NOT NULL FROM outbox WHERE incident_id=$1")
            .bind(&permanent_alert)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(failed);
    let (fallback,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE incident_id=$1 AND mode='fallback' AND state='pending'").bind(&permanent_alert).fetch_one(&app.db).await.unwrap();
    assert_eq!(fallback, 2);
    let router = secured_admin(&app);
    let path = format!("/v1/admin/slack-deliveries/{outbox_id}/retry");
    assert_eq!(
        admin_call(&router, "POST", &path, Some(VIEWER)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        admin_call(&router, "POST", &path, Some(NAMED_ADMIN))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    h.slack_permanent.store(false, Ordering::SeqCst);
    crate::workflow::tick(&app).await.unwrap();
    let (sent, failed): (bool, bool) =
        sqlx::query_as("SELECT sent_at IS NOT NULL,failed_at IS NOT NULL FROM outbox WHERE id=$1")
            .bind(outbox_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(sent && !failed);
    h.close().await;
}

#[tokio::test]
async fn recovered_slack_cancels_fallback_held_by_crashed_worker() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    let race = ingest_message(&app, "channels-recovered-race", "checkout").await;
    crate::messages::deliver(&app).await.unwrap();
    sqlx::query("UPDATE channel_deliveries SET lease_token='crashed',lease_until=now()+interval '30 seconds' WHERE message_id=$1 AND mode='fallback' AND channel='email'").bind(&race).execute(&app.db).await.unwrap();
    sqlx::query("UPDATE gateway_messages SET due_at=now() WHERE id=$1")
        .bind(&race)
        .execute(&app.db)
        .await
        .unwrap();
    crate::messages::deliver(&app).await.unwrap();
    sqlx::query("UPDATE channel_deliveries SET lease_until=now()-interval '1 second' WHERE message_id=$1 AND mode='fallback' AND channel='email'").bind(&race).execute(&app.db).await.unwrap();
    tick(&app).await.unwrap();
    let (state,):(String,)=sqlx::query_as("SELECT state FROM channel_deliveries WHERE message_id=$1 AND mode='fallback' AND channel='email'").bind(&race).fetch_one(&app.db).await.unwrap();
    assert_eq!(state, "cancelled");
    h.close().await;
}

#[tokio::test]
async fn digest_failure_activates_incident_fallback() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    let digest = ingest_alert(&app, json!({"event_id":"channels-digest","service":"checkout","environment":"dev","incident_key":"Digest fallback test","sequence":1,"severity":3,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"})).await;
    sqlx::query("UPDATE outbox SET due_at=now() WHERE incident_id=$1")
        .bind(&digest)
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE channel_deliveries SET due_at=now() WHERE incident_id=$1")
        .bind(&digest)
        .execute(&app.db)
        .await
        .unwrap();
    crate::workflow::tick(&app).await.unwrap();
    let (fallback,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE incident_id=$1 AND mode='fallback' AND state='pending'").bind(&digest).fetch_one(&app.db).await.unwrap();
    assert_eq!(fallback, 2);
    for _ in 0..4 {
        tick(&app).await.unwrap();
    }
    let (sent,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM channel_deliveries WHERE incident_id=$1 AND state='sent'",
    )
    .bind(&digest)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(sent, 4);
    let router = crate::admin::routes().with_state(app.clone());
    let (_, payload) = admin_call(&router, "GET", "/v1/admin/flows", None).await;
    let record = payload["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["incident_id"] == digest.as_str())
        .unwrap();
    assert_eq!(record["channels"].as_array().unwrap().len(), 4);
    sqlx::query("UPDATE outbox SET due_at=now() WHERE incident_id=$1 AND sent_at IS NULL")
        .bind(&digest)
        .execute(&app.db)
        .await
        .unwrap();
    crate::workflow::tick(&app).await.unwrap();
    let (sent,): (bool,) =
        sqlx::query_as("SELECT sent_at IS NOT NULL FROM outbox WHERE incident_id=$1 LIMIT 1")
            .bind(&digest)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(sent);
    h.close().await;
}

#[tokio::test]
async fn message_without_slack_uses_only_channels() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    let third = ingest_message(&app, "channels-no-slack", "without-slack").await;
    let (disabled,): (bool,) =
        sqlx::query_as("SELECT slack_disabled FROM gateway_messages WHERE id=$1")
            .bind(&third)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert!(disabled);
    tick(&app).await.unwrap();
    tick(&app).await.unwrap();
    let (sent,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM channel_deliveries WHERE message_id=$1 AND state='sent'",
    )
    .bind(&third)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(sent, 2);
    h.close().await;
}

#[tokio::test]
async fn telegram_rate_limit_is_retried_without_leaking_the_token() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    let retry = ingest_message(&app, "channels-retry", "telegram-retry").await;
    tick(&app).await.unwrap();
    let failed=sqlx::query("SELECT state,attempts,last_error,extract(epoch FROM due_at-now())::float8 AS remaining FROM channel_deliveries WHERE message_id=$1").bind(&retry).fetch_one(&app.db).await.unwrap();
    assert_eq!(failed.get::<String, _>("state"), "pending");
    assert_eq!(failed.get::<i32, _>("attempts"), 1);
    assert!(failed.get::<f64, _>("remaining") > 1.0);
    assert!(!failed.get::<String, _>("last_error").contains("123:test"));
    sqlx::query("UPDATE channel_deliveries SET due_at=now() WHERE message_id=$1")
        .bind(&retry)
        .execute(&app.db)
        .await
        .unwrap();
    tick(&app).await.unwrap();
    let (state, provider_id): (String, Option<String>) = sqlx::query_as(
        "SELECT state,provider_message_id FROM channel_deliveries WHERE message_id=$1",
    )
    .bind(&retry)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(state, "sent");
    assert_eq!(provider_id.as_deref(), Some("42"));
    h.close().await;
}

#[tokio::test]
async fn rejected_email_waits_for_admin_retry() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    let rejected = ingest_message(&app, "channels-rejected", "email-rejected").await;
    tick(&app).await.unwrap();
    let (state,): (String,) =
        sqlx::query_as("SELECT state FROM channel_deliveries WHERE message_id=$1")
            .bind(&rejected)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    let (delivery_id,): (i64,) =
        sqlx::query_as("SELECT id FROM channel_deliveries WHERE message_id=$1")
            .bind(&rejected)
            .fetch_one(&app.db)
            .await
            .unwrap();
    let router = secured_admin(&app);
    let path = format!("/v1/admin/channel-deliveries/{delivery_id}/retry");
    assert_eq!(
        admin_call(&router, "POST", &path, Some(VIEWER)).await.0,
        StatusCode::FORBIDDEN
    );
    let (_, stats) = admin_call(&router, "GET", "/v1/admin", Some(NAMED_ADMIN)).await;
    assert_eq!(stats["delivery"]["failed"], 1);
    assert_eq!(
        admin_call(&router, "POST", &path, Some(NAMED_ADMIN))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    h.reject_recipient.store(false, Ordering::SeqCst);
    tick(&app).await.unwrap();
    let (state, attempts): (String, i32) =
        sqlx::query_as("SELECT state,attempts FROM channel_deliveries WHERE id=$1")
            .bind(delivery_id)
            .fetch_one(&app.db)
            .await
            .unwrap();
    assert_eq!((state.as_str(), attempts), ("sent", 1));
    h.close().await;
}

#[tokio::test]
async fn slack_disabled_low_priority_incidents_use_channels() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    let low = |event: &str, key: &str, sequence: i64| json!({"event_id":event,"service":"without-slack","environment":"dev","incident_key":key,"sequence":sequence,"severity":3,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"});
    let old = ingest_alert(&app, low("old-low-1", "old-low", 1)).await;
    sqlx::query("UPDATE outbox SET sent_at=now() WHERE incident_id=$1")
        .bind(&old)
        .execute(&app.db)
        .await
        .unwrap();
    ingest_alert(&app, low("old-low-2", "old-low", 2)).await;
    sqlx::query("UPDATE incidents SET next_summary=now()-interval '1 second' WHERE id=$1")
        .bind(&old)
        .execute(&app.db)
        .await
        .unwrap();
    crate::workflow::tick(&app).await.unwrap();
    let (old_jobs,): (i64,) = sqlx::query_as("SELECT count(*) FROM outbox WHERE incident_id=$1")
        .bind(&old)
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(old_jobs, 2);
    sqlx::query("UPDATE incidents SET state='resolved' WHERE id=$1")
        .bind(&old)
        .execute(&app.db)
        .await
        .unwrap();
    let current = ingest_alert(&app, low("current-low-1", "current-low", 1)).await;
    ingest_alert(&app, low("current-low-2", "current-low", 2)).await;
    sqlx::query("UPDATE incidents SET next_summary=now()-interval '1 second' WHERE id=$1")
        .bind(&current)
        .execute(&app.db)
        .await
        .unwrap();
    crate::workflow::tick(&app).await.unwrap();
    let (current_jobs,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM outbox WHERE incident_id=$1 AND slack_disabled AND sent_at IS NULL",
    )
    .bind(&current)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(current_jobs, 1);
    sqlx::query("UPDATE outbox SET due_at=now() WHERE incident_id=$1")
        .bind(&current)
        .execute(&app.db)
        .await
        .unwrap();
    sqlx::query("UPDATE channel_deliveries SET due_at=now() WHERE incident_id=$1")
        .bind(&current)
        .execute(&app.db)
        .await
        .unwrap();
    crate::workflow::tick(&app).await.unwrap();
    tick(&app).await.unwrap();
    tick(&app).await.unwrap();
    let (sent,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM channel_deliveries WHERE incident_id=$1 AND state='sent'",
    )
    .bind(&current)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(sent, 2);
    h.close().await;
}

#[tokio::test]
async fn unacknowledged_escalation_reaches_backup_channels() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.skip_first_slack_failure();
    let app = h.app();
    let incident = ingest_alert(&app, json!({"event_id":"channels-unack","service":"checkout","environment":"production","incident_key":"Unacknowledged test","sequence":1,"severity":1,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"})).await;
    sqlx::query("UPDATE incidents SET next_escalation=now()-interval '1 second' WHERE id=$1")
        .bind(&incident)
        .execute(&app.db)
        .await
        .unwrap();
    crate::workflow::tick(&app).await.unwrap();
    let (unack,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE incident_id=$1 AND mode='unacknowledged' AND state='pending'").bind(&incident).fetch_one(&app.db).await.unwrap();
    assert_eq!(unack, 2);
    for _ in 0..6 {
        tick(&app).await.unwrap();
    }
    let (sent,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE incident_id=$1 AND mode='unacknowledged' AND state='sent'").bind(&incident).fetch_one(&app.db).await.unwrap();
    assert_eq!(sent, 2);
    h.close().await;
}

#[tokio::test]
async fn acknowledgment_cancels_unacknowledged_deliveries() {
    let Some(h) = Harness::start().await else {
        return;
    };
    h.skip_first_slack_failure();
    let app = h.app();
    let cancelled = ingest_alert(&app, json!({"event_id":"channels-unack-cancel","service":"checkout","environment":"production","incident_key":"Acknowledged before delivery","sequence":1,"severity":1,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"})).await;
    sqlx::query("UPDATE incidents SET next_escalation=now()-interval '1 second' WHERE id=$1")
        .bind(&cancelled)
        .execute(&app.db)
        .await
        .unwrap();
    crate::workflow::tick(&app).await.unwrap();
    let (pending,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE incident_id=$1 AND mode='unacknowledged' AND state='pending'").bind(&cancelled).fetch_one(&app.db).await.unwrap();
    assert_eq!(pending, 2);
    sqlx::query("UPDATE incidents SET state='acknowledged' WHERE id=$1")
        .bind(&cancelled)
        .execute(&app.db)
        .await
        .unwrap();
    tick(&app).await.unwrap();
    let (stopped,):(i64,)=sqlx::query_as("SELECT count(*) FROM channel_deliveries WHERE incident_id=$1 AND mode='unacknowledged' AND state='cancelled'").bind(&cancelled).fetch_one(&app.db).await.unwrap();
    assert_eq!(stopped, 2);
    h.close().await;
}

#[tokio::test]
async fn removed_destination_cancels_unleased_deliveries() {
    let Some(h) = Harness::start().await else {
        return;
    };
    let app = h.app();
    let removed = ingest_message(&app, "channels-destination-removed", "checkout").await;
    sqlx::query("UPDATE channel_deliveries SET lease_until=now()+interval '30 seconds' WHERE id=(SELECT id FROM channel_deliveries WHERE message_id=$1 AND mode='parallel' ORDER BY id LIMIT 1)").bind(&removed).execute(&app.db).await.unwrap();
    let mut changed = (*app).clone();
    changed
        .config
        .groups
        .get_mut("operations")
        .unwrap()
        .destinations
        .clear();
    reconcile(&changed).await.unwrap();
    let (cancelled,pending):(i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE state='cancelled'),count(*) FILTER(WHERE state='pending') FROM channel_deliveries WHERE message_id=$1").bind(&removed).fetch_one(&app.db).await.unwrap();
    assert_eq!((cancelled, pending), (3, 1));
    sqlx::query("UPDATE channel_deliveries SET lease_until=now()-interval '1 second' WHERE message_id=$1 AND state='pending'").bind(&removed).execute(&app.db).await.unwrap();
    reconcile(&changed).await.unwrap();
    let (cancelled,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM channel_deliveries WHERE message_id=$1 AND state='cancelled'",
    )
    .bind(&removed)
    .fetch_one(&app.db)
    .await
    .unwrap();
    assert_eq!(cancelled, 4);
    h.close().await;
}
