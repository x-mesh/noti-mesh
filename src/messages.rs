use crate::{App, Failure, authenticate_client, db_error};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{sync::Arc, time::Duration};
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub(crate) event_id: String,
    pub(crate) service: String,
    pub(crate) environment: String,
    pub(crate) title: String,
    pub(crate) body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) notify_users: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) level: Option<Level>,
}
#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Crit,
    Error,
    Warn,
    Info,
}
impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Crit => "crit",
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
        }
    }
}
pub async fn ingest(
    State(app): State<Arc<App>>,
    Query(preview): Query<crate::Preview>,
    headers: HeaderMap,
    Json(message): Json<Message>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let source = authenticate_client(&headers, &app, Some(&message.service)).await?;
    accept(&app, source, message, None, preview.dry_run).await
}
/// Stores an authenticated message; `original` keeps a foreign payload that was converted into it.
/// With `dry_run`, returns the validated message and its route without storing or sending it.
pub(crate) async fn accept(
    app: &App,
    source: String,
    message: Message,
    original: Option<Value>,
    dry_run: bool,
) -> Result<(StatusCode, Json<Value>), Failure> {
    if !dry_run
        && std::env::var("NOTI_DEV_SLACK_LIVE").as_deref() == Ok("1")
        && app.config.groups["development"].channel == "set-by-environment"
    {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "select a Slack channel and restart".into(),
        ));
    }
    if [
        (&message.event_id, 256),
        (&message.service, 256),
        (&message.environment, 256),
        (&message.title, 200),
        (&message.body, 2000),
    ]
    .iter()
    .any(|(s, max)| s.trim().is_empty() || s.len() > *max)
    {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid message".into()));
    }
    let payload = serde_json::to_value(&message)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid message".into()))?;
    let (group, _, _) = app.config.route(&message.service, &message.environment, 4);
    if dry_run {
        return Ok((
            StatusCode::OK,
            Json(
                json!({"dry_run":true,"message":payload,"route":{"group":group,"slack_channel":app.config.groups[group].channel,"level":message.level.unwrap_or(Level::Info).as_str()}}),
            ),
        ));
    }
    let mut tx = app.db.begin().await.map_err(db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(1)")
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    if let Some(row) =
        sqlx::query("SELECT id,payload FROM gateway_messages WHERE source=$1 AND event_id=$2")
            .bind(&source)
            .bind(&message.event_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?
    {
        if row.get::<Value, _>("payload") != payload {
            return Err((StatusCode::CONFLICT, "event ID content conflict".into()));
        }
        return Ok((
            StatusCode::OK,
            Json(json!({"message_id":row.get::<String,_>("id")})),
        ));
    }
    if let Some(row) = sqlx::query(
        "SELECT message_id,payload FROM message_duplicates WHERE source=$1 AND event_id=$2",
    )
    .bind(&source)
    .bind(&message.event_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    {
        if row.get::<Value, _>("payload") != payload {
            return Err((StatusCode::CONFLICT, "event ID content conflict".into()));
        }
        return Ok((
            StatusCode::OK,
            Json(json!({"message_id":row.get::<String,_>("message_id"),"deduplicated":true})),
        ));
    }
    if let Some((id,)) = sqlx::query_as::<_, (String,)>("SELECT id FROM gateway_messages WHERE source=$1 AND created_at > now()-interval '60 seconds' AND payload - 'event_id' = $2::jsonb - 'event_id' ORDER BY created_at DESC LIMIT 1")
        .bind(&source).bind(&payload).fetch_optional(&mut *tx).await.map_err(db_error)? {
        sqlx::query("INSERT INTO message_duplicates(source,event_id,message_id,payload) VALUES($1,$2,$3,$4)")
            .bind(&source).bind(&message.event_id).bind(&id).bind(&payload).execute(&mut *tx).await.map_err(db_error)?;
        tx.commit().await.map_err(db_error)?;
        return Ok((StatusCode::OK, Json(json!({"message_id":id,"deduplicated":true}))));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let silence = crate::silence::active(&mut tx, &message.service, &message.environment)
        .await
        .map_err(db_error)?;
    // Messages during maintenance are kept for the record and never sent later.
    sqlx::query("INSERT INTO gateway_messages(id,source,event_id,payload,target,channel,level,original_payload,silence_id,slack_disabled) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9 IS NOT NULL)").bind(&id).bind(source).bind(&message.event_id).bind(payload).bind(group).bind(&app.config.groups[group].channel).bind(message.level.unwrap_or(Level::Info).as_str()).bind(original).bind(&silence).execute(&mut *tx).await.map_err(db_error)?;
    if silence.is_none() {
        crate::channels::enqueue_message(&mut tx, &app.config.groups[group], &id)
            .await
            .map_err(db_error)?;
    }
    tx.commit().await.map_err(db_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"message_id":id,"silenced":silence.is_some()})),
    ))
}
pub async fn status(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let source = authenticate_client(&headers, &app, None).await?;
    let row: Option<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(m)-'lease_token' FROM gateway_messages m WHERE id=$1 AND source=$2",
    )
    .bind(&id)
    .bind(source)
    .fetch_optional(&app.db)
    .await
    .map_err(db_error)?;
    let (message,) = row.ok_or((StatusCode::NOT_FOUND, "message not found".into()))?;
    let confirmations:Vec<(Value,)>=sqlx::query_as("SELECT to_jsonb(c) FROM message_confirmations c WHERE message_id=$1 ORDER BY created_at DESC LIMIT 100").bind(&id).fetch_all(&app.db).await.map_err(db_error)?;
    let channels:Vec<(Value,)>=sqlx::query_as("SELECT jsonb_build_object('channel',channel,'state',state,'attempts',attempts,'sent_at',sent_at,'last_error',last_error) FROM channel_deliveries WHERE message_id=$1 ORDER BY id DESC LIMIT 100").bind(&id).fetch_all(&app.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"message":message,"duplicate_count":sqlx::query_scalar::<_,i64>("SELECT count(*) FROM message_duplicates WHERE message_id=$1").bind(id).fetch_one(&app.db).await.map_err(db_error)?,"confirmations":confirmations.into_iter().map(|r|r.0).collect::<Vec<_>>(),"channels":channels.into_iter().map(|r|r.0).collect::<Vec<_>>()}),
    ))
}
pub async fn list(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>, Failure> {
    crate::admin::authorize(&headers, &app)?;
    let rows:Vec<(Value,)>=sqlx::query_as("SELECT jsonb_build_object('message',(to_jsonb(m)-'lease_token')||jsonb_build_object('source_name',coalesce((SELECT t.name FROM client_tokens t WHERE 'client:'||t.id=m.source),m.source)),'duplicate_count',(SELECT count(*) FROM message_duplicates d WHERE d.message_id=m.id),'confirmations',(SELECT coalesce(jsonb_agg(to_jsonb(c) ORDER BY c.created_at),'[]'::jsonb) FROM message_confirmations c WHERE c.message_id=m.id),'channels',(SELECT coalesce(jsonb_agg(to_jsonb(d)-'lease_token' ORDER BY d.id DESC),'[]'::jsonb) FROM channel_deliveries d WHERE d.message_id=m.id)) FROM gateway_messages m ORDER BY created_at DESC LIMIT 100").fetch_all(&app.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"items":rows.into_iter().map(|r|r.0).collect::<Vec<_>>()}),
    ))
}
pub async fn worker(app: Arc<App>) {
    loop {
        match deliver(&app).await {
            Ok(worked) => {
                if let Err(error) = crate::worker_heartbeat(&app.db, "messages").await {
                    eprintln!("messages heartbeat: {error}");
                }
                if worked {
                    continue;
                }
            }
            Err(error) => eprintln!("message delivery: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
pub(crate) async fn deliver(app: &App) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let lease = uuid::Uuid::new_v4().to_string();
    let row=sqlx::query("UPDATE gateway_messages SET lease_token=$1,lease_until=now()+interval '30 seconds' WHERE id=(SELECT m.id FROM gateway_messages m WHERE NOT m.slack_disabled AND (m.lease_until IS NULL OR m.lease_until<now()) AND ((m.sent_at IS NULL AND m.failed_at IS NULL AND m.due_at<=now()) OR (m.sent_at IS NOT NULL AND m.provider_message_ts IS NOT NULL AND m.refresh_failed_at IS NULL AND m.refresh_due_at<=now() AND (m.displayed_count < 1+(SELECT count(*) FROM message_duplicates d WHERE d.message_id=m.id) OR m.displayed_confirmers < (SELECT count(DISTINCT c.actor) FROM message_confirmations c WHERE c.message_id=m.id)))) ORDER BY (m.sent_at IS NOT NULL),m.created_at FOR UPDATE OF m SKIP LOCKED LIMIT 1) RETURNING *").bind(&lease).fetch_optional(&app.db).await?;
    let Some(row) = row else { return Ok(false) };
    let id: String = row.get("id");
    let channel: String = row.get("channel");
    let payload: Value = row.get("payload");
    let target: String = row.get("target");
    let group = app
        .config
        .groups
        .get(&target)
        .ok_or("message target group no longer exists")?;
    let count: i64 =
        sqlx::query_scalar("SELECT 1+count(*) FROM message_duplicates WHERE message_id=$1")
            .bind(&id)
            .fetch_one(&app.db)
            .await?;
    let confirmers: Vec<String> = sqlx::query_scalar(
        "SELECT actor FROM message_confirmations WHERE message_id=$1 GROUP BY actor ORDER BY min(created_at)",
    )
    .bind(&id)
    .fetch_all(&app.db)
    .await?;
    let posted = row
        .get::<Option<String>, _>("provider_message_ts")
        .is_some();
    // info is the default for most messages, so only the levels that need attention get a marker.
    let marker = match row.get::<String, _>("level").as_str() {
        "crit" => "🔴 CRIT · ",
        "error" => "🟠 ERROR · ",
        "warn" => "🟡 WARN · ",
        _ => "",
    };
    let title = payload["title"].as_str().unwrap_or("");
    let body = payload["body"].as_str().unwrap_or("");
    // A one-line Slack webhook text becomes both title and body; showing it twice adds nothing.
    let content = if body == title {
        format!("{marker}{title}\n{}", received(app.config.language, count))
    } else {
        format!(
            "{marker}{title}\n{body}\n{}",
            received(app.config.language, count)
        )
    };
    let mentioned: Vec<&String> = if payload["notify_users"] == true {
        group
            .members
            .iter()
            .map(|name| &app.config.users[name])
            .collect()
    } else {
        Vec::new()
    };
    let mentions = mentioned
        .iter()
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(" ");
    let text = if mentions.is_empty() {
        content.clone()
    } else {
        format!("{mentions} {content}")
    };
    let mut blocks = vec![json!({"type":"section","text":{"type":"plain_text","text":content}})];
    if !mentions.is_empty() {
        blocks.push(json!({"type":"section","text":{"type":"mrkdwn","text":mentions}}));
    }
    // plain_text never turns a URL into a link, so the URLs in the body get their own mrkdwn line.
    let found = links(body);
    if !found.is_empty() {
        blocks.push(
            json!({"type":"context","elements":[{"type":"mrkdwn","text":link_line(&found)}]}),
        );
    }
    blocks.push(json!({"type":"actions","elements":[{"type":"button","text":{"type":"plain_text","text":app.config.language.pick("확인","Confirm")},"action_id":"message_confirm","value":id}]}));
    // Only ID characters may enter the mention; `<`, `>` or `|` would change the mrkdwn around it.
    let confirmed = confirmers
        .iter()
        .filter(|actor| actor.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        .map(|actor| format!("<@{actor}>"))
        .collect::<Vec<_>>();
    if !confirmed.is_empty() {
        blocks.push(json!({"type":"context","elements":[{"type":"mrkdwn","text":format!("✅ {}: {}", app.config.language.pick("확인", "Confirmed by"), confirmed.join(", "))}]}));
    }
    let method = if posted {
        "chat.update"
    } else {
        "chat.postMessage"
    };
    let mut request = json!({"channel":channel,"text":text,"blocks":blocks,"parse":"none"});
    if posted {
        request["ts"] = row.get::<Option<String>, _>("provider_message_ts").into();
    }
    let result = app
        .http
        .post(format!("{}/{method}", app.config.api_url))
        .bearer_auth(&app.slack_token)
        .json(&request)
        .send()
        .await;
    let (sent, ts, delay, error, permanent) = match crate::slack::response(result, false).await {
        Ok(value) => (
            true,
            value["ts"].as_str().map(str::to_owned),
            0,
            String::new(),
            false,
        ),
        Err(error) => (
            false,
            None,
            error.retry_after,
            error.message,
            error.permanent,
        ),
    };
    if posted {
        sqlx::query("UPDATE gateway_messages SET displayed_count=CASE WHEN $3 THEN $4 ELSE displayed_count END,displayed_confirmers=CASE WHEN $3 THEN $8 ELSE displayed_confirmers END,refresh_error=NULLIF($5,''),refresh_failed_at=CASE WHEN $6 THEN now() ELSE NULL END,refresh_due_at=now()+$7*interval '1 second',refresh_attempts=CASE WHEN $3 THEN 0 ELSE refresh_attempts+1 END,lease_until=NULL,lease_token=NULL WHERE id=$1 AND lease_token=$2")
            .bind(&id).bind(&lease).bind(sent).bind(count).bind(error).bind(permanent).bind(delay as f64).bind(confirmers.len() as i64).execute(&app.db).await?;
        return Ok(true);
    }
    let mut tx = app.db.begin().await?;
    let changed=sqlx::query("UPDATE gateway_messages SET sent_at=CASE WHEN $2 THEN now() ELSE NULL END,provider_message_ts=$3,attempts=attempts+CASE WHEN $2 THEN 0 ELSE 1 END,last_error=NULLIF($4,''),due_at=now()+$5*interval '1 second',lease_until=NULL,target_members=CASE WHEN $2 THEN $7 ELSE target_members END,mentioned_members=CASE WHEN $2 THEN $8 ELSE mentioned_members END,failed_at=CASE WHEN $9 THEN now() ELSE NULL END,displayed_count=CASE WHEN $2 THEN $10 ELSE displayed_count END WHERE id=$1 AND lease_token=$6 AND sent_at IS NULL").bind(&id).bind(sent).bind(ts).bind(error).bind(delay as f64).bind(lease).bind(json!(group.members.iter().map(|name|&app.config.users[name]).collect::<Vec<_>>())).bind(json!(mentioned)).bind(permanent).bind(count).execute(&mut *tx).await?.rows_affected();
    if changed > 0 {
        crate::channels::finish_message(&mut tx, &id, sent).await?;
    }
    tx.commit().await?;
    Ok(true)
}

fn received(language: crate::policy::Language, count: i64) -> String {
    match (language, count) {
        (crate::policy::Language::Ko, _) => format!("수신 {count}회"),
        (crate::policy::Language::En, 1) => "Received 1 time".to_owned(),
        (crate::policy::Language::En, _) => format!("Received {count} times"),
    }
}

const LINK_LIMIT: usize = 5;
const LINK_LABEL_LIMIT: usize = 60;

/// Finds the http(s) URLs in free text, without trailing punctuation or an unmatched `)`.
fn links(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in text.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`'))
    {
        let Some(start) = ["http://", "https://"]
            .iter()
            .filter_map(|scheme| word.find(scheme))
            .min()
        else {
            continue;
        };
        let mut candidate = &word[start..];
        loop {
            let trimmed = candidate.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            let trimmed = if trimmed.ends_with(')')
                && trimmed.matches(')').count() > trimmed.matches('(').count()
            {
                &trimmed[..trimmed.len() - 1]
            } else {
                trimmed
            };
            if trimmed == candidate {
                break;
            }
            candidate = trimmed;
        }
        let valid = reqwest::Url::parse(candidate)
            .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
        if valid && !found.iter().any(|seen| seen == candidate) {
            found.push(candidate.to_owned());
            if found.len() == LINK_LIMIT {
                break;
            }
        }
    }
    found
}

fn link_line(links: &[String]) -> String {
    let escape = |text: &str| {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('|', "%7C")
    };
    links
        .iter()
        .map(|link| {
            let bare = link
                .split_once("://")
                .map_or(link.as_str(), |(_, rest)| rest);
            let label: String = if bare.chars().count() > LINK_LABEL_LIMIT {
                format!(
                    "{}…",
                    bare.chars().take(LINK_LABEL_LIMIT - 1).collect::<String>()
                )
            } else {
                bare.to_owned()
            };
            format!("<{}|{}>", escape(link), escape(&label))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_found_in_free_text() {
        assert_eq!(
            links("see run 1 (https://ci.example/1). Next: http://x.example/a?b=1&c=2, done"),
            ["https://ci.example/1", "http://x.example/a?b=1&c=2"]
        );
        assert_eq!(
            links(
                "wiki https://en.wikipedia.org/wiki/Foo_(bar) and https://ci.example/1 again https://ci.example/1"
            ),
            [
                "https://en.wikipedia.org/wiki/Foo_(bar)",
                "https://ci.example/1"
            ]
        );
        assert!(links("ftp://x.example javascript:alert(1) https:// mailto:a@b").is_empty());
        let many = (0..8)
            .map(|n| format!("https://h{n}.example/"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(links(&many).len(), LINK_LIMIT);
    }

    #[test]
    fn link_line_escapes_mrkdwn() {
        assert_eq!(
            link_line(&["https://x.example/a?b=1&c=<2>|3".into()]),
            "<https://x.example/a?b=1&amp;c=&lt;2&gt;%7C3|x.example/a?b=1&amp;c=&lt;2&gt;%7C3>"
        );
    }
}
