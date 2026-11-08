//! Accepts Slack incoming webhook payloads so existing senders only change their URL.
use crate::messages::{Level, Message, accept};
use crate::{App, db_error};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::Value;
use sqlx::Row;
use std::sync::Arc;

const TITLE_LIMIT: usize = 200;
const BODY_LIMIT: usize = 2000;

#[derive(Deserialize)]
pub struct HookQuery {
    environment: Option<String>,
    #[serde(default)]
    dry_run: bool,
}

// Slack answers with plain-text codes, and some senders compare the body with "ok".
fn reply(status: StatusCode, text: &'static str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain")], text).into_response()
}

pub async fn ingest(
    State(app): State<Arc<App>>,
    Path(token): Path<String>,
    Query(query): Query<HookQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let environment = query.environment.unwrap_or_else(|| "production".into());
    if environment.trim().is_empty() || environment.len() > 256 {
        return reply(StatusCode::BAD_REQUEST, "invalid_environment");
    }
    let (source, service) = match client(&app, &token).await {
        Ok(Some(found)) => found,
        // Slack answers an unknown webhook URL with 404 no_service.
        Ok(None) => return reply(StatusCode::NOT_FOUND, "no_service"),
        Err((status, _)) => return reply(status, "internal_error"),
    };
    let Some(payload) = parse(&headers, &body) else {
        return reply(StatusCode::BAD_REQUEST, "invalid_payload");
    };
    let Some(message) = to_message(&payload, &service, &environment) else {
        return reply(StatusCode::BAD_REQUEST, "no_text");
    };
    match accept(&app, source, message, Some(payload), query.dry_run).await {
        // A preview answers with the converted message instead of Slack's plain "ok".
        Ok((status, body)) if query.dry_run => (status, body).into_response(),
        Ok(_) => reply(StatusCode::OK, "ok"),
        Err((StatusCode::UNPROCESSABLE_ENTITY, _)) => {
            reply(StatusCode::BAD_REQUEST, "invalid_payload")
        }
        Err((StatusCode::SERVICE_UNAVAILABLE, _)) => {
            reply(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
        }
        Err((status, _)) => reply(status, "internal_error"),
    }
}

pub(crate) async fn client(
    app: &App,
    token: &str,
) -> Result<Option<(String, String)>, (StatusCode, String)> {
    use sha2::{Digest, Sha256};
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let row = sqlx::query("UPDATE client_tokens SET last_used_at=now() WHERE token_hash=$1 AND revoked_at IS NULL RETURNING id,service")
        .bind(hash)
        .fetch_optional(&app.db)
        .await
        .map_err(db_error)?;
    Ok(row.map(|r| {
        (
            format!("client:{}", r.get::<String, _>("id")),
            r.get("service"),
        )
    }))
}

/// Slack accepts JSON and the legacy form body `payload=<json>`.
fn parse(headers: &HeaderMap, body: &[u8]) -> Option<Value> {
    let form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
    let value: Value = if form {
        let fields: Vec<(String, String)> = serde_urlencoded::from_bytes(body).ok()?;
        let (_, json) = fields.into_iter().find(|(name, _)| name == "payload")?;
        serde_json::from_str(&json).ok()?
    } else {
        serde_json::from_slice(body).ok()?
    };
    value.is_object().then_some(value)
}

pub(crate) fn to_message(payload: &Value, service: &str, environment: &str) -> Option<Message> {
    let mut parts = Vec::new();
    for block in payload["blocks"].as_array().into_iter().flatten() {
        collect_block(block, &mut parts);
    }
    // Slack shows the blocks and uses `text` only as the notification fallback.
    if parts.is_empty()
        && let Some(text) = payload["text"].as_str()
    {
        parts.push(text.to_owned());
    }
    let mut level = Level::Info;
    for attachment in payload["attachments"].as_array().into_iter().flatten() {
        level = higher(
            level,
            color_level(attachment["color"].as_str().unwrap_or("")),
        );
        collect_attachment(attachment, &mut parts);
    }
    let raw = parts.join("\n");
    let notify = ["<!here", "<!channel", "<!everyone"]
        .iter()
        .any(|mention| raw.contains(mention));
    let plain = plain_text(&raw);
    let lines: Vec<&str> = plain
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let (first, rest) = lines.split_first()?;
    let title = clip(first, TITLE_LIMIT);
    let rest = rest.join("\n");
    let body = if rest.is_empty() {
        title.clone()
    } else {
        clip(&rest, BODY_LIMIT)
    };
    Some(Message {
        event_id: format!("slack-hook-{}", uuid::Uuid::new_v4()),
        service: service.to_owned(),
        environment: environment.to_owned(),
        title,
        body,
        notify_users: notify.then_some(true),
        level: Some(level),
    })
}

fn collect_block(block: &Value, parts: &mut Vec<String>) {
    let mut push = |value: &Value| {
        if let Some(text) = value["text"].as_str().or_else(|| value.as_str()) {
            parts.push(text.to_owned());
        }
    };
    match block["type"].as_str() {
        Some("header" | "section") => {
            push(&block["text"]);
            for field in block["fields"].as_array().into_iter().flatten() {
                push(field);
            }
        }
        Some("context") => {
            for element in block["elements"].as_array().into_iter().flatten() {
                if element["type"] != "image" {
                    push(element);
                }
            }
        }
        _ => {}
    }
}

fn collect_attachment(attachment: &Value, parts: &mut Vec<String>) {
    let before = parts.len();
    if let Some(pretext) = attachment["pretext"].as_str() {
        parts.push(pretext.to_owned());
    }
    match (
        attachment["title"].as_str(),
        attachment["title_link"].as_str(),
    ) {
        (Some(title), Some(link)) => {
            parts.push(title.to_owned());
            parts.push(link.to_owned());
        }
        (Some(text), None) | (None, Some(text)) => parts.push(text.to_owned()),
        (None, None) => {}
    }
    if let Some(text) = attachment["text"].as_str() {
        parts.push(text.to_owned());
    }
    for field in attachment["fields"].as_array().into_iter().flatten() {
        let title = field["title"].as_str().unwrap_or("");
        let value = field["value"].as_str().unwrap_or("");
        parts.push(if title.is_empty() {
            value.to_owned()
        } else {
            format!("{title}: {value}")
        });
    }
    for block in attachment["blocks"].as_array().into_iter().flatten() {
        collect_block(block, parts);
    }
    if parts.len() == before
        && let Some(fallback) = attachment["fallback"].as_str()
    {
        parts.push(fallback.to_owned());
    }
}

fn color_level(color: &str) -> Level {
    match color {
        "danger" => Level::Error,
        "warning" => Level::Warn,
        _ => Level::Info,
    }
}

fn higher(a: Level, b: Level) -> Level {
    let rank = |level: Level| match level {
        Level::Crit => 0,
        Level::Error => 1,
        Level::Warn => 2,
        Level::Info => 3,
    };
    if rank(b) < rank(a) { b } else { a }
}

/// Rewrites Slack mrkdwn, because noti-mesh posts the text as plain_text.
fn plain_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('>') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let replacement = reference(&after[..end]);
        if replacement.is_empty() {
            out.truncate(out.trim_end_matches(' ').len());
        }
        out.push_str(&replacement);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out.lines()
        .map(strip_formatting)
        .collect::<Vec<_>>()
        .join("\n")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn reference(inner: &str) -> String {
    let (target, label) = match inner.split_once('|') {
        Some((target, label)) => (target, Some(label)),
        None => (inner, None),
    };
    if let Some(user) = target.strip_prefix('@') {
        return format!("@{}", label.unwrap_or(user));
    }
    if let Some(channel) = target.strip_prefix('#') {
        return format!("#{}", label.unwrap_or(channel));
    }
    if let Some(special) = target.strip_prefix('!') {
        // notify_users carries these broadcasts, so the words would only repeat it.
        if ["here", "channel", "everyone"].contains(&special) {
            return String::new();
        }
        return match label {
            Some(label) if special.starts_with("subteam^") || special.starts_with("date^") => {
                label.to_owned()
            }
            _ => format!("@{}", special.split('^').next().unwrap_or(special)),
        };
    }
    match label {
        Some(label) if label != target => format!("{label} ({target})"),
        _ => target.to_owned(),
    }
}

/// Removes `*bold*`, `_italic_`, and `~strike~` markers with Slack's word-boundary rule,
/// so `snake_case`, `2 * 3`, and paths keep their characters. Code spans stay as written.
fn strip_formatting(line: &str) -> String {
    const OPEN_AFTER: &str = "([{\"'";
    const CLOSE_BEFORE: &str = ".,;:!?)]}\"'";
    let chars: Vec<char> = line.chars().collect();
    let mut removed = vec![false; chars.len()];
    let mut in_code = false;
    for i in 0..chars.len() {
        let marker = chars[i];
        if marker == '`' {
            in_code = !in_code;
            continue;
        }
        if in_code || removed[i] || !matches!(marker, '*' | '_' | '~') {
            continue;
        }
        let opens = (i == 0 || chars[i - 1].is_whitespace() || OPEN_AFTER.contains(chars[i - 1]))
            && chars
                .get(i + 1)
                .is_some_and(|next| !next.is_whitespace() && *next != marker);
        if !opens {
            continue;
        }
        let close = (i + 2..chars.len())
            .take_while(|&j| chars[j] != '`')
            .find(|&j| {
                chars[j] == marker
                    && !chars[j - 1].is_whitespace()
                    && chars
                        .get(j + 1)
                        .is_none_or(|next| next.is_whitespace() || CLOSE_BEFORE.contains(*next))
            });
        if let Some(j) = close {
            removed[i] = true;
            removed[j] = true;
        }
    }
    chars
        .iter()
        .zip(removed)
        .filter(|(_, gone)| !gone)
        .map(|(c, _)| c)
        .collect()
}

fn clip(text: &str, limit: usize) -> String {
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
    use serde_json::json;

    fn convert(payload: Value) -> Message {
        to_message(&payload, "billing", "production").expect("payload has text")
    }

    #[test]
    fn text_becomes_title_and_body() {
        let message = convert(json!({"text":"Deploy finished\nversion 1.4.2 is live"}));
        assert_eq!(message.title, "Deploy finished");
        assert_eq!(message.body, "version 1.4.2 is live");
        assert_eq!(message.level, Some(Level::Info));
        assert_eq!(message.notify_users, None);
        assert_eq!(
            (message.service.as_str(), message.environment.as_str()),
            ("billing", "production")
        );
        assert!(message.event_id.starts_with("slack-hook-"));
    }

    #[test]
    fn one_line_text_uses_the_title_as_body() {
        let message = convert(json!({"text":"Backup done"}));
        assert_eq!(
            (message.title.as_str(), message.body.as_str()),
            ("Backup done", "Backup done")
        );
    }

    #[test]
    fn mrkdwn_references_become_plain_text() {
        let message = convert(
            json!({"text":"<!here> build failed\nsee <https://ci.example/1|run 1> by <@U123> in <#C9|ops> &amp; <https://ci.example/raw>"}),
        );
        assert_eq!(message.title, "build failed");
        assert_eq!(
            message.body,
            "see run 1 (https://ci.example/1) by @U123 in #ops & https://ci.example/raw"
        );
        assert_eq!(message.notify_users, Some(true));
    }

    #[test]
    fn blocks_win_over_the_fallback_text() {
        let message = convert(json!({
            "text":"fallback only",
            "blocks":[
                {"type":"header","text":{"type":"plain_text","text":"Nightly job"}},
                {"type":"section","text":{"type":"mrkdwn","text":"*3* rows failed"},"fields":[{"type":"mrkdwn","text":"*Job:* export"}]},
                {"type":"divider"},
                {"type":"context","elements":[{"type":"image","image_url":"x","alt_text":"x"},{"type":"mrkdwn","text":"host db-1"}]}
            ]
        }));
        assert_eq!(message.title, "Nightly job");
        assert_eq!(message.body, "3 rows failed\nJob: export\nhost db-1");
    }

    #[test]
    fn attachments_add_text_and_set_the_level() {
        let message = convert(json!({
            "text":"Disk alert",
            "attachments":[
                {"color":"warning","title":"db-1","title_link":"https://grafana.example/d/1","text":"disk 85%","fields":[{"title":"Mount","value":"/data","short":true}]},
                {"color":"danger","fallback":"db-2 disk 97%"}
            ]
        }));
        assert_eq!(message.title, "Disk alert");
        assert_eq!(
            message.body,
            "db-1\nhttps://grafana.example/d/1\ndisk 85%\nMount: /data\ndb-2 disk 97%"
        );
        assert_eq!(message.level, Some(Level::Error));
    }

    #[test]
    fn broadcast_mentions_leave_the_text() {
        let message = convert(json!({"text":"build <!channel> failed\n<!everyone|everyone>"}));
        assert_eq!(message.title, "build failed");
        assert_eq!(message.body, "build failed");
        assert_eq!(message.notify_users, Some(true));
    }

    #[test]
    fn formatting_markers_are_removed_only_around_words() {
        let message = convert(json!({
            "text":"*Deploy* _done_ ~old~ (*v2*)\nkeep snake_case_name, 2 * 3 * 4, a*b*c, `*code*` and https://x.example/a_b_c/_d_/"
        }));
        assert_eq!(message.title, "Deploy done old (v2)");
        assert_eq!(
            message.body,
            "keep snake_case_name, 2 * 3 * 4, a*b*c, `*code*` and https://x.example/a_b_c/_d_/"
        );
    }

    #[test]
    fn payload_without_text_is_rejected() {
        assert!(to_message(&json!({"text":"  \n "}), "s", "e").is_none());
        assert!(to_message(&json!({"blocks":[{"type":"divider"}]}), "s", "e").is_none());
        assert!(to_message(&json!({}), "s", "e").is_none());
    }

    #[test]
    fn long_text_is_clipped_on_a_character_boundary() {
        let message =
            convert(json!({"text":format!("{}\n{}", "가".repeat(100), "나".repeat(1000))}));
        assert!(message.title.len() <= TITLE_LIMIT && message.title.ends_with('…'));
        assert!(message.body.len() <= BODY_LIMIT && message.body.ends_with('…'));
    }

    #[test]
    fn form_body_carries_the_json_in_payload() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            "application/x-www-form-urlencoded".parse().unwrap(),
        );
        let body = serde_urlencoded::to_string([("payload", r#"{"text":"from form"}"#)]).unwrap();
        assert_eq!(
            parse(&headers, body.as_bytes()).unwrap()["text"],
            "from form"
        );
        assert!(parse(&headers, b"text=missing").is_none());
        assert!(parse(&HeaderMap::new(), b"[1,2]").is_none());
    }
}
