//! Exports delivery evidence as CSV, one row per incident, for audits and service reports.
use crate::{App, Failure, db_error};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, Utc};
use serde::Deserialize;
use sqlx::Row;
use std::sync::Arc;

const MAX_DAYS: i64 = 92;
const ROW_LIMIT: i64 = 10_000;

#[derive(Deserialize)]
pub struct EvidenceQuery {
    from: Option<String>,
    to: Option<String>,
    #[serde(default)]
    offset_minutes: i32,
}

pub async fn export(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(query): Query<EvidenceQuery>,
) -> Result<Response, Failure> {
    crate::admin::authorize(&headers, &app)?;
    let zone = FixedOffset::east_opt(query.offset_minutes.clamp(-14 * 60, 14 * 60) * 60)
        .ok_or((StatusCode::BAD_REQUEST, "invalid offset".to_owned()))?;
    let today = Utc::now().with_timezone(&zone).date_naive();
    let date = |value: Option<&str>| {
        value
            .map(|v| NaiveDate::parse_from_str(v, "%Y-%m-%d"))
            .transpose()
            .map_err(|_| {
                (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "dates must use YYYY-MM-DD".to_owned(),
                )
            })
    };
    let to = date(query.to.as_deref())?.unwrap_or(today);
    let from = date(query.from.as_deref())?.unwrap_or(to - Duration::days(29));
    let days = (to - from).num_days() + 1;
    if !(1..=MAX_DAYS).contains(&days) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("from must not be after to, and the range must be at most {MAX_DAYS} days"),
        ));
    }
    let start = from
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(zone).single())
        .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "invalid from".to_owned()))?;
    let end = start + Duration::days(days);
    let rows = sqlx::query("SELECT i.id,i.service,i.environment,i.incident_key,i.severity,i.state,EXISTS(SELECT 1 FROM silences s WHERE s.service=i.service AND (s.environment IS NULL OR s.environment=i.environment) AND s.starts_at<=i.created_at AND least(s.ends_at,coalesce(s.cancelled_at,'infinity'))>i.created_at) AS held,(SELECT e.payload->>'summary' FROM events e WHERE e.incident_id=i.id AND e.payload->>'recovered'='false' ORDER BY (e.payload->>'sequence')::bigint LIMIT 1) AS summary,(SELECT min((extract(epoch FROM (e.payload->>'occurred_at')::timestamptz)*1000)::bigint) FROM events e WHERE e.incident_id=i.id AND e.payload->>'recovered'='false') AS first_ms,(SELECT max((extract(epoch FROM (e.payload->>'occurred_at')::timestamptz)*1000)::bigint) FROM events e WHERE e.incident_id=i.id AND e.payload->>'recovered'='true' AND i.state='resolved') AS resolved_ms,(SELECT min((extract(epoch FROM o.sent_at)*1000)::bigint) FROM outbox o WHERE o.incident_id=i.id AND o.provider_message_ts IS NOT NULL) AS first_post_ms,(SELECT count(*) FROM outbox o WHERE o.incident_id=i.id AND o.provider_message_ts IS NOT NULL) AS posts,(SELECT count(*) FROM outbox o WHERE o.incident_id=i.id AND o.failed_at IS NOT NULL AND o.sent_at IS NULL) AS post_failures,(SELECT coalesce(jsonb_agg(DISTINCT m),'[]'::jsonb) FROM outbox o, jsonb_array_elements_text(coalesce(o.mentioned_members,'[]'::jsonb)) m WHERE o.incident_id=i.id) AS mentioned,(SELECT count(*) FROM channel_deliveries c WHERE c.incident_id=i.id AND c.channel='email' AND c.state='sent') AS emails,(SELECT count(*) FROM channel_deliveries c WHERE c.incident_id=i.id AND c.channel='telegram' AND c.state='sent') AS telegrams,(SELECT count(*) FROM channel_deliveries c WHERE c.incident_id=i.id AND c.state='failed') AS channel_failures,(SELECT coalesce(c.actor_name,c.actor) FROM confirmations c WHERE c.incident_id=i.id ORDER BY c.created_at LIMIT 1) AS confirmer,(SELECT min((extract(epoch FROM c.created_at)*1000)::bigint) FROM confirmations c WHERE c.incident_id=i.id) AS confirmed_ms,(SELECT a.actor FROM actions a WHERE a.incident_id=i.id ORDER BY a.created_at LIMIT 1) AS owner,(SELECT min((extract(epoch FROM a.created_at)*1000)::bigint) FROM actions a WHERE a.incident_id=i.id) AS assigned_ms FROM incidents i WHERE i.created_at>=to_timestamp($1/1000.0) AND i.created_at<to_timestamp($2/1000.0) ORDER BY i.created_at LIMIT $3")
        .bind(start.timestamp_millis())
        .bind(end.timestamp_millis())
        .bind(ROW_LIMIT)
        .fetch_all(&app.db)
        .await
        .map_err(db_error)?;
    let label = if zone.local_minus_utc() == 0 {
        "UTC".to_owned()
    } else {
        format!("UTC{zone}")
    };
    let time = |value: Option<i64>| {
        value
            .and_then(DateTime::from_timestamp_millis)
            .map(|t| {
                t.with_timezone(&zone)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default()
    };
    let person = |id: Option<String>| {
        id.map(|id| {
            app.config
                .users
                .iter()
                .find(|(_, slack)| **slack == id)
                .map_or(id, |(name, _)| name.clone())
        })
        .unwrap_or_default()
    };
    // Excel opens UTF-8 CSV correctly only with a byte order mark.
    let mut out = String::from("\u{feff}");
    let language = app.config.language;
    let w = |ko: &'static str, en: &'static str| language.pick(ko, en);
    let header = [
        w("사건 ID", "Incident ID").replace("{label}", &label),
        w("서비스", "Service").replace("{label}", &label),
        w("환경", "Environment").replace("{label}", &label),
        w("사건 키", "Incident key").replace("{label}", &label),
        w("요약", "Summary").replace("{label}", &label),
        w("우선순위", "Priority").replace("{label}", &label),
        w("상태", "State").replace("{label}", &label),
        w("점검 무음 중 발생", "Started during a silence").replace("{label}", &label),
        w("처음 발생 ({label})", "First event ({label})").replace("{label}", &label),
        w("첫 Slack 게시 ({label})", "First Slack post ({label})").replace("{label}", &label),
        w("Slack 게시 수", "Slack posts").replace("{label}", &label),
        w("Slack 실패 수", "Slack failures").replace("{label}", &label),
        w("멘션 대상", "Mentioned users").replace("{label}", &label),
        w(
            "메일 접수 수(수신 확인 아님)",
            "Emails accepted (not proof of receipt)",
        )
        .replace("{label}", &label),
        w(
            "Telegram 접수 수(읽음 확인 아님)",
            "Telegram messages accepted (not proof of reading)",
        )
        .replace("{label}", &label),
        w("메일·Telegram 실패 수", "Email and Telegram failures").replace("{label}", &label),
        w("첫 확인 ({label})", "First confirmation ({label})").replace("{label}", &label),
        w("첫 확인자", "First confirmed by").replace("{label}", &label),
        w("담당 지정 ({label})", "Owner assigned ({label})").replace("{label}", &label),
        w("담당자", "Owner").replace("{label}", &label),
        w("해결 ({label})", "Resolved ({label})").replace("{label}", &label),
    ];
    push_row(&mut out, &header);
    for row in rows {
        let mentioned: Vec<String> = row
            .get::<serde_json::Value, _>("mentioned")
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .map(|id| person(Some(id.to_owned())))
            .collect();
        let state = match row.get::<String, _>("state").as_str() {
            "resolved" => w("해결됨", "resolved"),
            "acknowledged" => w("담당 중", "acknowledged"),
            _ => w("담당 대기", "waiting for an owner"),
        };
        push_row(
            &mut out,
            &[
                row.get("id"),
                row.get("service"),
                row.get("environment"),
                row.get("incident_key"),
                row.get::<Option<String>, _>("summary").unwrap_or_default(),
                format!("P{}", row.get::<i16, _>("severity")),
                state.to_owned(),
                if row.get("held") {
                    w("예", "yes")
                } else {
                    w("아니요", "no")
                }
                .to_owned(),
                time(row.get("first_ms")),
                time(row.get("first_post_ms")),
                row.get::<i64, _>("posts").to_string(),
                row.get::<i64, _>("post_failures").to_string(),
                mentioned.join(" "),
                row.get::<i64, _>("emails").to_string(),
                row.get::<i64, _>("telegrams").to_string(),
                row.get::<i64, _>("channel_failures").to_string(),
                time(row.get("confirmed_ms")),
                person(row.get("confirmer")),
                time(row.get("assigned_ms")),
                person(row.get("owner")),
                time(row.get("resolved_ms")),
            ],
        );
    }
    let filename = format!("noti-mesh-evidence-{from}-{to}.csv");
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        out,
    )
        .into_response())
}

fn push_row(out: &mut String, fields: &[String]) {
    let cells: Vec<String> = fields.iter().map(|field| cell(field)).collect();
    out.push_str(&cells.join(","));
    out.push_str("\r\n");
}

/// Quotes a CSV cell and keeps spreadsheet apps from running sender text as a formula.
fn cell(value: &str) -> String {
    let value = if value.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{value}")
    } else {
        value.to_owned()
    };
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::cell;

    #[test]
    fn cells_are_quoted_and_formulas_are_neutralized() {
        assert_eq!(cell("plain"), "plain");
        assert_eq!(cell("a,b"), "\"a,b\"");
        assert_eq!(cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(cell("line\nbreak"), "\"line\nbreak\"");
        assert_eq!(cell("=1+1"), "'=1+1");
        assert_eq!(cell("-5"), "'-5");
        assert_eq!(cell("@SUM(A1)"), "'@SUM(A1)");
    }
}
