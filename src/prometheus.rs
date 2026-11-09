//! Accepts alerts that Prometheus posts to an Alertmanager (`POST /api/v2/alerts`), so Prometheus can send to noti-mesh directly.
use crate::{App, Failure, accept_event, alertmanager, db_error};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use std::sync::Arc;

/// Alertmanager's default `resolve_timeout`, used when an alert has no end time.
const RESOLVE_TIMEOUT: chrono::Duration = chrono::Duration::minutes(5);
const EXPIRE_BATCH: i64 = 50;

pub async fn ingest(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let (source, service) = alertmanager::client_service(&app, &headers).await?;
    let Some(alerts) = payload.as_array() else {
        return Err((
            StatusCode::BAD_REQUEST,
            "expected an array of alerts".into(),
        ));
    };
    let now = Utc::now();
    let mut rejected = Vec::new();
    for alert in alerts {
        match receive(&app, &source, &service, alert, now).await {
            Ok(()) => {}
            Err((status, reason)) if status.is_client_error() => {
                rejected.push(json!({"labels":alert["labels"],"error":reason}))
            }
            // Prometheus sends every active alert again at the next resend, so a failed alert is retried then.
            Err(failure) => return Err(failure),
        }
    }
    // Like Alertmanager, a 400 tells Prometheus which alerts were invalid; the valid ones are kept.
    let status = if rejected.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::BAD_REQUEST
    };
    Ok((status, Json(json!({"rejected":rejected}))))
}

struct Alert {
    fingerprint: String,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    labels: Value,
    annotations: Value,
    generator_url: Option<String>,
}

async fn receive(
    app: &App,
    source: &str,
    service: &str,
    raw: &Value,
    now: DateTime<Utc>,
) -> Result<(), Failure> {
    let invalid = |reason: &str| (StatusCode::BAD_REQUEST, reason.to_owned());
    let labels = raw["labels"].clone();
    if labels.as_object().is_none_or(|map| map.is_empty()) {
        return Err(invalid("an alert needs at least one label"));
    }
    let annotations = match &raw["annotations"] {
        Value::Null => json!({}),
        value @ Value::Object(_) => value.clone(),
        _ => return Err(invalid("annotations must be an object")),
    };
    let fingerprint = alertmanager::fingerprint(&json!({"labels":labels}));
    let mut tx = app.db.begin().await.map_err(db_error)?;
    let starts_at = match time(&raw["startsAt"]) {
        Some(at) => at,
        // Alertmanager keeps the start of an active alert that is resent without one.
        None => sqlx::query_scalar::<_, i64>("SELECT (extract(epoch FROM starts_at)*1000)::bigint FROM prometheus_alerts WHERE source=$1 AND fingerprint=$2 AND resolved_at IS NULL ORDER BY starts_at DESC LIMIT 1")
            .bind(source)
            .bind(&fingerprint)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?
            .and_then(DateTime::from_timestamp_millis)
            .unwrap_or(now),
    };
    let ends_at = time(&raw["endsAt"]).unwrap_or(now + RESOLVE_TIMEOUT);
    let alert = Alert {
        fingerprint,
        starts_at,
        ends_at,
        labels,
        annotations,
        generator_url: raw["generatorURL"].as_str().map(str::to_owned),
    };
    let resolved = alert.ends_at <= now;
    let row = sqlx::query("SELECT generation, resolved_at IS NOT NULL AS resolved FROM prometheus_alerts WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz FOR UPDATE")
        .bind(source)
        .bind(&alert.fingerprint)
        .bind(alert.starts_at.to_rfc3339())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
    let generation = match row {
        None => {
            sqlx::query("INSERT INTO prometheus_alerts(source,service,fingerprint,starts_at,ends_at,labels,annotations,generator_url,resolved_at) VALUES($1,$2,$3,$4::timestamptz,$5::timestamptz,$6,$7,$8,CASE WHEN $9 THEN $5::timestamptz END)")
                .bind(source)
                .bind(service)
                .bind(&alert.fingerprint)
                .bind(alert.starts_at.to_rfc3339())
                .bind(alert.ends_at.to_rfc3339())
                .bind(&alert.labels)
                .bind(&alert.annotations)
                .bind(&alert.generator_url)
                .bind(resolved)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            Some(0)
        }
        Some(row) => {
            let generation: i32 = row.get("generation");
            let was_resolved: bool = row.get("resolved");
            // An alert resolved by the timer that comes back with the same start opens a new incident.
            let next = match (was_resolved, resolved) {
                (true, true) => None,
                (true, false) => Some(generation + 1),
                (false, true) => Some(generation),
                (false, false) => None,
            };
            sqlx::query("UPDATE prometheus_alerts SET ends_at=CASE WHEN resolved_at IS NOT NULL AND $5 THEN ends_at ELSE $4::timestamptz END,labels=$6,annotations=$7,generator_url=$8,generation=$9,resolved_at=CASE WHEN $5 THEN coalesce(resolved_at,$4::timestamptz) END,updated_at=now() WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz")
                .bind(source)
                .bind(&alert.fingerprint)
                .bind(alert.starts_at.to_rfc3339())
                .bind(alert.ends_at.to_rfc3339())
                .bind(resolved)
                .bind(&alert.labels)
                .bind(&alert.annotations)
                .bind(&alert.generator_url)
                .bind(next.unwrap_or(generation))
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            next
        }
    };
    if let Some(generation) = generation {
        announce(app, source, service, &alert, generation, resolved).await?;
    }
    // The row commits only after the incident change, so a failed change is retried on the next resend.
    tx.commit().await.map_err(db_error)
}

/// Passes a state change of one alert to the incident workflow as an Alertmanager-style event.
async fn announce(
    app: &App,
    source: &str,
    service: &str,
    alert: &Alert,
    generation: i32,
    resolved: bool,
) -> Result<(), Failure> {
    let unset = "0001-01-01T00:00:00Z".to_owned();
    let mut value = json!({
        "status": if resolved { "resolved" } else { "firing" },
        "labels": alert.labels,
        "annotations": alert.annotations,
        "startsAt": alert.starts_at.to_rfc3339(),
        "endsAt": if resolved { alert.ends_at.to_rfc3339() } else { unset },
    });
    if let Some(url) = &alert.generator_url {
        value["generatorURL"] = json!(url);
    }
    let mut event = alertmanager::to_event(&value, &alert.fingerprint, None, service, "production")
        .map_err(|reason| (StatusCode::BAD_REQUEST, reason))?;
    if generation > 0 {
        event.event_id = format!("{}-r{generation}", event.event_id);
        event.incident_key = format!("{}/r{generation}", event.incident_key);
    }
    match accept_event(app, source.to_owned(), event, false).await {
        Ok(_) => Ok(()),
        // The same change was already recorded with other annotations.
        Err((StatusCode::CONFLICT, reason)) if reason == "event ID content conflict" => Ok(()),
        Err(failure) => Err(failure),
    }
}

/// Resolves alerts whose end time passed without a resend, as Alertmanager does when Prometheus stops sending.
pub(crate) async fn expire(app: &App) -> Result<(), sqlx::Error> {
    let mut tx = app.db.begin().await?;
    let rows = sqlx::query("SELECT source,service,fingerprint,generation,labels,annotations,generator_url,starts_at::text AS starts_key,(extract(epoch FROM starts_at)*1000)::bigint AS starts_ms,(extract(epoch FROM ends_at)*1000)::bigint AS ends_ms FROM prometheus_alerts WHERE resolved_at IS NULL AND ends_at<=now() ORDER BY ends_at LIMIT $1 FOR UPDATE SKIP LOCKED")
        .bind(EXPIRE_BATCH)
        .fetch_all(&mut *tx)
        .await?;
    for row in rows {
        let source: String = row.get("source");
        let starts_key: String = row.get("starts_key");
        let service: String = row.get("service");
        let millis =
            |name: &str| DateTime::from_timestamp_millis(row.get(name)).unwrap_or_default();
        let alert = Alert {
            fingerprint: row.get("fingerprint"),
            starts_at: millis("starts_ms"),
            ends_at: millis("ends_ms"),
            labels: row.get("labels"),
            annotations: row.get("annotations"),
            generator_url: row.get("generator_url"),
        };
        match announce(app, &source, &service, &alert, row.get("generation"), true).await {
            Ok(()) => {}
            // A rejected change would be rejected again, so the alert is closed instead of retried forever.
            Err((status, reason)) if status.is_client_error() => {
                eprintln!("prometheus alert {}: {reason}", alert.fingerprint)
            }
            Err((_, reason)) => {
                eprintln!("prometheus alert {}: {reason}", alert.fingerprint);
                break;
            }
        }
        sqlx::query("UPDATE prometheus_alerts SET resolved_at=ends_at,updated_at=now() WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz")
            .bind(&source)
            .bind(&alert.fingerprint)
            .bind(&starts_key)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await
}

fn time(value: &Value) -> Option<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(value.as_str()?).ok()?;
    // Prometheus and Alertmanager send year 1 for an unset time.
    // Event IDs use milliseconds, so the stored time must not keep a finer part that rounds differently.
    (parsed.timestamp() > 0)
        .then(|| DateTime::from_timestamp_millis(parsed.timestamp_millis()))
        .flatten()
}
