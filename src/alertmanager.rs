//! Accepts Alertmanager and Grafana webhook payloads as incident events; both send the same alert fields.
use crate::{App, Event, Failure, accept_event, authenticate_client, db_error};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

const SUMMARY_LIMIT: usize = 1000;
const KEY_NAME_LIMIT: usize = 128;

#[derive(Deserialize)]
pub struct HookQuery {
    environment: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

pub async fn ingest(
    State(app): State<Arc<App>>,
    Query(query): Query<HookQuery>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let (source, service) = client_service(&app, &headers).await?;
    let environment = query.environment.unwrap_or_else(|| "production".into());
    let Some(alerts) = payload["alerts"].as_array() else {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "missing alerts".into()));
    };
    let group_key = payload["groupKey"].as_str();
    let mut incidents = Vec::new();
    let mut rejected = Vec::new();
    for alert in alerts {
        let fingerprint = fingerprint(alert);
        let result = match to_event(alert, &fingerprint, group_key, &service, &environment) {
            Ok(event) => accept_event(&app, source.clone(), event, query.dry_run).await,
            Err(reason) => Err((StatusCode::UNPROCESSABLE_ENTITY, reason)),
        };
        match result {
            Ok((_, Json(body))) if query.dry_run => incidents.push(body),
            Ok((_, Json(body))) => incidents.push(body["incident_id"].clone()),
            // Alertmanager repeats firing alerts with refreshed annotations under the same event ID.
            Err((StatusCode::CONFLICT, reason)) if reason == "event ID content conflict" => {}
            Err((StatusCode::UNPROCESSABLE_ENTITY | StatusCode::CONFLICT, reason)) => {
                rejected.push(json!({"fingerprint":fingerprint,"error":reason}))
            }
            // A server error makes Alertmanager retry; accepted alerts deduplicate by event ID.
            Err(failure) => return Err(failure),
        }
    }
    // Alertmanager does not retry 4xx, so a rejected alert is reported once in its log.
    let status = if rejected.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    Ok((
        status,
        Json(json!({"incidents":incidents,"rejected":rejected})),
    ))
}

/// Returns the source and service of a client token; monitoring tools do not name a service.
pub(crate) async fn client_service(
    app: &App,
    headers: &HeaderMap,
) -> Result<(String, String), Failure> {
    let source = authenticate_client(headers, app, None).await?;
    let Some(client) = source.strip_prefix("client:") else {
        // Configured source tokens are not bound to a service, and an alert carries none.
        return Err((
            StatusCode::FORBIDDEN,
            "use a client token from the admin UI".into(),
        ));
    };
    let service: String =
        sqlx::query_scalar("SELECT service FROM client_tokens WHERE id=$1 AND revoked_at IS NULL")
            .bind(client)
            .fetch_one(&app.db)
            .await
            .map_err(db_error)?;
    Ok((source.clone(), service))
}

/// Keeps a URL only if `POST /v1/events` would accept it as `source_url`.
pub(crate) fn link(url: &str) -> Option<String> {
    (url.len() <= 2048
        && reqwest::Url::parse(url).is_ok_and(|u| {
            matches!(u.scheme(), "http" | "https")
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
        }))
    .then(|| url.to_owned())
}

pub(crate) fn fingerprint(alert: &Value) -> String {
    if let Some(value) = alert["fingerprint"].as_str().filter(|v| !v.is_empty()) {
        return value.chars().take(64).collect();
    }
    use sha2::{Digest, Sha256};
    // serde_json keeps object keys sorted, so equal label sets hash equally.
    hex::encode(Sha256::digest(alert["labels"].to_string().as_bytes()))[..16].to_owned()
}

pub(crate) fn to_event(
    alert: &Value,
    fingerprint: &str,
    group_key: Option<&str>,
    service: &str,
    default_environment: &str,
) -> Result<Event, String> {
    let labels = &alert["labels"];
    let annotations = &alert["annotations"];
    let started = time(&alert["startsAt"]).ok_or("missing or invalid startsAt")?;
    let resolved = alert["status"] == "resolved";
    let (occurred_at, sequence) = if resolved {
        let ended = time(&alert["endsAt"]).ok_or("missing or invalid endsAt")?;
        let sequence = ended.timestamp_millis().max(started.timestamp_millis() + 1);
        (ended.to_rfc3339(), sequence)
    } else {
        (started.to_rfc3339(), started.timestamp_millis())
    };
    let name = labels["alertname"].as_str().unwrap_or("alert");
    let name: String = name.chars().take(KEY_NAME_LIMIT).collect();
    let environment = labels["environment"]
        .as_str()
        .or_else(|| labels["env"].as_str())
        .unwrap_or(default_environment);
    let summary = ["summary", "description", "message"]
        .iter()
        .find_map(|key| annotations[key].as_str().filter(|v| !v.trim().is_empty()))
        .map(|text| clip(text.trim(), SUMMARY_LIMIT))
        .unwrap_or_else(|| name.clone());
    // Grafana adds panel and dashboard links, which show the data better than the rule page.
    let source_url = ["panelURL", "dashboardURL", "generatorURL"]
        .iter()
        .filter_map(|key| alert[key].as_str())
        .find_map(link);
    let error_code: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '.'
            }
        })
        .take(64)
        .collect();
    Ok(Event {
        event_id: format!(
            "am-{fingerprint}-{}-{}",
            started.timestamp_millis(),
            if resolved { "resolved" } else { "firing" }
        ),
        service: service.to_owned(),
        environment: environment.to_owned(),
        incident_key: format!("{name}/{fingerprint}/{}", started.timestamp_millis()),
        sequence,
        severity: severity(labels["severity"].as_str().unwrap_or("")),
        recovered: resolved,
        occurred_at,
        correlation_id: group_key.map(|key| clip(key, 256)),
        error_code: Some(error_code),
        summary: Some(summary),
        source_url,
    })
}

fn time(value: &Value) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value.as_str()?).ok()?;
    // Alertmanager and Grafana send year 1 for an unset time.
    (parsed.timestamp() > 0).then_some(parsed)
}

/// Maps common severity label values to noti-mesh severity 1 (P1) to 4 (P4).
pub(crate) fn severity(label: &str) -> i16 {
    match label.trim().to_ascii_lowercase().as_str() {
        "critical" | "crit" | "emergency" | "page" | "fatal" | "p1" | "1" => 1,
        "error" | "high" | "major" | "p2" | "2" => 2,
        "info" | "informational" | "low" | "none" | "p4" | "4" => 4,
        // An unknown or missing severity is treated as a warning instead of dropping the alert.
        _ => 3,
    }
}

pub(crate) fn clip(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", text[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn firing() -> Value {
        json!({
            "status":"firing",
            "labels":{"alertname":"HighLatency","severity":"critical","instance":"api-1"},
            "annotations":{"summary":"p99 latency above 2s","description":"longer text"},
            "startsAt":"2026-11-08T01:00:00.123456789Z",
            "endsAt":"0001-01-01T00:00:00Z",
            "generatorURL":"http://prometheus:9090/graph?g0.expr=up",
            "fingerprint":"a1b2c3"
        })
    }

    #[test]
    fn firing_alert_maps_to_an_open_incident_event() {
        let event = to_event(
            &firing(),
            "a1b2c3",
            Some("{}:{alertname=\"HighLatency\"}"),
            "api",
            "production",
        )
        .unwrap();
        assert_eq!(event.incident_key, "HighLatency/a1b2c3/1794099600123");
        assert_eq!(event.event_id, "am-a1b2c3-1794099600123-firing");
        assert_eq!(event.sequence, 1794099600123);
        assert_eq!(event.severity, 1);
        assert!(!event.recovered);
        assert_eq!(event.summary.as_deref(), Some("p99 latency above 2s"));
        assert_eq!(event.error_code.as_deref(), Some("HighLatency"));
        assert_eq!(event.environment, "production");
        assert_eq!(
            event.correlation_id.as_deref(),
            Some("{}:{alertname=\"HighLatency\"}")
        );
        assert_eq!(
            event.source_url.as_deref(),
            Some("http://prometheus:9090/graph?g0.expr=up")
        );
    }

    #[test]
    fn resolved_alert_keeps_the_key_and_orders_after_firing() {
        let mut alert = firing();
        alert["status"] = json!("resolved");
        alert["endsAt"] = json!("2026-11-08T01:10:00Z");
        let event = to_event(&alert, "a1b2c3", None, "api", "production").unwrap();
        assert_eq!(event.incident_key, "HighLatency/a1b2c3/1794099600123");
        assert_eq!(event.event_id, "am-a1b2c3-1794099600123-resolved");
        assert!(event.recovered);
        assert!(event.sequence > 1794099600123);
        assert_eq!(event.occurred_at, "2026-11-08T01:10:00+00:00");
    }

    #[test]
    fn resolved_alert_without_end_time_is_rejected() {
        let mut alert = firing();
        alert["status"] = json!("resolved");
        assert!(to_event(&alert, "f", None, "api", "production").is_err());
        assert!(to_event(&json!({"status":"firing"}), "f", None, "api", "production").is_err());
    }

    #[test]
    fn labels_choose_the_environment_and_unusual_names_are_cleaned() {
        let mut alert = firing();
        alert["labels"] = json!({"alertname":"node:disk_full","env":"staging"});
        alert["annotations"] = json!({});
        alert["generatorURL"] = json!("ftp://example");
        let event = to_event(&alert, "f", None, "api", "production").unwrap();
        assert_eq!(event.environment, "staging");
        assert_eq!(event.error_code.as_deref(), Some("node.disk_full"));
        assert_eq!(event.summary.as_deref(), Some("node:disk_full"));
        assert_eq!(event.severity, 3);
        assert_eq!(event.source_url, None);
    }

    #[test]
    fn grafana_alert_prefers_the_panel_link() {
        let alert = json!({
            "status":"firing",
            "labels":{"alertname":"CPU high","grafana_folder":"infra","team":"ops"},
            "annotations":{"description":"cpu 93%"},
            "startsAt":"2026-11-08T01:00:00+09:00",
            "endsAt":"0001-01-01T00:00:00Z",
            "generatorURL":"https://grafana.example/alerting/grafana/abc/view",
            "fingerprint":"57c6d9296de2ad39",
            "silenceURL":"https://grafana.example/alerting/silence/new",
            "dashboardURL":"https://grafana.example/d/dash",
            "panelURL":"https://grafana.example/d/dash?viewPanel=2",
            "values":{"B":93.1}
        });
        let event = to_event(
            &alert,
            "57c6d9296de2ad39",
            Some("{}/{}:{}"),
            "api",
            "production",
        )
        .unwrap();
        assert_eq!(
            event.source_url.as_deref(),
            Some("https://grafana.example/d/dash?viewPanel=2")
        );
        assert_eq!(event.summary.as_deref(), Some("cpu 93%"));
        assert_eq!(event.error_code.as_deref(), Some("CPU.high"));
        let mut without_panel = alert.clone();
        without_panel["panelURL"] = json!("");
        without_panel["dashboardURL"] = json!("");
        let event = to_event(&without_panel, "f", None, "api", "production").unwrap();
        assert_eq!(
            event.source_url.as_deref(),
            Some("https://grafana.example/alerting/grafana/abc/view")
        );
    }

    #[test]
    fn severity_labels_map_to_priorities() {
        for (label, expected) in [
            ("critical", 1),
            ("PAGE", 1),
            ("error", 2),
            ("high", 2),
            ("warning", 3),
            ("", 3),
            ("info", 4),
            ("low", 4),
        ] {
            assert_eq!(severity(label), expected, "{label}");
        }
    }

    #[test]
    fn missing_fingerprint_hashes_the_labels() {
        let a = json!({"labels":{"b":"2","a":"1"}});
        let b = json!({"labels":{"a":"1","b":"2"}});
        assert_eq!(fingerprint(&a), fingerprint(&b));
        assert_eq!(fingerprint(&a).len(), 16);
        assert_ne!(fingerprint(&a), fingerprint(&json!({"labels":{"a":"2"}})));
    }
}
