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
    // A stored alert stays in the group it joined, even if the grouping labels change later.
    let stored: Option<Option<String>> = sqlx::query_scalar("SELECT group_hash FROM prometheus_alerts WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz")
        .bind(source)
        .bind(&alert.fingerprint)
        .bind(alert.starts_at.to_rfc3339())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
    let group_hash = stored
        .flatten()
        .unwrap_or_else(|| group_hash(&app.config.prometheus.group_by, &alert.labels));
    let mut group = Group::lock(&mut tx, source, &group_hash).await?;
    let row = sqlx::query("SELECT generation, resolved_at IS NOT NULL AS resolved, group_hash FROM prometheus_alerts WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz FOR UPDATE")
        .bind(source)
        .bind(&alert.fingerprint)
        .bind(alert.starts_at.to_rfc3339())
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?;
    let Some(row) = row else {
        sqlx::query("INSERT INTO prometheus_alerts(source,service,fingerprint,starts_at,ends_at,labels,annotations,generator_url,resolved_at,group_hash) VALUES($1,$2,$3,$4::timestamptz,$5::timestamptz,$6,$7,$8,CASE WHEN $9 THEN $5::timestamptz END,$10)")
            .bind(source)
            .bind(service)
            .bind(&alert.fingerprint)
            .bind(alert.starts_at.to_rfc3339())
            .bind(alert.ends_at.to_rfc3339())
            .bind(&alert.labels)
            .bind(&alert.annotations)
            .bind(&alert.generator_url)
            .bind(resolved)
            .bind(&group_hash)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        // An alert that arrives already resolved was never announced, so nobody needs to hear about it.
        if !resolved {
            group
                .join(&mut tx, app, source, service, &alert, 0, now)
                .await?;
        }
        // The rows commit only after the incident change, so a failed change is retried on the next resend.
        return tx.commit().await.map_err(db_error);
    };
    let generation: i32 = row.get("generation");
    let was_resolved: bool = row.get("resolved");
    let grouped = row.get::<Option<String>, _>("group_hash").is_some();
    // An alert resolved by the timer that comes back with the same start counts as a new alert.
    let next = if was_resolved && !resolved {
        generation + 1
    } else {
        generation
    };
    sqlx::query("UPDATE prometheus_alerts SET ends_at=CASE WHEN resolved_at IS NOT NULL AND $5 THEN ends_at ELSE $4::timestamptz END,labels=$6,annotations=$7,generator_url=$8,generation=$9,resolved_at=CASE WHEN $5 THEN coalesce(resolved_at,$4::timestamptz) END,group_hash=$10,updated_at=now() WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz")
        .bind(source)
        .bind(&alert.fingerprint)
        .bind(alert.starts_at.to_rfc3339())
        .bind(alert.ends_at.to_rfc3339())
        .bind(resolved)
        .bind(&alert.labels)
        .bind(&alert.annotations)
        .bind(&alert.generator_url)
        .bind(next)
        .bind(&group_hash)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    match (was_resolved, resolved) {
        (true, false) => {
            group
                .join(&mut tx, app, source, service, &alert, next, now)
                .await?
        }
        (false, true) if grouped => {
            group
                .leave(&mut tx, app, source, service, &alert, now)
                .await?
        }
        (false, true) => announce(app, source, service, &alert, generation).await?,
        _ => {}
    }
    tx.commit().await.map_err(db_error)
}

/// Groups alerts by environment and the configured label values, like Alertmanager's `group_by`.
fn group_hash(group_by: &[String], labels: &Value) -> String {
    use sha2::{Digest, Sha256};
    let environment = labels["environment"]
        .as_str()
        .or_else(|| labels["env"].as_str())
        .unwrap_or("production");
    let values = if group_by.iter().any(|label| label == "...") {
        labels.clone()
    } else {
        // A missing label groups as an empty value, as in Alertmanager.
        Value::Object(
            group_by
                .iter()
                .map(|label| (label.clone(), json!(labels[label].as_str().unwrap_or(""))))
                .collect(),
        )
    };
    hex::encode(Sha256::digest(
        json!([environment, values]).to_string().as_bytes(),
    ))[..16]
        .to_owned()
}

/// The incident that the active alerts of one group share.
struct Group {
    source: String,
    hash: String,
    generation: i32,
    incident_key: String,
    sequence: i64,
    open: bool,
}

impl Group {
    async fn lock(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        source: &str,
        hash: &str,
    ) -> Result<Self, Failure> {
        sqlx::query(
            "INSERT INTO prometheus_groups(source,group_hash) VALUES($1,$2) ON CONFLICT DO NOTHING",
        )
        .bind(source)
        .bind(hash)
        .execute(&mut **tx)
        .await
        .map_err(db_error)?;
        let row = sqlx::query("SELECT generation,incident_key,sequence,open FROM prometheus_groups WHERE source=$1 AND group_hash=$2 FOR UPDATE")
            .bind(source)
            .bind(hash)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
        Ok(Self {
            source: source.to_owned(),
            hash: hash.to_owned(),
            generation: row.get("generation"),
            incident_key: row.get("incident_key"),
            sequence: row.get("sequence"),
            open: row.get("open"),
        })
    }

    /// Orders group events by receipt, because a member can join with an earlier start than the first one.
    fn next_sequence(&mut self, now: DateTime<Utc>) -> i64 {
        self.sequence = now.timestamp_millis().max(self.sequence + 1);
        self.sequence
    }

    async fn save(&self, tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<(), Failure> {
        sqlx::query("UPDATE prometheus_groups SET generation=$3,incident_key=$4,sequence=$5,open=$6 WHERE source=$1 AND group_hash=$2")
            .bind(&self.source)
            .bind(&self.hash)
            .bind(self.generation)
            .bind(&self.incident_key)
            .bind(self.sequence)
            .bind(self.open)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        Ok(())
    }

    /// Adds an active alert to the open incident of the group, or opens a new one.
    #[allow(clippy::too_many_arguments)] // The transaction, the sender, and the alert all come from the caller.
    async fn join(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        app: &App,
        source: &str,
        service: &str,
        alert: &Alert,
        alert_generation: i32,
        now: DateTime<Utc>,
    ) -> Result<(), Failure> {
        if !self.open {
            if !self.incident_key.is_empty() {
                self.generation += 1;
            }
            let name: String = alert.labels["alertname"]
                .as_str()
                .unwrap_or("alert")
                .chars()
                .take(128)
                .collect();
            self.incident_key = format!("{name}/{}/g{}", self.hash, self.generation);
            self.open = true;
        }
        let mut event = event(alert, service, false)?;
        if alert_generation > 0 {
            event.event_id = format!("{}-r{alert_generation}", event.event_id);
        }
        event.incident_key = self.incident_key.clone();
        event.sequence = self.next_sequence(now);
        sqlx::query("UPDATE prometheus_alerts SET group_generation=$4 WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz")
            .bind(source)
            .bind(&alert.fingerprint)
            .bind(alert.starts_at.to_rfc3339())
            .bind(self.generation)
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
        self.save(tx).await?;
        deliver(app, source, event).await
    }

    /// Resolves the incident of the group when its last active alert resolves.
    async fn leave(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        app: &App,
        source: &str,
        service: &str,
        alert: &Alert,
        now: DateTime<Utc>,
    ) -> Result<(), Failure> {
        let active: i64 = sqlx::query_scalar("SELECT count(*) FROM prometheus_alerts WHERE source=$1 AND group_hash=$2 AND group_generation=$3 AND resolved_at IS NULL")
            .bind(source)
            .bind(&self.hash)
            .bind(self.generation)
            .fetch_one(&mut **tx)
            .await
            .map_err(db_error)?;
        if active > 0 || !self.open {
            return Ok(());
        }
        let mut event = event(alert, service, true)?;
        event.event_id = format!("amg-{}-g{}-resolved", self.hash, self.generation);
        event.incident_key = self.incident_key.clone();
        event.sequence = self.next_sequence(now);
        self.open = false;
        self.save(tx).await?;
        deliver(app, source, event).await
    }
}

/// Builds an Alertmanager-style event for one alert, which sets the summary, priority, and link.
fn event(alert: &Alert, service: &str, resolved: bool) -> Result<crate::Event, Failure> {
    let mut value = json!({
        "status": if resolved { "resolved" } else { "firing" },
        "labels": alert.labels,
        "annotations": alert.annotations,
        "startsAt": alert.starts_at.to_rfc3339(),
        "endsAt": if resolved { alert.ends_at.to_rfc3339() } else { "0001-01-01T00:00:00Z".to_owned() },
    });
    if let Some(url) = &alert.generator_url {
        value["generatorURL"] = json!(url);
    }
    alertmanager::to_event(&value, &alert.fingerprint, None, service, "production")
        .map_err(|reason| (StatusCode::BAD_REQUEST, reason))
}

async fn deliver(app: &App, source: &str, event: crate::Event) -> Result<(), Failure> {
    match accept_event(app, source.to_owned(), event, false).await {
        Ok(_) => Ok(()),
        // The same change was already recorded with other annotations.
        Err((StatusCode::CONFLICT, reason)) if reason == "event ID content conflict" => Ok(()),
        Err(failure) => Err(failure),
    }
}

/// Resolves the incident of an alert stored before grouping, which has its own incident.
async fn announce(
    app: &App,
    source: &str,
    service: &str,
    alert: &Alert,
    generation: i32,
) -> Result<(), Failure> {
    let mut event = event(alert, service, true)?;
    if generation > 0 {
        event.event_id = format!("{}-r{generation}", event.event_id);
        event.incident_key = format!("{}/r{generation}", event.incident_key);
    }
    deliver(app, source, event).await
}

/// Resolves alerts whose end time passed without a resend, as Alertmanager does when Prometheus stops sending.
pub(crate) async fn expire(app: &App) -> Result<(), sqlx::Error> {
    let due = sqlx::query("SELECT source,fingerprint,starts_at::text AS starts_key,group_hash FROM prometheus_alerts WHERE resolved_at IS NULL AND ends_at<=now() ORDER BY ends_at LIMIT $1")
        .bind(EXPIRE_BATCH)
        .fetch_all(&app.db)
        .await?;
    for candidate in due {
        let source: String = candidate.get("source");
        let starts_key: String = candidate.get("starts_key");
        let group_hash: Option<String> = candidate.get("group_hash");
        let mut tx = app.db.begin().await?;
        // Lock the group before the alert, in the same order as `receive`, so the two cannot deadlock.
        let mut group = match &group_hash {
            Some(hash) => match Group::lock(&mut tx, &source, hash).await {
                Ok(group) => Some(group),
                Err(_) => break,
            },
            None => None,
        };
        let Some(row) = sqlx::query("SELECT service,fingerprint,generation,labels,annotations,generator_url,(extract(epoch FROM starts_at)*1000)::bigint AS starts_ms,(extract(epoch FROM ends_at)*1000)::bigint AS ends_ms FROM prometheus_alerts WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz AND resolved_at IS NULL AND ends_at<=now() FOR UPDATE")
            .bind(&source)
            .bind(candidate.get::<String, _>("fingerprint"))
            .bind(&starts_key)
            .fetch_optional(&mut *tx)
            .await?
        else {
            // A resend arrived after the candidate was read.
            continue;
        };
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
        sqlx::query("UPDATE prometheus_alerts SET resolved_at=ends_at,updated_at=now() WHERE source=$1 AND fingerprint=$2 AND starts_at=$3::timestamptz")
            .bind(&source)
            .bind(&alert.fingerprint)
            .bind(&starts_key)
            .execute(&mut *tx)
            .await?;
        let result = match group.as_mut() {
            Some(group) => {
                group
                    .leave(&mut tx, app, &source, &service, &alert, Utc::now())
                    .await
            }
            None => announce(app, &source, &service, &alert, row.get("generation")).await,
        };
        match result {
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
        tx.commit().await?;
    }
    Ok(())
}

fn time(value: &Value) -> Option<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(value.as_str()?).ok()?;
    // Prometheus and Alertmanager send year 1 for an unset time.
    // Event IDs use milliseconds, so the stored time must not keep a finer part that rounds differently.
    (parsed.timestamp() > 0)
        .then(|| DateTime::from_timestamp_millis(parsed.timestamp_millis()))
        .flatten()
}
