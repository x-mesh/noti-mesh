use crate::App;
use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SLACK_CHECK_INTERVAL: Duration = Duration::from_secs(300);
// Slack closes Socket Mode connections for routine refreshes and the worker reconnects
// within seconds, so only a longer gap means button actions are being lost.
const SOCKET_GRACE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct DeliveryHealth {
    slack: Mutex<SlackCheck>,
    socket_disconnected_since: Mutex<Option<Instant>>,
}

#[derive(Clone, Default)]
pub struct SlackCheck {
    pub checked: bool,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
}

impl DeliveryHealth {
    pub fn socket_connected(&self) {
        *self.socket_disconnected_since.lock().unwrap() = None;
    }
    pub fn socket_disconnected_since(&self, since: Instant) {
        self.socket_disconnected_since
            .lock()
            .unwrap()
            .get_or_insert(since);
    }
    fn snapshot(&self) -> (SlackCheck, Option<Instant>) {
        (
            self.slack.lock().unwrap().clone(),
            *self.socket_disconnected_since.lock().unwrap(),
        )
    }
}

pub async fn workers_alive(db: &sqlx::PgPool) -> bool {
    let result:Result<(i64,),sqlx::Error>=sqlx::query_as("SELECT count(*) FROM worker_heartbeats WHERE worker IN ('incidents','messages','channels') AND last_tick>now()-interval '60 seconds'").fetch_one(db).await;
    matches!(result, Ok((3,)))
}

pub async fn slack_worker(app: Arc<App>) {
    loop {
        let check = check_slack(&app).await;
        for problem in &check.problems {
            eprintln!("Slack check: {problem}");
        }
        *app.delivery_health.slack.lock().unwrap() = check;
        tokio::time::sleep(SLACK_CHECK_INTERVAL).await;
    }
}

pub async fn check_slack(app: &App) -> SlackCheck {
    let mut check = SlackCheck {
        checked: true,
        ..SlackCheck::default()
    };
    match call(app, "auth.test", &[]).await {
        Err(problem) => {
            check
                .problems
                .push(format!("Slack auth.test failed: {problem}"));
            return check;
        }
        Ok(body) if body["team_id"] != app.config.workspace.as_str() => {
            check.problems.push(format!(
                "Slack token belongs to workspace {}, not the configured workspace",
                body["team_id"].as_str().unwrap_or("unknown")
            ));
            return check;
        }
        Ok(_) => {}
    }
    let channels: BTreeSet<&str> = app
        .config
        .groups
        .values()
        .filter(|group| group.slack_enabled)
        .map(|group| group.channel.as_str())
        .collect();
    for channel in channels {
        match call(app, "conversations.info", &[("channel", channel)]).await {
            Err(problem) if problem == "missing_scope" => check.warnings.push(format!(
                "cannot verify channel {channel}; grant channels:read or groups:read"
            )),
            Err(problem) => check
                .problems
                .push(format!("channel {channel} check failed: {problem}")),
            Ok(body) if body["channel"]["is_archived"] == true => check
                .problems
                .push(format!("channel {channel} is archived")),
            Ok(body) if body["channel"]["is_member"] == false => check
                .problems
                .push(format!("bot is not a member of channel {channel}")),
            Ok(_) => {}
        }
    }
    check
}

async fn call(app: &App, method: &str, query: &[(&str, &str)]) -> Result<Value, String> {
    let response = app
        .http
        .get(format!("{}/{method}", app.config.api_url))
        .bearer_auth(&app.slack_token)
        .query(query)
        .send()
        .await
        .map_err(|_| "Slack request failed".to_owned())?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|_| format!("invalid Slack response (HTTP {})", status.as_u16()))?;
    if body["ok"] == true {
        return Ok(body);
    }
    Err(body["error"]
        .as_str()
        .filter(|code| {
            code.len() <= 80 && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .unwrap_or("unknown_error")
        .to_owned())
}

pub async fn delivery(State(app): State<Arc<App>>) -> (StatusCode, Json<Value>) {
    let mut problems = Vec::new();
    let mut warnings = Vec::new();
    if !workers_alive(&app.db).await {
        problems.push("database or delivery workers are not responding".to_owned());
    }
    let (slack, socket) = app.delivery_health.snapshot();
    problems.extend(slack.problems);
    warnings.extend(slack.warnings);
    if app.slack_check_enabled && !slack.checked {
        warnings.push("Slack check has not completed yet".to_owned());
    }
    if socket.is_some_and(|since| since.elapsed() > SOCKET_GRACE) {
        problems.push("Slack Socket Mode is disconnected; button actions are not received".into());
    }
    // A job that failed three times or waits five minutes past its due time indicates a
    // provider outage even while each attempt is still classified as transient.
    let counts:Result<(i64,i64),sqlx::Error>=sqlx::query_as("WITH jobs AS (SELECT (failed_at IS NOT NULL AND failed_at>now()-interval '24 hours') AS failed,(failed_at IS NULL AND (attempts>=3 OR due_at<now()-interval '5 minutes')) AS stuck FROM outbox WHERE sent_at IS NULL AND NOT slack_disabled UNION ALL SELECT (failed_at IS NOT NULL AND failed_at>now()-interval '24 hours'),(failed_at IS NULL AND (attempts>=3 OR due_at<now()-interval '5 minutes')) FROM gateway_messages WHERE sent_at IS NULL AND NOT slack_disabled UNION ALL SELECT (state='failed' AND created_at>now()-interval '24 hours'),(state='pending' AND (attempts>=3 OR due_at<now()-interval '5 minutes')) FROM channel_deliveries WHERE state IN ('pending','failed')) SELECT count(*) FILTER(WHERE failed),count(*) FILTER(WHERE stuck) FROM jobs").fetch_one(&app.db).await;
    match counts {
        Ok((failed, stuck)) => {
            if failed > 0 {
                problems.push(format!(
                    "{failed} deliveries failed permanently in the last 24 hours"
                ));
            }
            if stuck > 0 {
                problems.push(format!("{stuck} deliveries are delayed or retrying"));
            }
        }
        Err(_) => problems.push("delivery state query failed".to_owned()),
    }
    let status = if problems.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({"healthy":problems.is_empty(),"problems":problems,"warnings":warnings})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::Query, routing::get};
    use std::collections::HashMap;

    async fn check_with(team: &'static str) -> SlackCheck {
        let mock = Router::new()
            .route(
                "/auth.test",
                get(move || async move { Json(json!({"ok":true,"team_id":team})) }),
            )
            .route(
                "/conversations.info",
                get(|Query(query): Query<HashMap<String, String>>| async move {
                    Json(match query["channel"].as_str() {
                        "CMEMBER" => {
                            json!({"ok":true,"channel":{"is_member":true,"is_archived":false}})
                        }
                        "CARCHIVED" => {
                            json!({"ok":true,"channel":{"is_member":true,"is_archived":true}})
                        }
                        "COUTSIDE" => {
                            json!({"ok":true,"channel":{"is_member":false,"is_archived":false}})
                        }
                        "CPRIVATE" => json!({"ok":false,"error":"missing_scope"}),
                        _ => json!({"ok":false,"error":"channel_not_found"}),
                    })
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config: crate::policy::Config =
            toml::from_str(include_str!("../config.example.toml")).unwrap();
        config.api_url = format!("http://{}", listener.local_addr().unwrap());
        config.workspace = "TOWN".into();
        let channels = ["CMEMBER", "CARCHIVED", "COUTSIDE", "CPRIVATE", "CMISSING"];
        let template = config.groups["operations"].clone();
        config.groups.clear();
        for channel in channels {
            let mut group = template.clone();
            group.channel = channel.into();
            config.groups.insert(channel.into(), group);
        }
        let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let db = sqlx::PgPool::connect_lazy("postgresql://unused@127.0.0.1/unused").unwrap();
        let check = check_slack(&App::for_test(db, config)).await;
        server.abort();
        check
    }

    #[tokio::test]
    async fn slack_check_reports_channel_problems() {
        let check = check_with("TOWN").await;
        assert_eq!(
            check.problems,
            [
                "channel CARCHIVED is archived",
                "channel CMISSING check failed: channel_not_found",
                "bot is not a member of channel COUTSIDE",
            ]
        );
        assert_eq!(
            check.warnings,
            ["cannot verify channel CPRIVATE; grant channels:read or groups:read"]
        );
    }

    #[tokio::test]
    async fn slack_check_rejects_another_workspace() {
        let check = check_with("TOTHER").await;
        assert_eq!(
            check.problems,
            ["Slack token belongs to workspace TOTHER, not the configured workspace"]
        );
    }
}
