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
    let language = app.config.language;
    let w = |ko: &'static str, en: &'static str| language.pick(ko, en);
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
            "{}, P{}",
            if recovered {
                w("해결 이벤트 수신", "Recovery event received")
            } else {
                w("발생 이벤트 수신", "Event received")
            },
            payload["severity"]
        );
        if let Some(occurred) = occurred {
            line.push_str(&format!(
                ", {} {}",
                w("발생 시각", "occurred at"),
                time(occurred, zone)
            ));
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
            "update" => w("알림", "alert"),
            "reopen" => w("우선순위 상승 알림", "priority rise alert"),
            "escalation" => w("재알림", "escalation"),
            "summary" => w("요약", "summary"),
            _ => w("메시지", "message"),
        };
        let target: Option<String> = row.get("target");
        let target = target
            .map(|t| format!(" ({})", w("{t} 그룹", "group {t}").replace("{t}", &t)))
            .unwrap_or_default();
        if let Some(at) = row
            .get::<Option<i64>, _>("failed_at")
            .and_then(DateTime::from_timestamp_millis)
        {
            let error: Option<String> = row.get("last_error");
            timeline.push((
                at,
                format!(
                    "Slack {kind} {}{target}: {}",
                    w("실패", "failed"),
                    error.unwrap_or_default()
                ),
            ));
        } else if row
            .get::<Option<String>, _>("provider_message_ts")
            .is_some()
        {
            let at = at(row, "sent_at");
            first_post = Some(first_post.map_or(at, |seen| seen.min(at)));
            timeline.push((at, format!("Slack {kind} {}{target}", w("게시", "posted"))));
        }
    }
    let channels = sqlx::query("SELECT channel,state,(extract(epoch FROM sent_at)*1000)::bigint AS sent_at,(extract(epoch FROM due_at)*1000)::bigint AS due_at,last_error FROM channel_deliveries WHERE incident_id=$1 AND state IN ('sent','failed')")
        .bind(&id)
        .fetch_all(&app.db)
        .await
        .map_err(db_error)?;
    for row in &channels {
        let channel = match row.get::<String, _>("channel").as_str() {
            "email" => w("메일", "Email"),
            _ => "Telegram",
        };
        if row.get::<String, _>("state") == "sent" {
            // Provider acceptance does not prove that a person read it.
            timeline.push((
                at(row, "sent_at"),
                format!("{channel} {}", w("발송 접수", "accepted by the provider")),
            ));
        } else {
            let error: Option<String> = row.get("last_error");
            // Failed deliveries keep no failure time; the last scheduled attempt is the closest record.
            timeline.push((
                at(row, "due_at"),
                format!(
                    "{channel} {}: {}",
                    w(
                        "발송 실패(마지막 시도 예정 시각)",
                        "delivery failed (last scheduled attempt)"
                    ),
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
            .map(|who| format!(" ({} {who})", w("지정:", "assigned by")))
            .unwrap_or_default();
        timeline.push((
            at,
            format!("{} {actor}{by}", w("담당 지정:", "Owner assigned:")),
        ));
    }
    let confirmations = sqlx::query("SELECT coalesce(actor_name,actor) AS who,(extract(epoch FROM created_at)*1000)::bigint AS created_at FROM confirmations WHERE incident_id=$1")
        .bind(&id)
        .fetch_all(&app.db)
        .await
        .map_err(db_error)?;
    for row in &confirmations {
        timeline.push((
            at(row, "created_at"),
            format!(
                "{} {}",
                w("확인:", "Confirmed by"),
                person(&app, row.get("who"))
            ),
        ));
    }
    timeline.sort_by_key(|(at, _)| *at);

    let key: String = incident.get("incident_key");
    let title = summary.clone().unwrap_or_else(|| key.clone());
    let since = |at: DateTime<Utc>| {
        first.map_or(String::new(), |start| {
            format!(
                ", {}",
                w("발생 후 {d}", "{d} after the first event")
                    .replace("{d}", &duration(language, at - start))
            )
        })
    };
    let state = match incident.get::<String, _>("state").as_str() {
        "resolved" => w("해결됨", "resolved"),
        "acknowledged" => w("담당 중", "acknowledged"),
        _ => w("담당 대기", "waiting for an owner"),
    };
    let mut out = format!(
        "# {} {title}\n\n",
        w("사건 회고 초안:", "Post-incident draft:")
    );
    out.push_str(&w(
        "noti-mesh가 기록으로 만든 초안입니다. 원인, 영향, 후속 조치는 직접 작성하세요. 시각은 {zone} 기준입니다. 메일과 Telegram은 발송 접수까지만 기록되며, 수신자가 읽었는지는 알 수 없습니다.\n\n## 요약\n\n",
        "noti-mesh made this draft from its records. Write the cause, the impact, and the follow-up actions yourself. Times are in {zone}. Email and Telegram records show provider acceptance only. They do not show whether a person read the message.\n\n## Summary\n\n",
    ).replace("{zone}", &zone_label(zone)));
    out.push_str(&format!(
        "- {}: {} / {}\n- {}: `{key}`\n- {}: P{}\n- {}: {state}\n- {}: {}\n",
        w("서비스", "Service"),
        incident.get::<String, _>("service"),
        incident.get::<String, _>("environment"),
        w("사건 키", "Incident key"),
        w("우선순위", "Priority"),
        incident.get::<i16, _>("severity"),
        w("상태", "State"),
        w("이벤트", "Events"),
        incident.get::<i64, _>("count"),
    ));
    if let Some(start) = first {
        out.push_str(&format!(
            "- {}: {}\n",
            w("처음 발생", "First event"),
            time(start, zone)
        ));
    }
    let first_alert = w("첫 Slack 알림", "First Slack alert");
    match first_post {
        Some(at) => out.push_str(&format!(
            "- {first_alert}: {}{}\n",
            time(at, zone),
            since(at)
        )),
        None => out.push_str(&format!(
            "- {first_alert}: {}\n",
            w("게시 기록 없음", "no post recorded")
        )),
    }
    let owner_label = w("담당", "Owner");
    match first_owner {
        Some((at, actor)) => out.push_str(&format!(
            "- {owner_label}: {actor}, {}{}\n",
            time(at, zone),
            since(at)
        )),
        None => out.push_str(&format!(
            "- {owner_label}: {}\n",
            w("지정 기록 없음", "not assigned")
        )),
    }
    if let Some(at) = resolved {
        out.push_str(&format!(
            "- {}: {}{}\n",
            w("해결", "Resolved"),
            time(at, zone),
            since(at)
        ));
    }
    if let Some(url) = source_url {
        out.push_str(&format!("- {}: {url}\n", w("원본", "Source")));
    }
    out.push_str(&format!("\n## {}\n\n", w("타임라인", "Timeline")));
    for (at, line) in &timeline {
        out.push_str(&format!("- {} {line}\n", time(*at, zone)));
    }
    let todo = w("(작성 필요)", "(to be written)");
    for heading in [
        w("원인", "Cause"),
        w("영향", "Impact"),
        w("후속 조치", "Follow-up actions"),
    ] {
        out.push_str(&format!("\n## {heading}\n\n{todo}\n"));
    }
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

fn duration(language: crate::policy::Language, span: chrono::TimeDelta) -> String {
    let seconds = span.num_seconds().max(0);
    let (h, m, s) = match language {
        crate::policy::Language::Ko => ("시간", "분", "초"),
        crate::policy::Language::En => (" h", " min", " s"),
    };
    match (seconds / 3600, seconds % 3600 / 60) {
        (0, 0) => format!("{seconds}{s}"),
        (0, minutes) => format!("{minutes}{m}"),
        (hours, 0) => format!("{hours}{h}"),
        (hours, minutes) => format!("{hours}{h} {minutes}{m}"),
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
        let ko = |s| duration(crate::policy::Language::Ko, span(s));
        let en = |s| duration(crate::policy::Language::En, span(s));
        assert_eq!(ko(42), "42초");
        assert_eq!(ko(30 * 60), "30분");
        assert_eq!(ko(2 * 3600), "2시간");
        assert_eq!(ko(3600 + 5 * 60 + 9), "1시간 5분");
        assert_eq!(ko(-5), "0초");
        assert_eq!(en(3600 + 5 * 60), "1 h 5 min");
    }
}
