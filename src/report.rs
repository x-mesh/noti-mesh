//! Builds a Korean post-incident draft from the records noti-mesh already keeps.
use crate::{App, Failure, db_error};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, FixedOffset, Utc};
use serde::Deserialize;
use sqlx::Row;
use std::sync::Arc;

#[derive(Deserialize)]
pub struct ReportQuery {
    /// Minutes east of UTC, so the draft shows the reader's local time.
    #[serde(default)]
    offset_minutes: i32,
}

pub async fn incident(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<ReportQuery>,
) -> Result<Response, Failure> {
    crate::admin::authorize(&headers, &app)?;
    let zone = FixedOffset::east_opt(query.offset_minutes.clamp(-14 * 60, 14 * 60) * 60)
        .ok_or((StatusCode::BAD_REQUEST, "invalid offset".to_owned()))?;
    let incident = sqlx::query("SELECT service,environment,incident_key,severity,state,owner,count FROM incidents WHERE id=$1")
        .bind(&id)
        .fetch_optional(&app.db)
        .await
        .map_err(db_error)?
        .ok_or((StatusCode::NOT_FOUND, "incident not found".to_owned()))?;
    let events = sqlx::query("SELECT (extract(epoch FROM received_at)*1000)::bigint AS received_at,payload FROM events WHERE incident_id=$1 ORDER BY (payload->>'sequence')::bigint,received_at")
        .bind(&id)
        .fetch_all(&app.db)
        .await
        .map_err(db_error)?;
    let mut timeline: Vec<(DateTime<Utc>, String)> = Vec::new();
    let mut first: Option<DateTime<Utc>> = None;
    let mut resolved: Option<DateTime<Utc>> = None;
    let mut summary = None;
    let mut source_url = None;
    for row in &events {
        let payload: serde_json::Value = row.get("payload");
        let occurred = payload["occurred_at"]
            .as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.with_timezone(&Utc));
        let recovered = payload["recovered"] == true;
        if recovered {
            resolved = resolved.or(occurred);
        } else {
            first = first.or(occurred);
            summary = summary.or_else(|| payload["summary"].as_str().map(one_line));
        }
        source_url = source_url.or_else(|| payload["source_url"].as_str().map(str::to_owned));
        let mut line = format!(
            "{} 이벤트 수신, P{}",
            if recovered { "해결" } else { "발생" },
            payload["severity"]
        );
        if let Some(occurred) = occurred {
            line.push_str(&format!(", 발생 시각 {}", time(occurred, zone)));
        }
        if let Some(text) = payload["summary"].as_str() {
            line.push_str(&format!(": {}", one_line(text)));
        }
        timeline.push((at(row, "received_at"), line));
    }
    let deliveries = sqlx::query("SELECT kind,target,(extract(epoch FROM sent_at)*1000)::bigint AS sent_at,(extract(epoch FROM failed_at)*1000)::bigint AS failed_at,last_error,provider_message_ts FROM outbox WHERE incident_id=$1 AND (sent_at IS NOT NULL OR failed_at IS NOT NULL)")
        .bind(&id)
        .fetch_all(&app.db)
        .await
        .map_err(db_error)?;
    let mut first_post: Option<DateTime<Utc>> = None;
    for row in &deliveries {
        let kind = match row.get::<String, _>("kind").as_str() {
            "update" => "알림",
            "reopen" => "우선순위 상승 알림",
            "escalation" => "재알림",
            "summary" => "요약",
            _ => "메시지",
        };
        let target: Option<String> = row.get("target");
        let target = target.map(|t| format!(" ({t} 그룹)")).unwrap_or_default();
        if let Some(at) = row
            .get::<Option<i64>, _>("failed_at")
            .and_then(DateTime::from_timestamp_millis)
        {
            let error: Option<String> = row.get("last_error");
            timeline.push((
                at,
                format!("Slack {kind} 실패{target}: {}", error.unwrap_or_default()),
            ));
        } else if row
            .get::<Option<String>, _>("provider_message_ts")
            .is_some()
        {
            let at = at(row, "sent_at");
            first_post = Some(first_post.map_or(at, |seen| seen.min(at)));
            timeline.push((at, format!("Slack {kind} 게시{target}")));
        }
    }
    let channels = sqlx::query("SELECT channel,state,(extract(epoch FROM sent_at)*1000)::bigint AS sent_at,(extract(epoch FROM due_at)*1000)::bigint AS due_at,last_error FROM channel_deliveries WHERE incident_id=$1 AND state IN ('sent','failed')")
        .bind(&id)
        .fetch_all(&app.db)
        .await
        .map_err(db_error)?;
    for row in &channels {
        let channel = match row.get::<String, _>("channel").as_str() {
            "email" => "메일",
            _ => "Telegram",
        };
        if row.get::<String, _>("state") == "sent" {
            // Provider acceptance does not prove that a person read it.
            timeline.push((at(row, "sent_at"), format!("{channel} 발송 접수")));
        } else {
            let error: Option<String> = row.get("last_error");
            // Failed deliveries keep no failure time; the last scheduled attempt is the closest record.
            timeline.push((
                at(row, "due_at"),
                format!(
                    "{channel} 발송 실패(마지막 시도 예정 시각): {}",
                    error.unwrap_or_default()
                ),
            ));
        }
    }
    let actions =
        sqlx::query("SELECT actor,initiator,(extract(epoch FROM created_at)*1000)::bigint AS created_at FROM actions WHERE incident_id=$1")
            .bind(&id)
            .fetch_all(&app.db)
            .await
            .map_err(db_error)?;
    let mut first_owner: Option<(DateTime<Utc>, String)> = None;
    for row in &actions {
        let at = at(row, "created_at");
        let actor = person(&app, row.get("actor"));
        if first_owner.as_ref().is_none_or(|(seen, _)| at < *seen) {
            first_owner = Some((at, actor.clone()));
        }
        let initiator: Option<String> = row.get("initiator");
        let by = initiator
            .map(|who| person(&app, who))
            .filter(|who| *who != actor)
            .map(|who| format!(" (지정: {who})"))
            .unwrap_or_default();
        timeline.push((at, format!("담당 지정: {actor}{by}")));
    }
    let confirmations = sqlx::query("SELECT coalesce(actor_name,actor) AS who,(extract(epoch FROM created_at)*1000)::bigint AS created_at FROM confirmations WHERE incident_id=$1")
        .bind(&id)
        .fetch_all(&app.db)
        .await
        .map_err(db_error)?;
    for row in &confirmations {
        timeline.push((
            at(row, "created_at"),
            format!("확인: {}", person(&app, row.get("who"))),
        ));
    }
    timeline.sort_by_key(|(at, _)| *at);

    let key: String = incident.get("incident_key");
    let title = summary.clone().unwrap_or_else(|| key.clone());
    let since = |at: DateTime<Utc>| {
        first.map_or(String::new(), |start| {
            format!(", 발생 후 {}", duration(at - start))
        })
    };
    let state = match incident.get::<String, _>("state").as_str() {
        "resolved" => "해결됨",
        "acknowledged" => "담당 중",
        _ => "담당 대기",
    };
    let mut out = format!("# 사건 회고 초안: {title}\n\n");
    out.push_str(&format!("noti-mesh가 기록으로 만든 초안입니다. 원인, 영향, 후속 조치는 직접 작성하세요. 시각은 {} 기준입니다. 메일과 Telegram은 발송 접수까지만 기록되며, 수신자가 읽었는지는 알 수 없습니다.\n\n## 요약\n\n", zone_label(zone)));
    out.push_str(&format!(
        "- 서비스: {} / {}\n- 사건 키: `{key}`\n- 우선순위: P{}\n- 상태: {state}\n- 이벤트: {}건\n",
        incident.get::<String, _>("service"),
        incident.get::<String, _>("environment"),
        incident.get::<i16, _>("severity"),
        incident.get::<i64, _>("count"),
    ));
    if let Some(start) = first {
        out.push_str(&format!("- 처음 발생: {}\n", time(start, zone)));
    }
    match first_post {
        Some(at) => out.push_str(&format!(
            "- 첫 Slack 알림: {}{}\n",
            time(at, zone),
            since(at)
        )),
        None => out.push_str("- 첫 Slack 알림: 게시 기록 없음\n"),
    }
    match first_owner {
        Some((at, actor)) => out.push_str(&format!(
            "- 담당: {actor}, {}{}\n",
            time(at, zone),
            since(at)
        )),
        None => out.push_str("- 담당: 지정 기록 없음\n"),
    }
    if let Some(at) = resolved {
        out.push_str(&format!("- 해결: {}{}\n", time(at, zone), since(at)));
    }
    if let Some(url) = source_url {
        out.push_str(&format!("- 원본: {url}\n"));
    }
    out.push_str("\n## 타임라인\n\n");
    for (at, line) in &timeline {
        out.push_str(&format!("- {} {line}\n", time(*at, zone)));
    }
    out.push_str(
        "\n## 원인\n\n(작성 필요)\n\n## 영향\n\n(작성 필요)\n\n## 후속 조치\n\n(작성 필요)\n",
    );
    Ok((
        [
            (header::CONTENT_TYPE, "text/markdown; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        out,
    )
        .into_response())
}

fn at(row: &sqlx::postgres::PgRow, column: &str) -> DateTime<Utc> {
    row.get::<Option<i64>, _>(column)
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or_default()
}

fn time(at: DateTime<Utc>, zone: FixedOffset) -> String {
    at.with_timezone(&zone)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn zone_label(zone: FixedOffset) -> String {
    if zone.local_minus_utc() == 0 {
        "UTC".to_owned()
    } else {
        format!("UTC{zone}")
    }
}

/// Shows a configured person by name; records keep the Slack user ID.
fn person(app: &App, id: String) -> String {
    app.config
        .users
        .iter()
        .find(|(_, slack_id)| **slack_id == id)
        .map_or(id, |(name, _)| name.clone())
}

fn duration(span: chrono::TimeDelta) -> String {
    let seconds = span.num_seconds().max(0);
    match (seconds / 3600, seconds % 3600 / 60) {
        (0, 0) => format!("{seconds}초"),
        (0, minutes) => format!("{minutes}분"),
        (hours, 0) => format!("{hours}시간"),
        (hours, minutes) => format!("{hours}시간 {minutes}분"),
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        let span = chrono::TimeDelta::seconds;
        assert_eq!(duration(span(42)), "42초");
        assert_eq!(duration(span(30 * 60)), "30분");
        assert_eq!(duration(span(2 * 3600)), "2시간");
        assert_eq!(duration(span(3600 + 5 * 60 + 9)), "1시간 5분");
        assert_eq!(duration(span(-5)), "0초");
    }
}
