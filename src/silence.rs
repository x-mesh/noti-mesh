//! Maintenance silences: hold notifications for a service while it is under planned work.
use crate::{App, Failure, db_error};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::sync::Arc;

const MAX_MINUTES: i64 = 7 * 24 * 60;

/// Returns the active silence for a service and environment, if any.
pub(crate) async fn active(
    tx: &mut Transaction<'_, Postgres>,
    service: &str,
    environment: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM silences WHERE service=$1 AND (environment IS NULL OR environment=$2) AND cancelled_at IS NULL AND starts_at<=now() AND ends_at>now() ORDER BY ends_at DESC LIMIT 1")
        .bind(service)
        .bind(environment)
        .fetch_optional(&mut **tx)
        .await
}

/// Announces held incidents whose silence ended. Incidents resolved meanwhile stay quiet.
pub(crate) async fn release(app: &App) -> Result<(), sqlx::Error> {
    let mut tx = app.db.begin().await?;
    let rows = sqlx::query("SELECT i.id,i.service,i.environment,i.severity,i.state FROM incidents i JOIN silences s ON s.id=i.silence_id WHERE s.cancelled_at IS NOT NULL OR s.ends_at<=now() FOR UPDATE OF i SKIP LOCKED LIMIT 100")
        .fetch_all(&mut *tx)
        .await?;
    for row in rows {
        let id: String = row.get("id");
        let service: String = row.get("service");
        let environment: String = row.get("environment");
        // An overlapping silence takes over instead of announcing the incident early.
        let next = active(&mut tx, &service, &environment).await?;
        sqlx::query("UPDATE incidents SET silence_id=$2 WHERE id=$1")
            .bind(&id)
            .bind(&next)
            .execute(&mut *tx)
            .await?;
        if next.is_none() && row.get::<String, _>("state") != "resolved" {
            crate::schedule_notification(
                &mut tx,
                app,
                &id,
                &service,
                &environment,
                row.get("severity"),
                false,
                false,
            )
            .await?;
        }
    }
    tx.commit().await
}

#[derive(Deserialize)]
pub struct NewSilence {
    service: String,
    environment: Option<String>,
    minutes: i64,
    reason: String,
}

pub async fn create(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<NewSilence>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let principal = crate::admin::authorize_write(&headers, &app)?;
    let service = input.service.trim();
    let environment = input
        .environment
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let reason = input.reason.trim();
    if service.is_empty()
        || service.len() > 256
        || environment.is_some_and(|v| v.len() > 256)
        || reason.is_empty()
        || reason.len() > 500
        || !(1..=MAX_MINUTES).contains(&input.minutes)
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("service, reason, and 1 to {MAX_MINUTES} minutes are required"),
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let (silence,): (Value,) = sqlx::query_as("INSERT INTO silences(id,service,environment,ends_at,reason,created_by) VALUES($1,$2,$3,now()+$4*interval '1 minute',$5,$6) RETURNING to_jsonb(silences.*)")
        .bind(&id)
        .bind(service)
        .bind(environment)
        .bind(input.minutes as f64)
        .bind(reason)
        .bind(principal.label())
        .fetch_one(&app.db)
        .await
        .map_err(db_error)?;
    Ok((StatusCode::CREATED, Json(silence)))
}

pub async fn list(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>, Failure> {
    crate::admin::authorize(&headers, &app)?;
    let rows: Vec<(Value,)> = sqlx::query_as("SELECT to_jsonb(s)||jsonb_build_object('active',s.cancelled_at IS NULL AND s.starts_at<=now() AND s.ends_at>now(),'held_incidents',(SELECT count(*) FROM incidents i WHERE i.silence_id=s.id),'held_messages',(SELECT count(*) FROM gateway_messages m WHERE m.silence_id=s.id)) FROM silences s ORDER BY (s.cancelled_at IS NULL AND s.ends_at>now()) DESC,s.created_at DESC LIMIT 100")
        .fetch_all(&app.db)
        .await
        .map_err(db_error)?;
    Ok(Json(
        json!({"items":rows.into_iter().map(|r| r.0).collect::<Vec<_>>()}),
    ))
}

pub async fn cancel(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let principal = crate::admin::authorize_write(&headers, &app)?;
    let changed = sqlx::query("UPDATE silences SET cancelled_at=now(),cancelled_by=$2 WHERE id=$1 AND cancelled_at IS NULL AND ends_at>now()")
        .bind(&id)
        .bind(principal.label())
        .execute(&app.db)
        .await
        .map_err(db_error)?
        .rows_affected();
    if changed == 0 {
        return Err((
            StatusCode::CONFLICT,
            "silence is unknown, already cancelled, or already ended".into(),
        ));
    }
    Ok(Json(json!({"id":id,"cancelled":true})))
}
