//! Accepts Uptime Kuma, Sentry, and Datadog notifications as incident events.
use crate::alertmanager::{clip, link, severity};
use crate::{App, Event, Failure, accept_event, db_error};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

const SUMMARY_LIMIT: usize = 1000;

#[derive(Deserialize)]
pub struct HookQuery {
    environment: Option<String>,
    severity: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

/// What a notification asks for after it is converted.
#[cfg_attr(test, derive(Debug, PartialEq))]
enum Step {
    Accept(Box<Event>),
    /// Tests, pending checks, and resolutions of unknown issues create nothing.
    Ignore(&'static str),
}

pub async fn uptime_kuma(
    State(app): State<Arc<App>>,
    Query(query): Query<HookQuery>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let (source, service) = crate::alertmanager::client_service(&app, &headers).await?;
    let environment = environment(&query);
    let open = match payload["monitor"]["id"].as_i64() {
        Some(id) => {
            open_key(
                &app,
                &source,
                &service,
                &environment,
                &format!("kuma/{id}/"),
            )
            .await?
        }
        None => None,
    };
    let step = kuma_step(
        &payload,
        &service,
        &environment,
        query.severity.as_deref(),
        open,
    )
    .map_err(|reason| (StatusCode::UNPROCESSABLE_ENTITY, reason))?;
    finish(&app, source, step, query.dry_run).await
}

pub async fn sentry(
    State(app): State<Arc<App>>,
    Path(token): Path<String>,
    Query(query): Query<HookQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), Failure> {
    // Sentry integrations cannot add an Authorization header, so the token is in the URL.
    let Some((source, service)) = crate::slack_hooks::client(&app, &token).await? else {
        return Err((StatusCode::NOT_FOUND, "unknown token".into()));
    };
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid JSON".to_owned()))?;
    let resource = headers
        .get("sentry-hook-resource")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let environment = environment(&query);
    let issue = match resource {
        "event_alert" => payload["data"]["event"]["issue_id"].as_str(),
        "issue" => payload["data"]["issue"]["id"].as_str(),
        _ => None,
    };
    let event_environment = payload["data"]["event"]["environment"]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(&environment)
        .to_owned();
    // An issue webhook has no environment, so its resolution finds the incident in any environment.
    let (open, event_environment) = match (resource, issue) {
        ("issue", Some(id)) => {
            match open_issue(&app, &source, &service, &format!("sentry/{id}/")).await? {
                Some((key, environment)) => (Some(key), environment),
                None => (None, event_environment),
            }
        }
        (_, Some(id)) => (
            open_key(
                &app,
                &source,
                &service,
                &event_environment,
                &format!("sentry/{id}/"),
            )
            .await?,
            event_environment,
        ),
        _ => (None, event_environment),
    };
    let received = headers
        .get("sentry-hook-timestamp")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .map_or_else(|| chrono::Utc::now().timestamp_millis(), |s| s * 1000);
    let step = sentry_step(
        resource,
        &payload,
        &service,
        &event_environment,
        open,
        received,
    )
    .map_err(|reason| (StatusCode::UNPROCESSABLE_ENTITY, reason))?;
    finish(&app, source, step, query.dry_run).await
}

pub async fn datadog(
    State(app): State<Arc<App>>,
    Query(query): Query<HookQuery>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let (source, service) = crate::alertmanager::client_service(&app, &headers).await?;
    let step = datadog_step(&payload, &service, &environment(&query))
        .map_err(|reason| (StatusCode::UNPROCESSABLE_ENTITY, reason))?;
    finish(&app, source, step, query.dry_run).await
}

fn environment(query: &HookQuery) -> String {
    query
        .environment
        .clone()
        .unwrap_or_else(|| "production".into())
}

async fn finish(
    app: &App,
    source: String,
    step: Step,
    dry_run: bool,
) -> Result<(StatusCode, Json<Value>), Failure> {
    match step {
        Step::Ignore(reason) => Ok((StatusCode::OK, Json(json!({"ignored":reason})))),
        Step::Accept(event) => match accept_event(app, source, *event, dry_run).await {
            // Monitors resend the same notification after a timeout; the first copy already counts.
            Err((StatusCode::CONFLICT, reason)) if reason == "event ID content conflict" => {
                Ok((StatusCode::OK, Json(json!({"ignored":"duplicate"}))))
            }
            other => other,
        },
    }
}

/// Returns the key of the open incident that a recovery for `prefix` should close.
async fn open_key(
    app: &App,
    source: &str,
    service: &str,
    environment: &str,
    prefix: &str,
) -> Result<Option<String>, Failure> {
    sqlx::query_scalar("SELECT incident_key FROM incidents WHERE source=$1 AND service=$2 AND environment=$3 AND left(incident_key,length($4))=$4 AND state<>'resolved' ORDER BY created_at DESC LIMIT 1")
        .bind(source)
        .bind(service)
        .bind(environment)
        .bind(prefix)
        .fetch_optional(&app.db)
        .await
        .map_err(db_error)
}

/// Returns the key and environment of the open incident for a Sentry issue in any environment.
async fn open_issue(
    app: &App,
    source: &str,
    service: &str,
    prefix: &str,
) -> Result<Option<(String, String)>, Failure> {
    sqlx::query_as("SELECT incident_key,environment FROM incidents WHERE source=$1 AND service=$2 AND left(incident_key,length($3))=$3 AND state<>'resolved' ORDER BY created_at DESC LIMIT 1")
        .bind(source)
        .bind(service)
        .bind(prefix)
        .fetch_optional(&app.db)
        .await
        .map_err(db_error)
}

fn kuma_step(
    payload: &Value,
    service: &str,
    environment: &str,
    severity_name: Option<&str>,
    open: Option<String>,
) -> Result<Step, String> {
    let heartbeat = &payload["heartbeat"];
    // The "Test" button in Uptime Kuma sends only `msg`.
    if heartbeat.is_null() {
        return Ok(Step::Ignore("test notification"));
    }
    let monitor = payload["monitor"]["id"]
        .as_i64()
        .ok_or("missing monitor.id")?;
    let down = match heartbeat["status"].as_i64() {
        Some(0) => true,
        Some(1) => false,
        Some(2) => return Ok(Step::Ignore("pending")),
        Some(3) => return Ok(Step::Ignore("maintenance")),
        _ => return Err("missing or unknown heartbeat.status".into()),
    };
    // Uptime Kuma stores and sends heartbeat.time in UTC.
    let at = heartbeat["time"]
        .as_str()
        .and_then(|t| chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M:%S%.f").ok())
        .ok_or("missing or invalid heartbeat.time")?
        .and_utc();
    if !down && open.is_none() {
        // The incident is already resolved, or noti-mesh never saw the outage.
        return Ok(Step::Ignore("no open incident"));
    }
    let incident_key = open.unwrap_or_else(|| format!("kuma/{monitor}/{}", at.timestamp_millis()));
    let name = payload["monitor"]["name"].as_str().unwrap_or("monitor");
    let summary = payload["msg"]
        .as_str()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or(name);
    Ok(Step::Accept(Box::new(Event {
        event_id: format!(
            "kuma-{monitor}-{}-{}",
            at.timestamp_millis(),
            if down { "down" } else { "up" }
        ),
        service: service.to_owned(),
        environment: environment.to_owned(),
        incident_key,
        sequence: at.timestamp_millis(),
        severity: severity_name.map_or(3, severity),
        recovered: !down,
        occurred_at: at.to_rfc3339(),
        correlation_id: None,
        error_code: None,
        summary: Some(clip(summary.trim(), SUMMARY_LIMIT)),
        source_url: payload["monitor"]["url"].as_str().and_then(link),
    })))
}

fn sentry_step(
    resource: &str,
    payload: &Value,
    service: &str,
    environment: &str,
    open: Option<String>,
    received_ms: i64,
) -> Result<Step, String> {
    match resource {
        "event_alert" => {
            let event = &payload["data"]["event"];
            let issue = event["issue_id"].as_str().ok_or("missing issue_id")?;
            let id = event["event_id"].as_str().ok_or("missing event_id")?;
            let at = event["datetime"]
                .as_str()
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .ok_or("missing or invalid datetime")?;
            let title = event["title"].as_str().unwrap_or("Sentry issue");
            let summary = match event["culprit"].as_str().filter(|c| !c.is_empty()) {
                Some(culprit) => format!("{title} · {culprit}"),
                None => title.to_owned(),
            };
            Ok(Step::Accept(Box::new(Event {
                event_id: format!("sentry-{}", clip(id, 64)),
                service: service.to_owned(),
                environment: environment.to_owned(),
                incident_key: open
                    .unwrap_or_else(|| format!("sentry/{issue}/{}", at.timestamp_millis())),
                sequence: at.timestamp_millis(),
                severity: sentry_level(event["level"].as_str().unwrap_or("")),
                recovered: false,
                occurred_at: at.to_rfc3339(),
                correlation_id: None,
                error_code: None,
                summary: Some(clip(&summary, SUMMARY_LIMIT)),
                source_url: event["web_url"].as_str().and_then(link),
            })))
        }
        "issue" if payload["action"] == "resolved" => {
            let issue = &payload["data"]["issue"];
            let id = issue["id"].as_str().ok_or("missing issue id")?;
            // Sentry reports every resolved issue, including ones that never raised an alert.
            let Some(incident_key) = open else {
                return Ok(Step::Ignore("no open incident"));
            };
            let at = chrono::DateTime::from_timestamp_millis(received_ms)
                .ok_or("invalid Sentry-Hook-Timestamp")?;
            Ok(Step::Accept(Box::new(Event {
                event_id: format!("sentry-issue-{}-resolved-{received_ms}", clip(id, 64)),
                service: service.to_owned(),
                environment: environment.to_owned(),
                incident_key,
                sequence: received_ms,
                severity: sentry_level(issue["level"].as_str().unwrap_or("")),
                recovered: true,
                occurred_at: at.to_rfc3339(),
                correlation_id: None,
                error_code: None,
                summary: issue["title"]
                    .as_str()
                    .map(|title| clip(title, SUMMARY_LIMIT)),
                source_url: issue["web_url"].as_str().and_then(link),
            })))
        }
        "issue" => Ok(Step::Ignore("issue action")),
        _ => Ok(Step::Ignore("resource")),
    }
}

fn sentry_level(level: &str) -> i16 {
    match level {
        "fatal" => 1,
        "error" => 2,
        "info" | "debug" => 4,
        _ => 3,
    }
}

fn datadog_step(payload: &Value, service: &str, environment: &str) -> Result<Step, String> {
    let text = |key: &str| {
        payload[key]
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    let cycle = text("cycle").ok_or("missing cycle")?;
    let id = text("id").ok_or("missing id")?;
    let transition = text("transition").ok_or("missing transition")?;
    // $LAST_UPDATED is epoch milliseconds; templates may quote it.
    let at_ms = payload["date"]
        .as_i64()
        .or_else(|| text("date").and_then(|d| d.parse().ok()))
        .ok_or("missing or invalid date")?;
    let at = chrono::DateTime::from_timestamp_millis(at_ms).ok_or("invalid date")?;
    let recovered = transition == "Recovered";
    let priority = text("priority").unwrap_or("");
    let severity = match priority.to_ascii_lowercase().as_str() {
        "p5" => 4,
        "" if transition.ends_with("Warn") => 3,
        "" => 2,
        other => severity(other),
    };
    Ok(Step::Accept(Box::new(Event {
        event_id: format!("dd-{}", clip(id, 64)),
        service: service.to_owned(),
        environment: environment.to_owned(),
        incident_key: format!("datadog/{}", clip(cycle, 128)),
        sequence: at_ms,
        severity,
        recovered,
        occurred_at: at.to_rfc3339(),
        correlation_id: None,
        error_code: None,
        summary: text("title").map(|title| clip(title, SUMMARY_LIMIT)),
        source_url: text("link").and_then(link),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed from a notification that Uptime Kuma 1.23.17 sent during a live test.
    fn kuma(status: i64, time: &str, msg: &str) -> Value {
        json!({
            "heartbeat":{"monitorID":1,"status":status,"time":time,"msg":"connect ECONNREFUSED","important":true,"duration":20,"timezone":"UTC","timezoneOffset":"+00:00","localDateTime":time},
            "monitor":{"id":1,"name":"checkout-api 상태 확인","url":"http://host.docker.internal:19111/","type":"http","active":true,"tags":[]},
            "msg":msg
        })
    }

    fn accepted(step: Result<Step, String>) -> Event {
        match step {
            Ok(Step::Accept(event)) => *event,
            other => panic!("expected an event, got {other:?}"),
        }
    }

    #[test]
    fn kuma_down_opens_and_up_closes_the_same_incident() {
        let down = accepted(kuma_step(
            &kuma(
                0,
                "2026-11-08 13:59:30.903",
                "[checkout-api 상태 확인] [🔴 Down] connect ECONNREFUSED",
            ),
            "checkout",
            "dev",
            Some("critical"),
            None,
        ));
        assert_eq!(down.incident_key, "kuma/1/1794146370903");
        assert_eq!(down.event_id, "kuma-1-1794146370903-down");
        assert_eq!((down.severity, down.recovered), (1, false));
        assert_eq!(down.occurred_at, "2026-11-08T13:59:30.903+00:00");
        assert_eq!(
            down.summary.as_deref(),
            Some("[checkout-api 상태 확인] [🔴 Down] connect ECONNREFUSED")
        );
        assert_eq!(
            down.source_url.as_deref(),
            Some("http://host.docker.internal:19111/")
        );
        let up = accepted(kuma_step(
            &kuma(
                1,
                "2026-11-08 13:59:50.951",
                "[checkout-api 상태 확인] [✅ Up] 200 - OK",
            ),
            "checkout",
            "dev",
            None,
            Some(down.incident_key.clone()),
        ));
        assert_eq!(up.incident_key, down.incident_key);
        assert!(up.recovered && up.sequence > down.sequence);
        assert_eq!(up.severity, 3);
    }

    #[test]
    fn kuma_tests_pending_and_unknown_recoveries_create_nothing() {
        let test = json!({"heartbeat":null,"monitor":null,"msg":"Uptime Kuma Testing..."});
        assert_eq!(
            kuma_step(&test, "s", "e", None, None),
            Ok(Step::Ignore("test notification"))
        );
        assert_eq!(
            kuma_step(
                &kuma(2, "2026-11-08 13:59:30.903", "p"),
                "s",
                "e",
                None,
                None
            ),
            Ok(Step::Ignore("pending"))
        );
        assert_eq!(
            kuma_step(
                &kuma(1, "2026-11-08 13:59:30.903", "up"),
                "s",
                "e",
                None,
                None
            ),
            Ok(Step::Ignore("no open incident"))
        );
        assert!(kuma_step(&kuma(0, "yesterday", "x"), "s", "e", None, None).is_err());
    }

    // Trimmed from the event_alert example in the Sentry integration platform documentation.
    fn sentry_alert() -> Value {
        json!({"action":"triggered","actor":{"id":"sentry","name":"Sentry","type":"application"},"data":{"event":{
            "event_id":"e4874d664c3540c1a32eab185f12c5ab","issue_id":"1117540176",
            "issue_url":"https://sentry.io/api/0/issues/1117540176/",
            "web_url":"https://sentry.io/organizations/test-org/issues/1117540176/events/e4874d664c3540c1a32eab185f12c5ab/",
            "title":"ReferenceError: heck is not defined","culprit":"?(<anonymous>)","level":"error",
            "datetime":"2019-08-19T21:06:17.677000Z","environment":null},
            "triggered_rule":"Very Important Alert!"},"installation":{"uuid":"a8e5d37a-696c-4c54-adb5-b3f28d64c7de"}})
    }

    #[test]
    fn sentry_alert_opens_an_incident_for_the_issue() {
        let event = accepted(sentry_step(
            "event_alert",
            &sentry_alert(),
            "web",
            "production",
            None,
            0,
        ));
        assert_eq!(event.incident_key, "sentry/1117540176/1566248777677");
        assert_eq!(event.event_id, "sentry-e4874d664c3540c1a32eab185f12c5ab");
        assert_eq!((event.severity, event.recovered), (2, false));
        assert_eq!(
            event.summary.as_deref(),
            Some("ReferenceError: heck is not defined · ?(<anonymous>)")
        );
        assert!(
            event
                .source_url
                .unwrap()
                .starts_with("https://sentry.io/organizations/")
        );
        let again = accepted(sentry_step(
            "event_alert",
            &sentry_alert(),
            "web",
            "production",
            Some("sentry/1117540176/1".into()),
            0,
        ));
        assert_eq!(again.incident_key, "sentry/1117540176/1");
    }

    #[test]
    fn sentry_resolution_closes_only_an_open_incident() {
        // Trimmed from the issue webhook example in the Sentry documentation.
        let resolved = json!({"action":"resolved","data":{"issue":{"id":"1234567890","shortId":"PYTHON-Y","title":"Error generated","level":"fatal","status":"resolved","web_url":"https://example-org.sentry.io/issues/1234567890/"}}});
        assert_eq!(
            sentry_step(
                "issue",
                &resolved,
                "web",
                "production",
                None,
                1_794_150_000_000
            ),
            Ok(Step::Ignore("no open incident"))
        );
        let event = accepted(sentry_step(
            "issue",
            &resolved,
            "web",
            "production",
            Some("sentry/1234567890/1".into()),
            1_794_150_000_000,
        ));
        assert_eq!(event.incident_key, "sentry/1234567890/1");
        assert!(event.recovered);
        assert_eq!(event.sequence, 1_794_150_000_000);
        let created = json!({"action":"created","data":{"issue":{"id":"1"}}});
        assert_eq!(
            sentry_step("issue", &created, "web", "production", None, 0),
            Ok(Step::Ignore("issue action"))
        );
        assert_eq!(
            sentry_step("installation", &created, "web", "production", None, 0),
            Ok(Step::Ignore("resource"))
        );
    }

    #[test]
    fn datadog_template_maps_transitions() {
        let triggered = json!({"id":"7781230","cycle":"7212345678901234567","transition":"Triggered","priority":"P1","date":"1794150000000","title":"[Triggered] checkout 5xx","link":"https://app.datadoghq.com/monitors/123"});
        let event = accepted(datadog_step(&triggered, "checkout", "production"));
        assert_eq!(event.incident_key, "datadog/7212345678901234567");
        assert_eq!(
            (event.severity, event.recovered, event.sequence),
            (1, false, 1_794_150_000_000)
        );
        let mut recovered = triggered.clone();
        recovered["id"] = json!("7781299");
        recovered["transition"] = json!("Recovered");
        recovered["date"] = json!(1_794_150_060_000_i64);
        let event = accepted(datadog_step(&recovered, "checkout", "production"));
        assert!(event.recovered);
        let mut warn = triggered;
        warn["transition"] = json!("Warn");
        warn["priority"] = json!("");
        assert_eq!(accepted(datadog_step(&warn, "c", "p")).severity, 3);
        assert!(
            datadog_step(
                &json!({"id":"1","transition":"Triggered","date":"1"}),
                "c",
                "p"
            )
            .is_err()
        );
    }
}
