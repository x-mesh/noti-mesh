use super::*;
use crate::test_support::{ADMIN_TOKEN, Fixture, NAMED_ADMIN, VIEWER, grant_admins};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use tower::ServiceExt;

const SOURCE_TOKEN: &str = "test-token-with-at-least-32-characters";

async fn send(router: &Router, event: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/events")
                .header("authorization", format!("Bearer {SOURCE_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(event.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"error":String::from_utf8_lossy(&bytes)})),
    )
}
async fn create_incident(router: &Router, event: Value) -> String {
    let (status, body) = send(router, event).await;
    body["incident_id"]
        .as_str()
        .unwrap_or_else(|| panic!("event was not accepted: {status} {body}"))
        .to_owned()
}
async fn api_request(
    router: &Router,
    path: &str,
    credential: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(if body.is_some() { "POST" } else { "GET" })
        .uri(path)
        .header("authorization", format!("Bearer {credential}"));
    let bytes = if let Some(body) = body {
        request = request.header("content-type", "application/json");
        body.to_string()
    } else {
        String::new()
    };
    let response = router
        .clone()
        .oneshot(request.body(Body::from(bytes)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
fn signed_action(payload: &Value) -> (HeaderMap, axum::body::Bytes) {
    use hmac::Mac;
    let encoded = serde_urlencoded::to_string([("payload", payload.to_string())]).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(b"secret").unwrap();
    mac.update(format!("v0:{now}:{encoded}").as_bytes());
    let mut headers = HeaderMap::new();
    headers.insert("x-slack-request-timestamp", now.parse().unwrap());
    headers.insert(
        "x-slack-signature",
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
            .parse()
            .unwrap(),
    );
    (headers, axum::body::Bytes::from(encoded))
}
fn first_event() -> Value {
    json!({"event_id":"event-0","service":"checkout","environment":"production","incident_key":"episode-1","sequence":0,"severity":2,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z","summary":"Payment gateway timed out","source_url":"https://example.com/traces/123","correlation_id":"shared-trace"})
}

type Calls = Arc<std::sync::Mutex<Vec<(String, Value)>>>;
async fn slack_mock() -> (String, Calls, tokio::task::JoinHandle<()>) {
    let calls: Calls = Arc::default();
    let captured = calls.clone();
    let mock = Router::new().route(
        "/{method}",
        post(move |Path(method): Path<String>, Json(body): Json<Value>| {
            let calls = captured.clone();
            async move {
                calls.lock().unwrap().push((method, body));
                Json(json!({"ok":true,"ts":"100.200"}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    (url, calls, server)
}
async fn strict_slack_mock(rate_limit_first: bool) -> (String, Calls, tokio::task::JoinHandle<()>) {
    let calls: Calls = Arc::default();
    let captured = calls.clone();
    let mock = Router::new().route(
        "/{method}",
        post(move |Path(method): Path<String>, Json(body): Json<Value>| {
            let calls = captured.clone();
            async move {
                let mut block_ids = std::collections::HashSet::new();
                let duplicate = body["blocks"].as_array().is_some_and(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|block| block["block_id"].as_str())
                        .any(|id| !block_ids.insert(id))
                });
                let first = {
                    let mut calls = calls.lock().unwrap();
                    calls.push((method, body));
                    calls.len() == 1
                };
                if rate_limit_first && first {
                    (
                        StatusCode::TOO_MANY_REQUESTS,
                        [("retry-after", "1")],
                        Json(json!({"ok":false})),
                    )
                } else if duplicate {
                    (
                        StatusCode::OK,
                        [("retry-after", "1")],
                        Json(json!({"ok":false,"error":"duplicate_block_id"})),
                    )
                } else {
                    (
                        StatusCode::OK,
                        [("retry-after", "1")],
                        Json(json!({"ok":true,"ts":"123.456"})),
                    )
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    (url, calls, server)
}
fn last_body(calls: &Calls) -> Value {
    calls.lock().unwrap().last().unwrap().1.clone()
}
async fn wait_for_call(calls: &Calls, method: &str) -> Value {
    for _ in 0..50 {
        if let Some((_, body)) = calls
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(m, _)| m == method)
        {
            return body.clone();
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    panic!("Slack mock did not receive {method}");
}
async fn drain(app: &App) {
    for _ in 0..10 {
        if !workflow::tick(app).await.unwrap() {
            return;
        }
    }
    panic!("incident worker still had work after ten jobs");
}
fn button_press(actor: &str, action: &str, incident: &str, api: &str) -> Value {
    json!({
        "team":{"id":"REPLACE_SLACK_WORKSPACE_ID"},
        "user":{"id":actor,"name":actor},
        "actions":[{"action_id":action,"value":incident,"action_ts":format!("{actor}.1")}],
        "channel":{"id":"REPLACE_CHANNEL_ID"},
        "message":{"ts":"100.200"},
        "response_url":format!("{api}/response"),
    })
}
impl Fixture {
    async fn open_incident_with_failed_delivery(&self) -> (String, i64) {
        let (status, body) = send(
            &self.router,
            json!({"event_id":"failed-1","service":"checkout","environment":"production","incident_key":"failed","sequence":1,"severity":1,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"}),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let incident = body["incident_id"].as_str().unwrap().to_owned();
        let (failed,): (i64,) = sqlx::query_as("UPDATE outbox SET failed_at=now(),attempts=1,last_error='Slack API error: not_in_channel (HTTP 200)' WHERE incident_id=$1 RETURNING id")
            .bind(&incident)
            .fetch_one(&self.app.db)
            .await
            .unwrap();
        (incident, failed)
    }
}

#[tokio::test]
async fn health_requires_fresh_worker_heartbeats() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let app = fixture.app.clone();
    let db = &app.db;
    assert_eq!(
        health(State(app.clone())).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    worker_heartbeat(db, "incidents").await.unwrap();
    worker_heartbeat(db, "messages").await.unwrap();
    assert_eq!(
        health(State(app.clone())).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    worker_heartbeat(db, "channels").await.unwrap();
    assert_eq!(health(State(app.clone())).await, StatusCode::OK);
    sqlx::query(
        "UPDATE worker_heartbeats SET last_tick=now()-interval '61 seconds' WHERE worker='messages'",
    )
    .execute(db)
    .await
    .unwrap();
    assert_eq!(
        health(State(app.clone())).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    worker_heartbeat(db, "messages").await.unwrap();
    sqlx::query(
        "UPDATE worker_heartbeats SET last_tick=now()-interval '61 seconds' WHERE worker='channels'",
    )
    .execute(db)
    .await
    .unwrap();
    assert_eq!(
        health(State(app.clone())).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    worker_heartbeat(db, "channels").await.unwrap();
    assert_eq!(health(State(app.clone())).await, StatusCode::OK);
    fixture.close().await;
}

#[tokio::test]
async fn repeated_and_concurrent_events_coalesce_into_one_incident() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let router = &fixture.router;
    let mut event = first_event();
    let (status, body) = send(router, event.clone()).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["incident_id"].as_str().unwrap();
    assert_eq!(send(router, event.clone()).await.0, StatusCode::OK);
    let mut invalid_context = event.clone();
    invalid_context["event_id"] = json!("invalid-url");
    invalid_context["source_url"] = json!("javascript:alert(1)");
    assert_eq!(
        send(router, invalid_context).await.0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut concurrent = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let router = router.clone();
        let event = event.clone();
        concurrent.spawn(async move { send(&router, event).await.0 });
    }
    while let Some(status) = concurrent.join_next().await {
        assert_eq!(status.unwrap(), StatusCode::OK);
    }
    event["severity"] = json!(1);
    assert_eq!(send(router, event.clone()).await.0, StatusCode::CONFLICT);
    event["severity"] = json!(2);
    for n in 1..1000 {
        event["event_id"] = json!(format!("event-{n}"));
        event["sequence"] = json!(n);
        assert_eq!(send(router, event.clone()).await.0, StatusCode::ACCEPTED);
    }
    let row = sqlx::query("SELECT count,severity FROM incidents WHERE id=$1")
        .bind(id)
        .fetch_one(&fixture.app.db)
        .await
        .unwrap();
    assert_eq!(row.get::<i64, _>("count"), 1000);
    let (queued,): (i64,) = sqlx::query_as("SELECT count(*) FROM outbox WHERE sent_at IS NULL")
        .fetch_one(&fixture.app.db)
        .await
        .unwrap();
    assert_eq!(queued, 1);
    fixture.close().await;
}

#[tokio::test]
async fn incident_delivery_retries_after_rate_limit_and_escalates() {
    let (api, calls, server) = strict_slack_mock(true).await;
    let Some(fixture) = Fixture::with_config(|config| config.api_url = api.clone()).await else {
        return;
    };
    let app = &fixture.app;
    let db = &app.db;
    let id = create_incident(&fixture.router, first_event()).await;
    workflow::tick(app).await.unwrap();
    let (attempts,): (i32,) = sqlx::query_as("SELECT attempts FROM outbox WHERE sent_at IS NULL")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(attempts, 1);
    sqlx::query("UPDATE outbox SET due_at=now()")
        .execute(db)
        .await
        .unwrap();
    workflow::tick(app).await.unwrap();
    let (incident_mentions,):(Value,)=sqlx::query_as("SELECT mentioned_members FROM outbox WHERE incident_id=$1 AND provider_channel='REPLACE_CHANNEL_ID' AND sent_at IS NOT NULL ORDER BY id LIMIT 1").bind(&id).fetch_one(db).await.unwrap();
    assert_eq!(incident_mentions, json!(["REPLACE_SLACK_USER_ID"]));
    let captured_text = last_body(&calls)["text"].as_str().unwrap().to_owned();
    assert!(captured_text.contains("Payment gateway timed out"));
    assert!(captured_text.contains("https://example.com/traces/123"));
    sqlx::query("UPDATE incidents SET next_escalation=now()-interval '1 second' WHERE id=$1")
        .bind(&id)
        .execute(db)
        .await
        .unwrap();
    workflow::tick(app).await.unwrap();
    let (sent,): (i64,) = sqlx::query_as("SELECT count(*) FROM outbox WHERE sent_at IS NOT NULL")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(sent, 2);
    server.abort();
    fixture.close().await;
}

#[tokio::test]
async fn slack_buttons_record_confirmations_and_acknowledge() {
    let (api, _calls, server) = strict_slack_mock(false).await;
    let Some(fixture) = Fixture::with_app(|app| {
        app.config.api_url = api.clone();
        grant_admins(app);
    })
    .await
    else {
        return;
    };
    let app = &fixture.app;
    let db = &app.db;
    let id = create_incident(&fixture.router, first_event()).await;
    let id = id.as_str();
    drain(app).await;
    let payload = json!({"team":{"id":"REPLACE_SLACK_WORKSPACE_ID"},"channel":{"id":"REPLACE_CHANNEL_ID"},"message":{"ts":"123.456","text":"original"},"user":{"id":"REPLACE_SLACK_USER_ID"},"actions":[{"action_id":"confirm","value":id,"action_ts":"100.0"}]});
    let (headers, body) = signed_action(&payload);
    assert_eq!(
        workflow::action(State(app.clone()), headers.clone(), body.clone())
            .await
            .unwrap(),
        StatusCode::OK
    );
    assert_eq!(
        workflow::action(State(app.clone()), headers, body)
            .await
            .unwrap(),
        StatusCode::OK
    );
    let (count,):(i64,)=sqlx::query_as("SELECT count(*) FROM confirmations WHERE incident_id=$1 AND message_snapshot->>'text'='original'").bind(id).fetch_one(db).await.unwrap();
    assert_eq!(count, 1);
    let mut other = payload.clone();
    other["user"] = json!({"id":"UOTHER", "name":"other"});
    other["actions"][0]["action_ts"] = json!("100.2");
    assert_eq!(
        workflow::action_payload(app.clone(), other.clone())
            .await
            .unwrap(),
        StatusCode::OK
    );
    other["actions"][0]["action_id"] = json!("ack");
    assert_eq!(
        workflow::action_payload(app.clone(), other)
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    let (name,): (Option<String>,) = sqlx::query_as(
        "SELECT actor_name FROM confirmations WHERE incident_id=$1 AND actor='UOTHER'",
    )
    .bind(id)
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(name.as_deref(), Some("other"));
    let ack = json!({"team":{"id":"REPLACE_SLACK_WORKSPACE_ID"},"user":{"id":"REPLACE_SLACK_USER_ID"},"actions":[{"action_id":"ack","value":id,"action_ts":"100.1"}]});
    let (headers, body) = signed_action(&ack);
    assert_eq!(
        workflow::action(State(app.clone()), headers.clone(), body.clone())
            .await
            .unwrap(),
        StatusCode::OK
    );
    assert_eq!(
        workflow::action(State(app.clone()), headers, body)
            .await
            .unwrap(),
        StatusCode::OK
    );
    let (state, deadline): (String, bool) =
        sqlx::query_as("SELECT state,next_escalation IS NULL FROM incidents WHERE id=$1")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(state, "acknowledged");
    assert!(deadline);
    sqlx::query("INSERT INTO outbox(incident_id,target,sent_at,provider_channel,provider_message_ts) VALUES($1,'development',now(),'C_OTHER','999.0')").bind(id).execute(db).await.unwrap();
    let (_, flow) = api_request(
        &fixture.admin_router(),
        "/v1/admin/flows",
        ADMIN_TOKEN,
        None,
    )
    .await;
    let other = flow["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["delivery"]["provider_channel"] == "C_OTHER")
        .unwrap();
    assert_eq!(other["confirmations"], 0);
    let original = flow["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| {
            entry["incident_id"] == id
                && entry["delivery"]["provider_channel"] == "REPLACE_CHANNEL_ID"
                && entry["delivery"]["provider_message_ts"] == "123.456"
        })
        .unwrap();
    assert_eq!(original["confirmations"], 2);
    server.abort();
    fixture.close().await;
}

#[tokio::test]
async fn recovery_resolves_incident_and_blocks_reopening() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let router = &fixture.router;
    let mut event = first_event();
    let id = create_incident(router, event.clone()).await;
    workflow::action_payload(
        fixture.app.clone(),
        json!({"team":{"id":"REPLACE_SLACK_WORKSPACE_ID"},"user":{"id":"REPLACE_SLACK_USER_ID"},"actions":[{"action_id":"ack","value":id,"action_ts":"100.1"}]}),
    )
    .await
    .unwrap();
    event["event_id"] = json!("urgent");
    event["sequence"] = json!(1001);
    event["severity"] = json!(1);
    assert_eq!(send(router, event.clone()).await.0, StatusCode::ACCEPTED);
    event["event_id"] = json!("recovery");
    event["sequence"] = json!(1002);
    event["recovered"] = json!(true);
    assert_eq!(send(router, event.clone()).await.0, StatusCode::ACCEPTED);
    event["event_id"] = json!("late");
    event["sequence"] = json!(500);
    event["recovered"] = json!(false);
    assert_eq!(send(router, event.clone()).await.0, StatusCode::ACCEPTED);
    let (state,): (String,) = sqlx::query_as("SELECT state FROM incidents WHERE id=$1")
        .bind(&id)
        .fetch_one(&fixture.app.db)
        .await
        .unwrap();
    assert_eq!(state, "resolved");
    event["event_id"] = json!("illegal-reopen");
    event["sequence"] = json!(1003);
    assert_eq!(send(router, event).await.0, StatusCode::CONFLICT);
    fixture.close().await;
}

#[tokio::test]
async fn low_priority_digest_lists_each_incident_once() {
    let (api, calls, server) = strict_slack_mock(false).await;
    let Some(fixture) = Fixture::with_config(|config| config.api_url = api.clone()).await else {
        return;
    };
    let app = &fixture.app;
    let db = &app.db;
    let low = json!({"event_id":"low-1","service":"other","environment":"dev","incident_key":"episode-low","sequence":1,"severity":3,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"});
    let (status, low_created) = send(&fixture.router, low).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let low_id = low_created["incident_id"].as_str().unwrap();
    sqlx::query(
        "UPDATE incidents SET state='acknowledged',owner='REPLACE_SLACK_USER_ID' WHERE id=$1",
    )
    .bind(low_id)
    .execute(db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO outbox(incident_id,target,due_at) VALUES($1,'operations',now())")
        .bind(low_id)
        .execute(db)
        .await
        .unwrap();
    sqlx::query("UPDATE outbox SET due_at=now() WHERE sent_at IS NULL")
        .execute(db)
        .await
        .unwrap();
    workflow::tick(app).await.unwrap();
    let (pending,): (i64,) = sqlx::query_as("SELECT count(*) FROM outbox WHERE sent_at IS NULL")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(pending, 0);
    let digest = last_body(&calls);
    let blocks = digest["blocks"].as_array().unwrap();
    let sections: Vec<_> = blocks.iter().filter(|b| b["block_id"] == low_id).collect();
    assert_eq!(sections.len(), 1);
    assert!(sections[0].get("accessory").is_none());
    assert_eq!(
        blocks
            .iter()
            .filter(|b| b["elements"].as_array().is_some_and(|els| els
                .iter()
                .any(|e| e["action_id"] == "confirm" && e["value"] == low_id)))
            .count(),
        1
    );
    server.abort();
    fixture.close().await;
}

#[tokio::test]
async fn ai_summary_records_failures_and_stores_the_summary() {
    let (api, _calls, server) = strict_slack_mock(false).await;
    let Some(fixture) = Fixture::with_app(|app| {
        app.config.api_url = api.clone();
        app.ai_token = Some("test".into());
    })
    .await
    else {
        return;
    };
    let db = &fixture.app.db;
    let id = create_incident(&fixture.router, first_event()).await;
    let mut ai_app = (*fixture.app).clone();
    ai_app.config.ai.api_url = api.clone() + "/responses";
    workflow::summarize(&ai_app).await.unwrap();
    let (error,): (Option<String>,) =
        sqlx::query_as("SELECT ai_error FROM incidents ORDER BY severity LIMIT 1")
            .fetch_one(db)
            .await
            .unwrap();
    assert!(error.is_some());
    let ai_mock=Router::new().route("/responses",post(|Json(value):Json<Value>|async move{assert_eq!(value["store"],false);let input:Value=serde_json::from_str(value["input"].as_str().unwrap()).unwrap();assert!(input.get("events").is_some());assert!(input.get("raw_log").is_none());Json(json!({"output":[{"content":[{"type":"output_text","text":"확인된 이벤트를 요약합니다."}]}]}))}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    ai_app.config.ai.api_url = format!("http://{}/responses", listener.local_addr().unwrap());
    let ai_server = tokio::spawn(async move {
        axum::serve(listener, ai_mock).await.unwrap();
    });
    sqlx::query("UPDATE incidents SET next_ai=now() WHERE id=$1")
        .bind(&id)
        .execute(db)
        .await
        .unwrap();
    workflow::summarize(&ai_app).await.unwrap();
    let (summary,): (Option<Value>,) =
        sqlx::query_as("SELECT ai_summary FROM incidents WHERE id=$1")
            .bind(&id)
            .fetch_one(db)
            .await
            .unwrap();
    assert!(summary.is_some());
    ai_server.abort();
    server.abort();
    fixture.close().await;
}

#[tokio::test]
async fn admin_api_requires_credentials_and_acknowledges_incidents() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let admin_router = fixture.admin_router();
    assert_eq!(
        api_request(&admin_router, "/v1/admin", "", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api_request(&admin_router, "/v1/admin", SOURCE_TOKEN, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (status, data) = api_request(&admin_router, "/v1/admin", ADMIN_TOKEN, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(data.get("counts").is_some());
    assert!(data.get("signing_secret").is_none());
    let admin_event = json!({"event_id":"admin-event","service":"checkout","environment":"production","incident_key":"admin-lifecycle","sequence":1,"severity":1,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"});
    let admin_id = create_incident(&fixture.router, admin_event).await;
    let ack_path = format!("/v1/admin/incidents/{admin_id}/ack");
    assert_eq!(
        api_request(
            &admin_router,
            &ack_path,
            ADMIN_TOKEN,
            Some(json!({"actor":"operator","sequence":0}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        api_request(
            &admin_router,
            &ack_path,
            ADMIN_TOKEN,
            Some(json!({"actor":"unknown","sequence":1}))
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        api_request(
            &admin_router,
            &ack_path,
            ADMIN_TOKEN,
            Some(json!({"actor":"operator","sequence":1}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        api_request(
            &admin_router,
            &ack_path,
            ADMIN_TOKEN,
            Some(json!({"actor":"operator","sequence":1}))
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let (_, detail) = api_request(
        &admin_router,
        &format!("/v1/admin/incidents/{admin_id}"),
        ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(detail["incident"]["state"], "acknowledged");
    assert_eq!(detail["actions"].as_array().unwrap().len(), 1);
    let (_, list) = api_request(
        &admin_router,
        "/v1/admin/incidents?q=admin-lifecycle&state=acknowledged&priority=1",
        ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        api_request(
            &admin_router,
            "/v1/admin/incidents?offset=-1",
            ADMIN_TOKEN,
            None
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let root = admin_router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/admin")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::OK);
    assert!(root.headers().contains_key("content-security-policy"));
    fixture.close().await;
}

#[tokio::test]
async fn client_token_is_scoped_rotated_and_revoked() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let router = &fixture.router;
    let admin_router = fixture.admin_router();
    let db = &fixture.app.db;
    let id = create_incident(router, first_event()).await;
    let (status, created_token) = api_request(
        &admin_router,
        "/v1/admin/tokens",
        ADMIN_TOKEN,
        Some(json!({"name":"client","service":"checkout"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let client_token = created_token["token"].as_str().unwrap();
    let token_id = created_token["id"].as_str().unwrap();
    let (identity_status, identity) = api_request(router, "/v1/client", client_token, None).await;
    assert_eq!(identity_status, StatusCode::OK);
    assert_eq!(identity["service"], "checkout");
    assert_eq!(
        api_request(router, "/v1/client", "invalid-token", None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, listed) = api_request(&admin_router, "/v1/admin/tokens", ADMIN_TOKEN, None).await;
    assert!(listed["items"][0].get("token_hash").is_none());
    let client_event = json!({"event_id":"client-event","service":"checkout","environment":"dev","incident_key":"client-incident","sequence":1,"severity":2,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z","correlation_id":"shared-trace"});
    let req = |event: Value| {
        Request::builder()
            .method("POST")
            .uri("/v1/events")
            .header("authorization", format!("Bearer {client_token}"))
            .header("content-type", "application/json")
            .body(Body::from(event.to_string()))
            .unwrap()
    };
    assert_eq!(
        router
            .clone()
            .oneshot(req(client_event.clone()))
            .await
            .unwrap()
            .status(),
        StatusCode::ACCEPTED
    );
    let (own,): (String,) =
        sqlx::query_as("SELECT id FROM incidents WHERE incident_key='client-incident'")
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(
        api_request(router, &format!("/v1/incidents/{own}"), client_token, None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        api_request(router, &format!("/v1/incidents/{id}"), client_token, None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (_, own_list) = api_request(router, "/v1/incidents", client_token, None).await;
    assert_eq!(own_list.as_array().unwrap().len(), 1);
    let (_, named_source) = api_request(
        &admin_router,
        &format!("/v1/admin/incidents/{own}"),
        ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(named_source["incident"]["source_name"], "client");
    let (_, own_detail) =
        api_request(router, &format!("/v1/incidents/{own}"), client_token, None).await;
    assert!(
        own_detail["related_incidents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (_, admin_detail) = api_request(
        &admin_router,
        &format!("/v1/admin/incidents/{own}"),
        ADMIN_TOKEN,
        None,
    )
    .await;
    assert!(
        admin_detail["related"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == id)
    );
    let client_message = json!({"event_id":"client-message","service":"checkout","environment":"dev","title":"Report","body":"Ready"});
    let (status, created_message) = api_request(
        router,
        "/v1/messages",
        client_token,
        Some(client_message.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let mut duplicate = client_message.clone();
    duplicate["event_id"] = json!("client-message-repeat");
    let (status, combined) = api_request(
        router,
        "/v1/messages",
        client_token,
        Some(duplicate.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(combined["message_id"], created_message["message_id"]);
    assert_eq!(combined["deduplicated"], true);
    assert_eq!(
        api_request(
            router,
            "/v1/messages",
            client_token,
            Some(duplicate.clone())
        )
        .await
        .1["message_id"],
        created_message["message_id"]
    );
    duplicate["body"] = json!("Different content");
    assert_eq!(
        api_request(router, "/v1/messages", client_token, Some(duplicate))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let message_id = created_message["message_id"].as_str().unwrap();
    let (_, status) = api_request(
        router,
        &format!("/v1/messages/{message_id}"),
        client_token,
        None,
    )
    .await;
    assert_eq!(status["duplicate_count"], 1);
    let mut other_content = client_message.clone();
    other_content["event_id"] = json!("client-message-new");
    other_content["body"] = json!("New content");
    assert_eq!(
        api_request(router, "/v1/messages", client_token, Some(other_content))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    sqlx::query("UPDATE gateway_messages SET created_at=now()-interval '61 seconds' WHERE id=$1")
        .bind(message_id)
        .execute(db)
        .await
        .unwrap();
    let mut later = client_message;
    later["event_id"] = json!("client-message-later");
    assert_eq!(
        api_request(router, "/v1/messages", client_token, Some(later))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let (incident_jobs,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM outbox WHERE sent_at IS NULL")
            .fetch_one(db)
            .await
            .unwrap();
    let (message_jobs,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM gateway_messages WHERE sent_at IS NULL")
            .fetch_one(db)
            .await
            .unwrap();
    assert!(message_jobs > 0);
    let (_, stats) = api_request(&admin_router, "/v1/admin", ADMIN_TOKEN, None).await;
    assert_eq!(stats["delivery"]["pending"], incident_jobs + message_jobs);
    assert_eq!(
        api_request(
            router,
            &format!("/v1/messages/{message_id}"),
            client_token,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let activity_path = format!("/v1/admin/tokens/{token_id}/activity");
    let (status, activity) = api_request(&admin_router, &activity_path, ADMIN_TOKEN, None).await;
    assert_eq!(status, StatusCode::OK);
    let activity = activity["items"].as_array().unwrap();
    assert_eq!(activity.len(), 4);
    assert!(
        activity
            .iter()
            .all(|entry| entry["received_at"].is_string())
    );
    assert!(
        activity
            .iter()
            .any(|entry| entry["kind"] == "event" && entry["event_id"] == "client-event")
    );
    assert!(
        activity
            .iter()
            .any(|entry| entry["kind"] == "message" && entry["event_id"] == "client-message")
    );
    sqlx::query("UPDATE events SET received_at=NULL WHERE event_id='client-event'")
        .execute(db)
        .await
        .unwrap();
    let (_, legacy) = api_request(&admin_router, &activity_path, ADMIN_TOKEN, None).await;
    assert!(
        legacy["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["kind"] == "event" && entry["received_at"].is_null())
    );
    let mut other = client_event.clone();
    other["service"] = json!("other");
    assert_eq!(
        router.clone().oneshot(req(other)).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let (status, rotated) = api_request(
        &admin_router,
        &format!("/v1/admin/tokens/{token_id}/rotate"),
        ADMIN_TOKEN,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new_token = rotated["token"].as_str().unwrap();
    assert_ne!(new_token, client_token);
    assert_eq!(
        api_request(router, "/v1/client", client_token, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api_request(router, "/v1/client", new_token, None).await.1["service"],
        "checkout"
    );
    assert_eq!(
        api_request(router, &format!("/v1/incidents/{own}"), client_token, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api_request(router, &format!("/v1/incidents/{own}"), new_token, None)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        api_request(
            router,
            &format!("/v1/messages/{message_id}"),
            new_token,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        api_request(
            &admin_router,
            &format!("/v1/admin/tokens/{token_id}/revoke"),
            ADMIN_TOKEN,
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        router
            .clone()
            .oneshot(req(client_event))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api_request(router, &format!("/v1/incidents/{own}"), new_token, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    let (_, history) = api_request(&admin_router, "/v1/admin/tokens", ADMIN_TOKEN, None).await;
    let actions = history["items"][0]["audit"].as_array().unwrap();
    assert_eq!(actions.len(), 3);
    assert!(actions.iter().any(|a| a["action"] == "created"));
    assert!(actions.iter().any(|a| a["action"] == "rotated"));
    assert!(actions.iter().any(|a| a["action"] == "revoked"));
    fixture.close().await;
}

#[tokio::test]
async fn named_admin_roles_are_enforced_and_audited() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let admin_router = fixture.admin_router();
    let (status, profile) = api_request(&admin_router, "/v1/admin", VIEWER, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(profile["administrator"]["name"], "bob");
    assert_eq!(profile["administrator"]["role"], "viewer");
    assert_eq!(
        api_request(
            &admin_router,
            "/v1/admin/tokens",
            VIEWER,
            Some(json!({"name":"denied","service":"checkout"}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (_, named_created) = api_request(
        &admin_router,
        "/v1/admin/tokens",
        NAMED_ADMIN,
        Some(json!({"name":"named audit","service":"checkout"})),
    )
    .await;
    let named_id = named_created["id"].as_str().unwrap();
    assert_eq!(
        api_request(
            &admin_router,
            &format!("/v1/admin/tokens/{named_id}/rotate"),
            VIEWER,
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        api_request(
            &admin_router,
            &format!("/v1/admin/tokens/{named_id}/rotate"),
            NAMED_ADMIN,
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        api_request(
            &admin_router,
            &format!("/v1/admin/tokens/{named_id}/revoke"),
            NAMED_ADMIN,
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (_, named_history) =
        api_request(&admin_router, "/v1/admin/tokens", NAMED_ADMIN, None).await;
    let record = named_history["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == named_id)
        .unwrap();
    let events = record["audit"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    assert!(
        events
            .iter()
            .all(|event| event["actor"] == "alice" && event["actor_kind"] == "named_admin")
    );
    let assignment_event = json!({"event_id":"named-assignment","service":"checkout","environment":"dev","incident_key":"named-assignment","sequence":1,"severity":1,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"});
    let assignment_id = create_incident(&fixture.router, assignment_event).await;
    let assignment_path = format!("/v1/admin/incidents/{assignment_id}/ack");
    assert_eq!(
        api_request(
            &admin_router,
            &assignment_path,
            VIEWER,
            Some(json!({"actor":"operator","sequence":1}))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        api_request(
            &admin_router,
            &assignment_path,
            NAMED_ADMIN,
            Some(json!({"actor":"operator","sequence":1}))
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, assignment_detail) = api_request(
        &admin_router,
        &format!("/v1/admin/incidents/{assignment_id}"),
        NAMED_ADMIN,
        None,
    )
    .await;
    assert_eq!(assignment_detail["actions"][0]["initiator"], "alice");
    fixture.close().await;
}

#[tokio::test]
async fn ordinary_message_mentions_updates_and_records_confirmations() {
    let (api, calls, server) = strict_slack_mock(false).await;
    let Some(fixture) = Fixture::with_app(|app| {
        app.config.api_url = api.clone();
        grant_admins(app);
    })
    .await
    else {
        return;
    };
    let app = &fixture.app;
    let db = &app.db;
    let router = &fixture.router;
    let (_, without_mentions) = api_request(
        router,
        "/v1/messages",
        SOURCE_TOKEN,
        Some(
            json!({"event_id":"client-message","service":"checkout","environment":"dev","title":"Report","body":"Ready"}),
        ),
    )
    .await;
    let message = json!({"event_id":"ordinary-1","service":"checkout","environment":"dev","title":"배치 완료","body":"처리가 완료됐습니다.","notify_users":true});
    let request = || {
        Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("authorization", format!("Bearer {SOURCE_TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(message.to_string()))
            .unwrap()
    };
    let response = router.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    let message_id = created["message_id"].as_str().unwrap();
    assert_eq!(
        router.clone().oneshot(request()).await.unwrap().status(),
        StatusCode::OK
    );
    messages::deliver(app).await.unwrap();
    let (default_mentions,): (Value,) =
        sqlx::query_as("SELECT mentioned_members FROM gateway_messages WHERE id=$1")
            .bind(without_mentions["message_id"].as_str().unwrap())
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(default_mentions, json!([]));
    messages::deliver(app).await.unwrap();
    let (sent,): (bool,) =
        sqlx::query_as("SELECT sent_at IS NOT NULL FROM gateway_messages WHERE id=$1")
            .bind(message_id)
            .fetch_one(db)
            .await
            .unwrap();
    assert!(sent);
    let (mentioned,): (Value,) =
        sqlx::query_as("SELECT mentioned_members FROM gateway_messages WHERE id=$1")
            .bind(message_id)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(mentioned, json!(["REPLACE_SLACK_USER_ID"]));
    let request_body = last_body(&calls);
    assert!(
        request_body["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["text"]["type"] == "mrkdwn"
                && block["text"]["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("<@REPLACE_SLACK_USER_ID>")))
    );
    assert!(request_body["text"].as_str().unwrap().contains("수신 1회"));
    let mut repeated = message.clone();
    repeated["event_id"] = json!("ordinary-repeat");
    let (status, repeated_response) =
        api_request(router, "/v1/messages", SOURCE_TOKEN, Some(repeated)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(repeated_response["message_id"], message_id);
    messages::deliver(app).await.unwrap();
    let update = last_body(&calls);
    assert_eq!(update["ts"], "123.456");
    assert!(update["text"].as_str().unwrap().contains("수신 2회"));
    assert!(
        update["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "actions")
    );
    let (displayed,): (i64,) =
        sqlx::query_as("SELECT displayed_count FROM gateway_messages WHERE id=$1")
            .bind(message_id)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(displayed, 2);
    let (status, listing) = api_request(
        &fixture.admin_router(),
        "/v1/admin/messages",
        ADMIN_TOKEN,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        listing["items"][0]["message"]["payload"]["title"],
        "배치 완료"
    );
    let payload = json!({"team":{"id":"REPLACE_SLACK_WORKSPACE_ID"},"channel":{"id":"REPLACE_CHANNEL_ID"},"message":{"ts":"123.456","text":"배치 완료"},"user":{"id":"REPLACE_SLACK_USER_ID"},"actions":[{"action_id":"message_confirm","value":message_id,"action_ts":"200.1"}]});
    let (headers, body) = signed_action(&payload);
    assert_eq!(
        workflow::action(State(app.clone()), headers.clone(), body.clone())
            .await
            .unwrap(),
        StatusCode::OK
    );
    assert_eq!(
        workflow::action(State(app.clone()), headers, body)
            .await
            .unwrap(),
        StatusCode::OK
    );
    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM message_confirmations WHERE message_id=$1")
            .bind(message_id)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(count, 1);
    let mut socket_payload = payload.clone();
    socket_payload["actions"][0]["action_ts"] = json!("200.2");
    assert_eq!(
        workflow::action_payload(app.clone(), socket_payload.clone())
            .await
            .unwrap(),
        StatusCode::OK
    );
    assert_eq!(
        workflow::action_payload(app.clone(), socket_payload.clone())
            .await
            .unwrap(),
        StatusCode::OK
    );
    socket_payload["team"]["id"] = json!("unexpected-workspace");
    assert_eq!(
        workflow::action_payload(app.clone(), socket_payload)
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM message_confirmations WHERE message_id=$1")
            .bind(message_id)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(count, 2);
    let mut other = payload.clone();
    other["user"] = json!({"id":"UOTHER", "name":"other"});
    other["actions"][0]["action_ts"] = json!("200.3");
    assert_eq!(
        workflow::action_payload(app.clone(), other.clone())
            .await
            .unwrap(),
        StatusCode::OK
    );
    assert!(messages::deliver(app).await.unwrap());
    let confirmed = last_body(&calls);
    assert_eq!(confirmed["ts"], "123.456");
    assert!(
        confirmed["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|block| block["type"] == "context"
                && block["elements"][0]["text"] == "✅ 확인: <@REPLACE_SLACK_USER_ID>, <@UOTHER>")
    );
    assert!(!messages::deliver(app).await.unwrap());
    other["message"]["ts"] = json!("999.999");
    assert_eq!(
        workflow::action_payload(app.clone(), other)
            .await
            .unwrap_err()
            .0,
        StatusCode::FORBIDDEN
    );
    let (name,): (Option<String>,) = sqlx::query_as(
        "SELECT actor_name FROM message_confirmations WHERE message_id=$1 AND actor='UOTHER'",
    )
    .bind(message_id)
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(name.as_deref(), Some("other"));
    server.abort();
    fixture.close().await;
}

#[tokio::test]
async fn rejected_button_press_tells_the_actor_why() {
    let (api, calls, server) = slack_mock().await;
    let Some(fixture) = Fixture::with_config(|config| config.api_url = api.clone()).await else {
        return;
    };
    let (status, body) = send(
        &fixture.router,
        json!({"event_id":"press-1","service":"checkout","environment":"production","incident_key":"press","sequence":1,"severity":2,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let incident = body["incident_id"].as_str().unwrap().to_owned();
    let outsider = workflow::action_payload(
        fixture.app.clone(),
        button_press("UOUTSIDER", "ack", &incident, &api),
    )
    .await;
    assert_eq!(outsider.unwrap_err().0, StatusCode::FORBIDDEN);
    let notice = wait_for_call(&calls, "response").await;
    assert_eq!(notice["response_type"], "ephemeral");
    assert_eq!(notice["replace_original"], false);
    assert!(notice["text"].as_str().unwrap().contains("등록되지 않은"));
    calls.lock().unwrap().clear();
    workflow::action_payload(
        fixture.app.clone(),
        button_press("REPLACE_SLACK_USER_ID", "ack", &incident, &api),
    )
    .await
    .unwrap();
    workflow::action_payload(
        fixture.app.clone(),
        button_press("REPLACE_BACKUP_SLACK_USER_ID", "ack", &incident, &api),
    )
    .await
    .unwrap();
    let notice = wait_for_call(&calls, "response").await;
    assert_eq!(
        notice["text"],
        "이미 <@REPLACE_SLACK_USER_ID> 님이 담당하고 있습니다."
    );
    let mut forged = button_press("UOUTSIDER", "ack", &incident, &api);
    forged["response_url"] = json!("https://attacker.example/response");
    calls.lock().unwrap().clear();
    assert!(
        workflow::action_payload(fixture.app.clone(), forged)
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(calls.lock().unwrap().is_empty());
    server.abort();
    fixture.close().await;
}

#[tokio::test]
async fn severity_rise_mentions_the_group_again() {
    let (api, calls, server) = slack_mock().await;
    let Some(fixture) = Fixture::with_config(|config| config.api_url = api.clone()).await else {
        return;
    };
    let event = json!({"event_id":"rise-1","service":"checkout","environment":"production","incident_key":"rise","sequence":1,"severity":2,"recovered":false,"occurred_at":"2026-11-07T00:00:00Z","source_url":"https://grafana.example/d/x?orgId=1&viewPanel=7"});
    let (_, body) = send(&fixture.router, event.clone()).await;
    let incident = body["incident_id"].as_str().unwrap().to_owned();
    drain(&fixture.app).await;
    let first = wait_for_call(&calls, "chat.postMessage").await;
    // plain_text never turns a URL into a link, so the source gets its own mrkdwn link.
    assert!(
        !first["blocks"][0]["text"]["text"]
            .as_str()
            .unwrap()
            .contains("grafana.example"),
        "{first}"
    );
    let linked = first["blocks"].as_array().unwrap().iter().any(|block| {
        block["type"] == "context"
            && block["elements"][0]["type"] == "mrkdwn"
            && block["elements"][0]["text"]
                == "<https://grafana.example/d/x?orgId=1&amp;viewPanel=7|원본 보기>"
    });
    assert!(linked, "{first}");
    let explained = first["blocks"].as_array().unwrap().iter().any(|block| {
        block["type"] == "context"
            && block["elements"][0]["text"]
                .as_str()
                .is_some_and(|text| text.starts_with("재알림을 멈추려면"))
    });
    assert!(explained, "{first}");
    let buttons: Vec<_> = first["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|block| block["type"] == "actions")
        .flat_map(|block| block["elements"].as_array().unwrap().iter())
        .map(|button| {
            (
                button["action_id"].as_str().unwrap(),
                button["text"]["text"].as_str().unwrap(),
                button["style"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        buttons,
        [
            ("ack", "담당하기", Some("primary")),
            ("confirm", "확인만 하기", None)
        ]
    );
    workflow::action_payload(
        fixture.app.clone(),
        button_press("REPLACE_SLACK_USER_ID", "ack", &incident, &api),
    )
    .await
    .unwrap();
    drain(&fixture.app).await;
    calls.lock().unwrap().clear();
    let mut rise = event;
    rise["event_id"] = json!("rise-2");
    rise["sequence"] = json!(2);
    rise["severity"] = json!(1);
    assert_eq!(send(&fixture.router, rise).await.0, StatusCode::ACCEPTED);
    drain(&fixture.app).await;
    let methods: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(m, _)| m.clone())
        .collect();
    assert_eq!(methods, ["chat.postMessage"]);
    let reopened = wait_for_call(&calls, "chat.postMessage").await;
    assert!(
        reopened["text"]
            .as_str()
            .unwrap()
            .contains("<@REPLACE_SLACK_USER_ID>"),
        "{reopened}"
    );
    server.abort();
    fixture.close().await;
}

#[tokio::test]
async fn failed_delivery_does_not_block_later_alerts() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let (incident, failed) = fixture.open_incident_with_failed_delivery().await;
    // The second escalation targets default_group, which is the same group as the failed post.
    sqlx::query("UPDATE incidents SET escalation_step=1,next_escalation=now()-interval '1 second' WHERE id=$1")
        .bind(&incident)
        .execute(&fixture.app.db)
        .await
        .unwrap();
    workflow::schedule(&fixture.app).await.unwrap();
    let (pending,): (i64,) = sqlx::query_as("SELECT count(*) FROM outbox WHERE incident_id=$1 AND target='operations' AND id<>$2 AND sent_at IS NULL AND failed_at IS NULL")
        .bind(&incident)
        .bind(failed)
        .fetch_one(&fixture.app.db)
        .await
        .unwrap();
    assert_eq!(pending, 1);
    fixture.close().await;
}

#[tokio::test]
async fn delivery_health_reports_failed_and_retrying_jobs() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    for worker in ["incidents", "messages", "channels"] {
        worker_heartbeat(&fixture.app.db, worker).await.unwrap();
    }
    let (status, body) = health::delivery(State(fixture.app.clone())).await;
    assert_eq!(status, StatusCode::OK, "{}", body.0);
    let (incident, failed) = fixture.open_incident_with_failed_delivery().await;
    let (status, body) = health::delivery(State(fixture.app.clone())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body.0["problems"],
        json!(["1 deliveries failed permanently in the last 24 hours"])
    );
    sqlx::query("UPDATE outbox SET failed_at=NULL,attempts=3 WHERE id=$1")
        .bind(failed)
        .execute(&fixture.app.db)
        .await
        .unwrap();
    let (_, body) = health::delivery(State(fixture.app.clone())).await;
    assert_eq!(
        body.0["problems"],
        json!(["1 deliveries are delayed or retrying"])
    );
    sqlx::query("UPDATE outbox SET sent_at=now() WHERE incident_id=$1")
        .bind(&incident)
        .execute(&fixture.app.db)
        .await
        .unwrap();
    fixture
        .app
        .delivery_health
        .socket_disconnected_since(std::time::Instant::now());
    let (status, _) = health::delivery(State(fixture.app.clone())).await;
    assert_eq!(status, StatusCode::OK);
    fixture.app.delivery_health.socket_connected();
    fixture
        .app
        .delivery_health
        .socket_disconnected_since(std::time::Instant::now() - Duration::from_secs(61));
    let (status, body) = health::delivery(State(fixture.app.clone())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body.0["problems"],
        json!(["Slack Socket Mode is disconnected; button actions are not received"])
    );
    fixture.close().await;
}

#[tokio::test]
async fn state_transition_keeps_failed_delivery_visible() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let (incident, failed) = fixture.open_incident_with_failed_delivery().await;
    let (status, _) = send(
        &fixture.router,
        json!({"event_id":"failed-2","service":"checkout","environment":"production","incident_key":"failed","sequence":2,"severity":1,"recovered":true,"occurred_at":"2026-11-07T00:01:00Z"}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (unsent, still_failed): (bool, bool) =
        sqlx::query_as("SELECT sent_at IS NULL,failed_at IS NOT NULL FROM outbox WHERE id=$1")
            .bind(failed)
            .fetch_one(&fixture.app.db)
            .await
            .unwrap();
    assert!(unsent);
    assert!(still_failed);
    let (recovery,): (i64,) = sqlx::query_as("SELECT count(*) FROM outbox WHERE incident_id=$1 AND id<>$2 AND sent_at IS NULL AND failed_at IS NULL")
        .bind(&incident)
        .bind(failed)
        .fetch_one(&fixture.app.db)
        .await
        .unwrap();
    assert_eq!(recovery, 1);
    fixture.close().await;
}

#[tokio::test]
async fn overview_counts_received_items_and_open_incidents_by_level() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    for (key, severity) in [("level-crit", 1), ("level-warn", 3)] {
        let event = json!({"event_id":key,"service":"other","environment":"dev","incident_key":key,"sequence":1,"severity":severity,"recovered":false,"occurred_at":"2026-11-08T00:00:00Z"});
        create_incident(&fixture.router, event).await;
    }
    let message = |event_id: &str, title: &str, level: Option<&str>| {
        let mut body = json!({"event_id":event_id,"service":"checkout","environment":"dev","title":title,"body":"level test"});
        if let Some(level) = level {
            body["level"] = json!(level);
        }
        body
    };
    for (event_id, title, level, expected) in [
        ("m-crit", "disk full", Some("crit"), StatusCode::ACCEPTED),
        ("m-crit-again", "disk full", Some("crit"), StatusCode::OK),
        ("m-warn", "slow query", Some("warn"), StatusCode::ACCEPTED),
        ("m-info", "report ready", None, StatusCode::ACCEPTED),
    ] {
        let (status, body) = api_request(
            &fixture.router,
            "/v1/messages",
            SOURCE_TOKEN,
            Some(message(event_id, title, level)),
        )
        .await;
        assert_eq!(status, expected, "{event_id}: {body}");
    }
    let (status, _) = api_request(
        &fixture.router,
        "/v1/messages",
        SOURCE_TOKEN,
        Some(message("m-bad", "bad level", Some("fatal"))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, data) = api_request(&fixture.admin_router(), "/v1/admin", ADMIN_TOKEN, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        data["levels"],
        json!([
            {"level":"crit","last_hour":3,"last_day":3,"open":1,"unacknowledged":1},
            {"level":"error","last_hour":0,"last_day":0,"open":0,"unacknowledged":0},
            {"level":"warn","last_hour":2,"last_day":2,"open":1,"unacknowledged":1},
            {"level":"info","last_hour":1,"last_day":1,"open":0,"unacknowledged":0}
        ])
    );
    fixture.close().await;
}

#[tokio::test]
async fn ordinary_message_shows_its_level_in_slack() {
    let (api, calls, server) = slack_mock().await;
    let Some(fixture) = Fixture::with_config(|config| config.api_url = api.clone()).await else {
        server.abort();
        return;
    };
    for (event_id, title, level, expected) in [
        (
            "lv-crit",
            "disk full",
            Some("crit"),
            "🔴 CRIT · disk full\n",
        ),
        (
            "lv-error",
            "job failed",
            Some("error"),
            "🟠 ERROR · job failed\n",
        ),
        (
            "lv-warn",
            "slow query",
            Some("warn"),
            "🟡 WARN · slow query\n",
        ),
        ("lv-info", "report ready", None, "report ready\n"),
    ] {
        let mut body = json!({"event_id":event_id,"service":"checkout","environment":"dev","title":title,"body":"level display"});
        if let Some(level) = level {
            body["level"] = json!(level);
        }
        let (status, _) =
            api_request(&fixture.router, "/v1/messages", SOURCE_TOKEN, Some(body)).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        calls.lock().unwrap().clear();
        assert!(messages::deliver(&fixture.app).await.unwrap());
        let posted = wait_for_call(&calls, "chat.postMessage").await;
        assert!(
            posted["text"].as_str().unwrap().starts_with(expected),
            "{event_id}: {posted}"
        );
        assert!(
            posted["blocks"][0]["text"]["text"]
                .as_str()
                .unwrap()
                .starts_with(expected),
            "{event_id}: {posted}"
        );
    }
    let linked = json!({"event_id":"lv-link","service":"checkout","environment":"dev","title":"export failed","body":"run log (https://ci.example/runs/42)."});
    let (status, _) =
        api_request(&fixture.router, "/v1/messages", SOURCE_TOKEN, Some(linked)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    calls.lock().unwrap().clear();
    assert!(messages::deliver(&fixture.app).await.unwrap());
    let posted = wait_for_call(&calls, "chat.postMessage").await;
    // plain_text never links a URL, so the message adds a mrkdwn line for its links.
    let link = posted["blocks"].as_array().unwrap().iter().any(|block| {
        block["type"] == "context"
            && block["elements"][0]["text"] == "<https://ci.example/runs/42|ci.example/runs/42>"
    });
    assert!(link, "{posted}");
    let same = json!({"event_id":"lv-one-line","service":"checkout","environment":"dev","title":"backup done","body":"backup done"});
    let (status, _) = api_request(&fixture.router, "/v1/messages", SOURCE_TOKEN, Some(same)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    calls.lock().unwrap().clear();
    assert!(messages::deliver(&fixture.app).await.unwrap());
    let posted = wait_for_call(&calls, "chat.postMessage").await;
    assert_eq!(posted["text"], "backup done\n수신 1회");
    server.abort();
    fixture.close().await;
}

async fn post_hook(
    router: &Router,
    path: &str,
    content_type: &str,
    body: String,
) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", content_type)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn slack_incoming_webhook_payload_becomes_an_ordinary_message() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let db = &fixture.app.db;
    let (status, created) = api_request(
        &fixture.admin_router(),
        "/v1/admin/tokens",
        ADMIN_TOKEN,
        Some(json!({"name":"legacy cron","service":"checkout"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let token = created["token"].as_str().unwrap();
    let path = format!("/hooks/slack/{token}?environment=dev");
    let payload = json!({"text":"Nightly export failed","attachments":[{"color":"danger","text":"3 rows rejected"}]});
    assert_eq!(
        post_hook(
            &fixture.router,
            &path,
            "application/json",
            payload.to_string()
        )
        .await,
        (StatusCode::OK, "ok".to_owned())
    );
    let (service, environment, title, body, level, original): (String, String, String, String, String, Value) = sqlx::query_as(
        "SELECT payload->>'service',payload->>'environment',payload->>'title',payload->>'body',level,original_payload FROM gateway_messages WHERE source=$1",
    )
    .bind(format!("client:{}", created["id"].as_str().unwrap()))
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(
        (
            service.as_str(),
            environment.as_str(),
            title.as_str(),
            body.as_str(),
            level.as_str()
        ),
        (
            "checkout",
            "dev",
            "Nightly export failed",
            "3 rows rejected",
            "error"
        )
    );
    assert_eq!(original, payload);
    let form = serde_urlencoded::to_string([("payload", payload.to_string())]).unwrap();
    assert_eq!(
        post_hook(
            &fixture.router,
            &path,
            "application/x-www-form-urlencoded",
            form
        )
        .await,
        (StatusCode::OK, "ok".to_owned())
    );
    let (messages, duplicates): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM gateway_messages),(SELECT count(*) FROM message_duplicates)",
    )
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!((messages, duplicates), (1, 1));
    assert_eq!(
        post_hook(
            &fixture.router,
            "/hooks/slack/not-a-token",
            "application/json",
            payload.to_string()
        )
        .await,
        (StatusCode::NOT_FOUND, "no_service".to_owned())
    );
    assert_eq!(
        post_hook(
            &fixture.router,
            &path,
            "application/json",
            "not json".into()
        )
        .await,
        (StatusCode::BAD_REQUEST, "invalid_payload".to_owned())
    );
    assert_eq!(
        post_hook(
            &fixture.router,
            &path,
            "application/json",
            json!({"text":" "}).to_string()
        )
        .await,
        (StatusCode::BAD_REQUEST, "no_text".to_owned())
    );
    fixture.close().await;
}

#[tokio::test]
async fn alertmanager_alerts_open_and_resolve_incidents() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let router = &fixture.router;
    let db = &fixture.app.db;
    let (status, created) = api_request(
        &fixture.admin_router(),
        "/v1/admin/tokens",
        ADMIN_TOKEN,
        Some(json!({"name":"alertmanager","service":"checkout"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let token = created["token"].as_str().unwrap();
    let path = "/hooks/alertmanager?environment=dev";
    let alert = |status: &str, starts: &str, ends: &str, summary: &str| json!({"status":status,"labels":{"alertname":"HighLatency","severity":"critical"},"annotations":{"summary":summary},"startsAt":starts,"endsAt":ends,"fingerprint":"fp1"});
    let first = alert(
        "firing",
        "2026-11-08T01:00:00Z",
        "0001-01-01T00:00:00Z",
        "p99 2.1s",
    );
    let (status, body) = api_request(
        router,
        path,
        token,
        Some(json!({"version":"4","groupKey":"g1","status":"firing","alerts":[first, {"status":"firing","labels":{"alertname":"Broken"}}]})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["incidents"].as_array().unwrap().len(), 1);
    assert_eq!(body["rejected"][0]["error"], "missing or invalid startsAt");
    let incident_id = body["incidents"][0].as_str().unwrap().to_owned();
    let (environment, severity, state, key): (String, i16, String, String) =
        sqlx::query_as("SELECT environment,severity,state,incident_key FROM incidents WHERE id=$1")
            .bind(&incident_id)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(
        (environment.as_str(), severity, state.as_str()),
        ("dev", 1, "open")
    );
    assert!(key.starts_with("HighLatency/fp1/"));
    let repeat = alert(
        "firing",
        "2026-11-08T01:00:00Z",
        "0001-01-01T00:00:00Z",
        "p99 2.4s",
    );
    let (status, body) = api_request(
        router,
        path,
        token,
        Some(json!({"groupKey":"g1","alerts":[repeat]})),
    )
    .await;
    assert_eq!(
        (status, body["incidents"].clone()),
        (StatusCode::OK, json!([]))
    );
    let resolved = alert(
        "resolved",
        "2026-11-08T01:00:00Z",
        "2026-11-08T01:10:00Z",
        "p99 2.4s",
    );
    let (status, body) = api_request(
        router,
        path,
        token,
        Some(json!({"groupKey":"g1","alerts":[resolved]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["incidents"][0], incident_id);
    let (state,): (String,) = sqlx::query_as("SELECT state FROM incidents WHERE id=$1")
        .bind(&incident_id)
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(state, "resolved");
    let again = alert(
        "firing",
        "2026-11-08T02:00:00Z",
        "0001-01-01T00:00:00Z",
        "p99 3s",
    );
    let (status, body) = api_request(
        router,
        path,
        token,
        Some(json!({"groupKey":"g1","alerts":[again]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(body["incidents"][0], incident_id);
    let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM incidents")
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(count, 2);
    // A restarted Alertmanager can send a resolution for an alert nobody was told about.
    let unknown = json!({"status":"resolved","labels":{"alertname":"NeverSeen","severity":"critical"},"startsAt":"2026-11-08T03:00:00Z","endsAt":"2026-11-08T03:05:00Z","fingerprint":"fp9"});
    let (status, body) = api_request(
        router,
        path,
        token,
        Some(json!({"groupKey":"g9","alerts":[unknown]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let unknown_id = body["incidents"][0].as_str().unwrap();
    let (state, deliveries, routed, scheduled): (String, i64, bool, bool) = sqlx::query_as(
        "SELECT state,(SELECT count(*) FROM outbox WHERE incident_id=i.id)+(SELECT count(*) FROM channel_deliveries WHERE incident_id=i.id),policy_snapshot IS NOT NULL,dirty OR coalesce(next_summary,next_escalation,next_ai) IS NOT NULL FROM incidents i WHERE id=$1",
    )
    .bind(unknown_id)
    .fetch_one(db)
    .await
    .unwrap();
    assert_eq!(
        (state.as_str(), deliveries, routed, scheduled),
        ("resolved", 0, true, false)
    );
    let (status, _) = api_request(router, path, SOURCE_TOKEN, Some(json!({"alerts":[]}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = api_request(router, path, "not-a-token", Some(json!({"alerts":[]}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    fixture.close().await;
}

#[tokio::test]
async fn grafana_webhook_opens_and_resolves_an_incident() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let (status, created) = api_request(
        &fixture.admin_router(),
        "/v1/admin/tokens",
        ADMIN_TOKEN,
        Some(json!({"name":"grafana","service":"checkout"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let token = created["token"].as_str().unwrap();
    let notification = |status: &str, ends: &str| {
        json!({
            "receiver":"noti-mesh","status":status,"orgId":1,"version":"1","groupKey":"{}/{}:{alertname=\"CPU high\"}",
            "truncatedAlerts":0,"state":if status == "firing" {"alerting"} else {"ok"},"title":"[FIRING:1] CPU high","message":"cpu",
            "groupLabels":{"alertname":"CPU high"},"commonLabels":{"alertname":"CPU high"},"commonAnnotations":{},"externalURL":"https://grafana.example/",
            "alerts":[{"status":status,"labels":{"alertname":"CPU high","severity":"warning"},"annotations":{"summary":"cpu 93%"},
                "startsAt":"2026-11-08T01:00:00Z","endsAt":ends,"generatorURL":"https://grafana.example/alerting/grafana/abc/view",
                "fingerprint":"57c6d9296de2ad39","silenceURL":"https://grafana.example/alerting/silence/new","dashboardURL":"",
                "panelURL":"https://grafana.example/d/dash?viewPanel=2","values":{"B":93.1}}]
        })
    };
    let (status, body) = api_request(
        &fixture.router,
        "/hooks/grafana?environment=dev",
        token,
        Some(notification("firing", "0001-01-01T00:00:00Z")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let incident_id = body["incidents"][0].as_str().unwrap().to_owned();
    let (severity, state, link): (i16, String, String) = sqlx::query_as(
        "SELECT i.severity,i.state,e.payload->>'source_url' FROM incidents i JOIN events e ON e.incident_id=i.id WHERE i.id=$1",
    )
    .bind(&incident_id)
    .fetch_one(&fixture.app.db)
    .await
    .unwrap();
    assert_eq!(
        (severity, state.as_str(), link.as_str()),
        (3, "open", "https://grafana.example/d/dash?viewPanel=2")
    );
    let (status, body) = api_request(
        &fixture.router,
        "/hooks/grafana?environment=dev",
        token,
        Some(notification("resolved", "2026-11-08T01:05:00Z")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (state,): (String,) = sqlx::query_as("SELECT state FROM incidents WHERE id=$1")
        .bind(&incident_id)
        .fetch_one(&fixture.app.db)
        .await
        .unwrap();
    assert_eq!(state, "resolved");
    fixture.close().await;
}

#[tokio::test]
async fn dry_run_previews_without_storing_or_sending() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let router = &fixture.router;
    let (_, created) = api_request(
        &fixture.admin_router(),
        "/v1/admin/tokens",
        ADMIN_TOKEN,
        Some(json!({"name":"preview","service":"checkout"})),
    )
    .await;
    let token = created["token"].as_str().unwrap();
    let (status, body) = api_request(
        router,
        "/v1/events?dry_run=true",
        SOURCE_TOKEN,
        Some(first_event()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["dry_run"], true);
    assert_eq!(body["event"]["incident_key"], first_event()["incident_key"]);
    assert!(body["route"]["group"].is_string() && body["route"]["priority"].is_number());
    let message = json!({"event_id":"preview-1","service":"checkout","environment":"dev","title":"Report","body":"ready","level":"warn"});
    let (status, body) = api_request(
        router,
        "/v1/messages?dry_run=true",
        SOURCE_TOKEN,
        Some(message),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            body["message"]["title"].clone(),
            body["route"]["level"].clone()
        ),
        (json!("Report"), json!("warn"))
    );
    let (status, text) = post_hook(
        router,
        &format!("/hooks/slack/{token}?dry_run=true"),
        "application/json",
        json!({"text":"Disk alert\ndb-1 at 91%","attachments":[{"color":"danger"}]}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let preview: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        (
            preview["message"]["title"].clone(),
            preview["message"]["body"].clone(),
            preview["route"]["level"].clone()
        ),
        (json!("Disk alert"), json!("db-1 at 91%"), json!("error"))
    );
    let alert = json!({"status":"firing","labels":{"alertname":"HighLatency","severity":"critical"},"startsAt":"2026-11-08T01:00:00Z","fingerprint":"fp1"});
    let (status, body) = api_request(
        router,
        "/hooks/alertmanager?dry_run=true",
        token,
        Some(json!({"alerts":[alert]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["incidents"][0]["event"]["severity"], 1);
    let (status, _) = api_request(
        router,
        "/v1/messages?dry_run=true",
        "not-a-token",
        Some(json!({})),
    )
    .await;
    assert!(status.is_client_error());
    let (incidents, events, messages, outbox): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM incidents),(SELECT count(*) FROM events),(SELECT count(*) FROM gateway_messages),(SELECT count(*) FROM outbox)",
    )
    .fetch_one(&fixture.app.db)
    .await
    .unwrap();
    assert_eq!((incidents, events, messages, outbox), (0, 0, 0, 0));
    fixture.close().await;
}

#[test]
fn openapi_documents_every_public_route() {
    let spec: Value = serde_json::from_str(include_str!("../../web/openapi.json")).unwrap();
    let documented: std::collections::BTreeSet<String> =
        spec["paths"].as_object().unwrap().keys().cloned().collect();
    let source = include_str!("../main.rs");
    let routed: std::collections::BTreeSet<String> = source
        .split(".route(")
        .skip(1)
        .filter_map(|rest| rest.trim_start().strip_prefix('"')?.split('"').next())
        // Slack calls the action route with a signed request; it is not a client API.
        .filter(|path| *path != "/v1/slack/actions")
        .map(str::to_owned)
        .collect();
    assert_eq!(documented, routed);
}

#[tokio::test]
async fn api_reference_page_limits_scripts_to_this_origin() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let response = fixture
        .admin_router()
        .oneshot(
            Request::builder()
                .uri("/admin/api")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let policy = response.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    let page = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(policy.contains("script-src 'self';") && policy.contains("connect-src 'self'"));
    assert!(page.contains("&quot;url&quot;:&quot;/openapi.json&quot;"));
    assert!(
        !page.contains("bearerAuth&quot;:{&quot;token"),
        "tokens are prefilled only in development mode"
    );
    let spec = fixture
        .admin_router()
        .oneshot(
            Request::builder()
                .uri("/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(spec.status(), StatusCode::OK);
    fixture.close().await;
}

#[tokio::test]
async fn monitoring_tools_open_and_resolve_incidents() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let router = &fixture.router;
    let db = &fixture.app.db;
    let (_, created) = api_request(
        &fixture.admin_router(),
        "/v1/admin/tokens",
        ADMIN_TOKEN,
        Some(json!({"name":"monitors","service":"checkout"})),
    )
    .await;
    let token = created["token"].as_str().unwrap().to_owned();
    let state = |id: String| async move {
        sqlx::query_scalar::<_, String>("SELECT state FROM incidents WHERE id=$1")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    };
    let kuma = |status: i64, time: &str| json!({"heartbeat":{"monitorID":7,"status":status,"time":time,"msg":"x","important":true},"monitor":{"id":7,"name":"api","url":"https://api.example/health"},"msg":format!("[api] status {status}")});
    let path = "/hooks/uptime-kuma?environment=dev&severity=critical";
    let (status, down) = api_request(
        router,
        path,
        &token,
        Some(kuma(0, "2026-11-08 10:00:00.000")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{down}");
    let incident = down["incident_id"].as_str().unwrap().to_owned();
    let (status, again) = api_request(
        router,
        path,
        &token,
        Some(kuma(0, "2026-11-08 10:01:00.000")),
    )
    .await;
    assert_eq!(
        (status, again["incident_id"].as_str()),
        (StatusCode::ACCEPTED, Some(incident.as_str()))
    );
    let (status, up) = api_request(
        router,
        path,
        &token,
        Some(kuma(1, "2026-11-08 10:02:00.000")),
    )
    .await;
    assert_eq!(
        (status, up["incident_id"].as_str()),
        (StatusCode::ACCEPTED, Some(incident.as_str()))
    );
    assert_eq!(state(incident.clone()).await, "resolved");
    let (status, ignored) = api_request(
        router,
        path,
        &token,
        Some(kuma(1, "2026-11-08 10:03:00.000")),
    )
    .await;
    assert_eq!(
        (status, ignored["ignored"].clone()),
        (StatusCode::OK, json!("no open incident"))
    );
    let (status, reopened) = api_request(
        router,
        path,
        &token,
        Some(kuma(0, "2026-11-08 11:00:00.000")),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_ne!(reopened["incident_id"].as_str(), Some(incident.as_str()));
    let (status, _) = api_request(
        router,
        path,
        SOURCE_TOKEN,
        Some(kuma(0, "2026-11-08 12:00:00.000")),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let sentry = |resource: &str, body: Value, token: &str| {
        Request::builder()
            .method("POST")
            .uri(format!("/hooks/sentry/{token}?environment=dev"))
            .header("content-type", "application/json")
            .header("sentry-hook-resource", resource)
            .header("sentry-hook-timestamp", "1794150000")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let send = |request: Request<Body>| async move {
        let response = router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
        )
    };
    let alert = json!({"action":"triggered","data":{"event":{"event_id":"e1","issue_id":"42","title":"TypeError","culprit":"app.js","level":"fatal","datetime":"2026-11-08T10:00:00Z","web_url":"https://sentry.example/issues/42/events/e1/"}}});
    let (status, opened) = send(sentry("event_alert", alert, &token)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{opened}");
    let issue_incident = opened["incident_id"].as_str().unwrap().to_owned();
    let (severity,): (i16,) = sqlx::query_as("SELECT severity FROM incidents WHERE id=$1")
        .bind(&issue_incident)
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(severity, 1);
    let resolved = json!({"action":"resolved","data":{"issue":{"id":"42","title":"TypeError","level":"fatal"}}});
    let (status, closed) = send(sentry("issue", resolved.clone(), &token)).await;
    assert_eq!(
        (status, closed["incident_id"].as_str()),
        (StatusCode::ACCEPTED, Some(issue_incident.as_str()))
    );
    assert_eq!(state(issue_incident).await, "resolved");
    let (status, unknown) = send(sentry(
        "issue",
        json!({"action":"resolved","data":{"issue":{"id":"77"}}}),
        &token,
    ))
    .await;
    assert_eq!(
        (status, unknown["ignored"].clone()),
        (StatusCode::OK, json!("no open incident"))
    );
    let (status, _) = send(sentry("issue", resolved, "not-a-token")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Sentry puts the environment on the alert event only; its resolution must still find the incident.
    let staged = json!({"action":"triggered","data":{"event":{"event_id":"e9","issue_id":"43","title":"KeyError","level":"error","datetime":"2026-11-08T10:00:00Z","environment":"staging"}}});
    let (status, staged_open) = send(sentry("event_alert", staged, &token)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{staged_open}");
    let staged_incident = staged_open["incident_id"].as_str().unwrap().to_owned();
    let (status, staged_closed) = send(sentry("issue", json!({"action":"resolved","data":{"issue":{"id":"43","title":"KeyError","level":"error"}}}), &token)).await;
    assert_eq!(
        (status, staged_closed["incident_id"].as_str()),
        (StatusCode::ACCEPTED, Some(staged_incident.as_str())),
        "{staged_closed}"
    );
    let (environment, staged_state): (String, String) =
        sqlx::query_as("SELECT environment,state FROM incidents WHERE id=$1")
            .bind(&staged_incident)
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(
        (environment.as_str(), staged_state.as_str()),
        ("staging", "resolved")
    );

    let datadog = |id: &str, transition: &str, date: i64| json!({"id":id,"cycle":"c-1","transition":transition,"priority":"P2","date":date.to_string(),"title":"checkout 5xx","link":"https://app.datadoghq.com/monitors/1"});
    let (status, triggered) = api_request(
        router,
        "/hooks/datadog?environment=dev",
        &token,
        Some(datadog("d1", "Triggered", 1_794_150_000_000)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{triggered}");
    let dd_incident = triggered["incident_id"].as_str().unwrap().to_owned();
    let (status, _) = api_request(
        router,
        "/hooks/datadog?environment=dev",
        &token,
        Some(datadog("d2", "Recovered", 1_794_150_060_000)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(state(dd_incident).await, "resolved");
    let (status, _) = api_request(
        router,
        "/hooks/datadog?environment=dev",
        &token,
        Some(json!({"id":"d3"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    fixture.close().await;
}

#[tokio::test]
async fn incident_report_lists_the_timeline_in_order() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let admin_router = fixture.admin_router();
    let event = json!({"event_id":"report-1","service":"checkout","environment":"production","incident_key":"report","sequence":1,"severity":2,"recovered":false,"occurred_at":"2026-11-08T01:00:00Z","summary":"결제 API 5xx, 재시도 증가","source_url":"https://grafana.example/d/x?viewPanel=7"});
    let id = create_incident(&fixture.router, event).await;
    drain(&fixture.app).await;
    let (status, _) = api_request(
        &admin_router,
        &format!("/v1/admin/incidents/{id}/ack"),
        ADMIN_TOKEN,
        Some(json!({"actor":"operator","sequence":1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let recovery = json!({"event_id":"report-2","service":"checkout","environment":"production","incident_key":"report","sequence":2,"severity":2,"recovered":true,"occurred_at":"2026-11-08T01:30:00Z"});
    assert_eq!(
        send(&fixture.router, recovery).await.0,
        StatusCode::ACCEPTED
    );
    let report = |path: String| {
        let router = admin_router.clone();
        async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let kind = response.headers()["content-type"]
                .to_str()
                .unwrap()
                .to_owned();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, kind, String::from_utf8(bytes.to_vec()).unwrap())
        }
    };
    let (status, kind, text) = report(format!(
        "/v1/admin/incidents/{id}/report?offset_minutes=540"
    ))
    .await;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(kind.starts_with("text/markdown"), "{kind}");
    assert!(
        text.starts_with("# 사건 회고 초안: 결제 API 5xx, 재시도 증가\n"),
        "{text}"
    );
    assert!(
        text.contains("- 처음 발생: 2026-11-08 10:00:00\n"),
        "{text}"
    );
    assert!(
        text.contains("- 해결: 2026-11-08 10:30:00, 발생 후 30분\n"),
        "{text}"
    );
    assert!(text.contains("- 담당: operator, "), "{text}");
    assert!(text.contains("시각은 UTC+09:00 기준입니다."), "{text}");
    let order: Vec<usize> = [
        "발생 이벤트 수신",
        "담당 지정: operator",
        "해결 이벤트 수신",
    ]
    .iter()
    .map(|needle| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle} missing in {text}"))
    })
    .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{text}");
    assert!(
        text.contains("https://grafana.example/d/x?viewPanel=7"),
        "{text}"
    );
    assert!(text.contains("## 원인"), "{text}");
    let (status, _, utc) = report(format!("/v1/admin/incidents/{id}/report")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        utc.contains("- 처음 발생: 2026-11-08 01:00:00\n")
            && utc.contains("시각은 UTC 기준입니다."),
        "{utc}"
    );
    let (status, _, _) = report("/v1/admin/incidents/missing/report".into()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    fixture.close().await;
}

#[tokio::test]
async fn silence_holds_notifications_until_it_ends() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let admin = fixture.admin_router();
    let db = &fixture.app.db;
    let (status, silence) = api_request(
        &admin,
        "/v1/admin/silences",
        ADMIN_TOKEN,
        Some(json!({"service":"checkout","environment":"production","minutes":60,"reason":"DB 점검"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{silence}");
    let silence_id = silence["id"].as_str().unwrap().to_owned();
    let outbox = |id: String| async move {
        sqlx::query_scalar::<_, i64>("SELECT (SELECT count(*) FROM outbox WHERE incident_id=$1)+(SELECT count(*) FROM channel_deliveries WHERE incident_id=$1)")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    };
    let event = |key: &str, sequence: i64, recovered: bool| json!({"event_id":format!("{key}-{sequence}"),"service":"checkout","environment":"production","incident_key":key,"sequence":sequence,"severity":1,"recovered":recovered,"occurred_at":"2026-11-08T01:00:00Z"});
    let open = create_incident(&fixture.router, event("still-open", 1, false)).await;
    let closed = create_incident(&fixture.router, event("closed-in-window", 1, false)).await;
    assert_eq!(
        send(&fixture.router, event("closed-in-window", 2, true))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let elsewhere = json!({"event_id":"other-1","service":"checkout","environment":"staging","incident_key":"other","sequence":1,"severity":1,"recovered":false,"occurred_at":"2026-11-08T01:00:00Z"});
    let other = create_incident(&fixture.router, elsewhere).await;
    assert_eq!(
        (outbox(open.clone()).await, outbox(closed.clone()).await),
        (0, 0)
    );
    assert!(outbox(other).await > 0);
    let (held,): (Option<String>,) = sqlx::query_as("SELECT silence_id FROM incidents WHERE id=$1")
        .bind(&open)
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!(held.as_deref(), Some(silence_id.as_str()));
    let (status, message) = api_request(
        &fixture.router,
        "/v1/messages",
        SOURCE_TOKEN,
        Some(json!({"event_id":"m-silenced","service":"checkout","environment":"production","title":"배포 시작","body":"v2"})),
    )
    .await;
    assert_eq!(
        (status, message["silenced"].clone()),
        (StatusCode::ACCEPTED, json!(true)),
        "{message}"
    );
    let (disabled, channels): (bool, i64) = sqlx::query_as("SELECT slack_disabled,(SELECT count(*) FROM channel_deliveries d WHERE d.message_id=m.id) FROM gateway_messages m WHERE id=$1")
        .bind(message["message_id"].as_str().unwrap())
        .fetch_one(db)
        .await
        .unwrap();
    assert_eq!((disabled, channels), (true, 0));
    let (_, listed) = api_request(&admin, "/v1/admin/silences", ADMIN_TOKEN, None).await;
    assert_eq!(listed["items"][0]["held_incidents"], 2, "{listed}");
    assert_eq!(listed["items"][0]["held_messages"], 1, "{listed}");
    assert_eq!(listed["items"][0]["active"], true, "{listed}");

    workflow::tick(&fixture.app).await.unwrap();
    assert_eq!(outbox(open.clone()).await, 0);
    let (status, _) = api_request(
        &admin,
        &format!("/v1/admin/silences/{silence_id}/cancel"),
        ADMIN_TOKEN,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    workflow::tick(&fixture.app).await.unwrap();
    assert_eq!(outbox(open.clone()).await, 1);
    assert_eq!(outbox(closed.clone()).await, 0);
    let (released,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM incidents WHERE silence_id IS NOT NULL")
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(released, 0);
    let (status, _) = api_request(
        &admin,
        &format!("/v1/admin/silences/{silence_id}/cancel"),
        ADMIN_TOKEN,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    for invalid in [
        json!({"service":"","minutes":30,"reason":"x"}),
        json!({"service":"checkout","minutes":0,"reason":"x"}),
        json!({"service":"checkout","minutes":20000,"reason":"x"}),
        json!({"service":"checkout","minutes":30,"reason":" "}),
    ] {
        let (status, body) = api_request(
            &admin,
            "/v1/admin/silences",
            ADMIN_TOKEN,
            Some(invalid.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{invalid} {body}");
    }
    let (status, _) = api_request(
        &admin,
        "/v1/admin/silences",
        VIEWER,
        Some(json!({"service":"checkout","minutes":30,"reason":"x"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    fixture.close().await;
}

#[tokio::test]
async fn evidence_export_writes_one_safe_csv_row_per_incident() {
    let Some(fixture) = Fixture::with_app(grant_admins).await else {
        return;
    };
    let tricky = json!({"event_id":"ev-1","service":"checkout","environment":"production","incident_key":"evidence","sequence":1,"severity":2,"recovered":false,"occurred_at":"2026-11-08T01:00:00Z","summary":"결제, \"카드\" 실패\n재시도 증가"});
    let id = create_incident(&fixture.router, tricky).await;
    let formula = json!({"event_id":"ev-2","service":"checkout","environment":"production","incident_key":"formula","sequence":1,"severity":3,"recovered":false,"occurred_at":"2026-11-08T02:00:00Z","summary":"=HYPERLINK(\"http://x\")"});
    create_incident(&fixture.router, formula).await;
    let (status, _) = api_request(
        &fixture.admin_router(),
        &format!("/v1/admin/incidents/{id}/ack"),
        ADMIN_TOKEN,
        Some(json!({"actor":"operator","sequence":1})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let today = chrono::Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap())
        .date_naive();
    let export = |path: String, credential: &'static str| {
        let router = fixture.admin_router();
        async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("authorization", format!("Bearer {credential}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let status = response.status();
            let headers = response.headers().clone();
            let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
            (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
        }
    };
    let (status, headers, csv) = export(
        format!("/v1/admin/evidence.csv?from={today}&to={today}&offset_minutes=540"),
        VIEWER,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{csv}");
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/csv")
    );
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("attachment")
    );
    assert!(
        csv.starts_with('\u{feff}'),
        "Excel needs the BOM to read UTF-8"
    );
    let header = csv.trim_start_matches('\u{feff}').lines().next().unwrap();
    assert!(header.contains("처음 발생 (UTC+09:00)"), "{header}");
    assert!(header.contains("메일 접수 수(수신 확인 아님)"), "{header}");
    assert!(header.contains("점검 무음 중 발생"), "{header}");
    assert!(
        csv.contains("\"결제, \"\"카드\"\" 실패\n재시도 증가\""),
        "{csv}"
    );
    assert!(
        csv.contains("'=HYPERLINK(\"\"http://x\"\")")
            || csv.contains("\"'=HYPERLINK(\"\"http://x\"\")\""),
        "{csv}"
    );
    assert!(csv.contains(",operator,"), "{csv}");
    assert!(csv.contains("2026-11-08 10:00:00"), "{csv}");
    let (status, _, empty) = export(
        "/v1/admin/evidence.csv?from=2020-01-01&to=2020-01-02".into(),
        VIEWER,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty.trim_start_matches('\u{feff}').lines().count(), 1);
    let (status, _, _) = export(
        "/v1/admin/evidence.csv?from=2026-01-01&to=2026-12-31".into(),
        VIEWER,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _, _) = export(
        "/v1/admin/evidence.csv?from=2026-11-09&to=2026-11-08".into(),
        VIEWER,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    fixture.close().await;
}

#[tokio::test]
async fn silence_keeps_messages_for_incidents_announced_before_it() {
    let (api, calls, server) = slack_mock().await;
    let Some(fixture) = Fixture::with_config(|config| config.api_url = api.clone()).await else {
        server.abort();
        return;
    };
    let db = &fixture.app.db;
    let event = |key: &str, sequence: i64, severity: i64, recovered: bool| json!({"event_id":format!("{key}-{sequence}"),"service":"checkout","environment":"production","incident_key":key,"sequence":sequence,"severity":severity,"recovered":recovered,"occurred_at":"2026-11-08T01:00:00Z"});
    let resolves = create_incident(&fixture.router, event("announced-resolves", 1, 2, false)).await;
    let rises = create_incident(&fixture.router, event("announced-rises", 1, 2, false)).await;
    drain(&fixture.app).await;
    wait_for_call(&calls, "chat.postMessage").await;
    let posted = |id: String| async move {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM outbox WHERE incident_id=$1")
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    };
    let (before_resolves, before_rises) =
        (posted(resolves.clone()).await, posted(rises.clone()).await);
    sqlx::query("INSERT INTO silences(id,service,environment,ends_at,reason,created_by) VALUES('s1','checkout','production',now()+interval '1 hour','점검','test')")
        .execute(db)
        .await
        .unwrap();
    assert_eq!(
        send(&fixture.router, event("announced-resolves", 2, 2, true))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(posted(resolves.clone()).await, before_resolves + 1);
    assert_eq!(
        send(&fixture.router, event("announced-rises", 2, 1, false))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(
        send(&fixture.router, event("announced-rises", 3, 1, true))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(posted(rises.clone()).await, before_rises + 2);
    let (held,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM incidents WHERE silence_id IS NOT NULL")
            .fetch_one(db)
            .await
            .unwrap();
    assert_eq!(held, 0);
    server.abort();
    fixture.close().await;
}
