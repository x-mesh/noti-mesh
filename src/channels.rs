use crate::{
    App,
    policy::{Channels, DeliveryMode, Group, SmtpSecurity},
};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor, message::Mailbox,
    transport::smtp::authentication::Credentials,
};
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use std::{sync::Arc, time::Duration};

pub struct EmailSender {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}
pub struct TelegramSender {
    token: String,
    base: String,
}
#[derive(Default)]
pub struct Providers {
    email: Option<EmailSender>,
    telegram: Option<TelegramSender>,
}
impl Providers {
    pub fn configure(config: &Channels, development: bool) -> Result<Self, String> {
        let email = if let Some(c) = &config.email {
            let builder = match c.security {
                SmtpSecurity::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&c.host)
                    .map_err(|_| "invalid SMTP TLS configuration")?,
                SmtpSecurity::Starttls => {
                    AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&c.host)
                        .map_err(|_| "invalid SMTP STARTTLS configuration")?
                }
                SmtpSecurity::Local => {
                    if !development || !matches!(c.host.as_str(), "127.0.0.1" | "localhost" | "::1")
                    {
                        return Err("unencrypted SMTP is restricted to local development".into());
                    }
                    AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&c.host)
                }
            };
            let mut builder = builder.port(c.port).timeout(Some(Duration::from_secs(10)));
            if let (Some(user), Some(password)) = (&c.username_env, &c.password_env) {
                let user = std::env::var(user)
                    .map_err(|_| "SMTP username environment variable is missing")?;
                let password = std::env::var(password)
                    .map_err(|_| "SMTP password environment variable is missing")?;
                if user.trim().is_empty() || password.is_empty() {
                    return Err("SMTP credentials cannot be empty".into());
                }
                builder = builder.credentials(Credentials::new(user, password));
            }
            Some(EmailSender {
                transport: builder.build(),
                from: c.from.parse().map_err(|_| "invalid SMTP sender")?,
            })
        } else {
            None
        };
        let telegram = if let Some(c) = &config.telegram {
            let url = reqwest::Url::parse(&c.api_base).map_err(|_| "invalid Telegram API URL")?;
            let local = development
                && url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            if url.scheme() != "https" && !local {
                return Err("Telegram API requires HTTPS outside local development".into());
            }
            let token = std::env::var(&c.token_env)
                .map_err(|_| "Telegram token environment variable is missing")?;
            let Some((id, secret)) = token.split_once(':') else {
                return Err("invalid Telegram token".into());
            };
            if id.is_empty()
                || !id.bytes().all(|b| b.is_ascii_digit())
                || secret.is_empty()
                || !secret
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err("invalid Telegram token".into());
            }
            Some(TelegramSender {
                token,
                base: c.api_base.trim_end_matches('/').to_owned(),
            })
        } else {
            None
        };
        Ok(Self { email, telegram })
    }
}

pub async fn enqueue_incident(
    tx: &mut Transaction<'_, Postgres>,
    group: &Group,
    incident_id: &str,
    outbox_id: i64,
) -> Result<(), sqlx::Error> {
    if !group.slack_enabled {
        sqlx::query("UPDATE outbox SET slack_disabled=true,last_error='Slack disabled by policy' WHERE id=$1").bind(outbox_id).execute(&mut **tx).await?;
    }
    let kind: String = sqlx::query_scalar("SELECT kind FROM outbox WHERE id=$1")
        .bind(outbox_id)
        .fetch_one(&mut **tx)
        .await?;
    for d in &group.destinations {
        if d.mode == DeliveryMode::Unacknowledged && kind != "escalation" {
            continue;
        }
        let state = match d.mode {
            DeliveryMode::Fallback => "standby",
            DeliveryMode::Parallel | DeliveryMode::Unacknowledged => "pending",
        };
        sqlx::query("INSERT INTO channel_deliveries(incident_id,parent_outbox_id,channel,destination,mode,state,due_at,incident_state,incident_priority) SELECT $1,$2,$3,$4,$5,$6,o.due_at,i.state,i.severity FROM outbox o JOIN incidents i ON i.id=o.incident_id WHERE o.id=$2 AND i.id=$1").bind(incident_id).bind(outbox_id).bind(d.channel.as_str()).bind(&d.address).bind(d.mode.as_str()).bind(state).execute(&mut **tx).await?;
    }
    Ok(())
}
pub async fn enqueue_message(
    tx: &mut Transaction<'_, Postgres>,
    group: &Group,
    message_id: &str,
) -> Result<(), sqlx::Error> {
    if !group.slack_enabled {
        sqlx::query("UPDATE gateway_messages SET slack_disabled=true WHERE id=$1")
            .bind(message_id)
            .execute(&mut **tx)
            .await?;
    }
    for d in &group.destinations {
        if d.mode == DeliveryMode::Unacknowledged {
            continue;
        }
        let state = match d.mode {
            DeliveryMode::Fallback => "standby",
            DeliveryMode::Parallel | DeliveryMode::Unacknowledged => "pending",
        };
        sqlx::query("INSERT INTO channel_deliveries(message_id,channel,destination,mode,state,due_at) SELECT $1,$2,$3,$4,$5,due_at FROM gateway_messages WHERE id=$1").bind(message_id).bind(d.channel.as_str()).bind(&d.address).bind(d.mode.as_str()).bind(state).execute(&mut **tx).await?;
    }
    Ok(())
}
pub async fn finish_incident(
    tx: &mut Transaction<'_, Postgres>,
    outbox_id: i64,
    success: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE channel_deliveries SET state=CASE WHEN $2 THEN 'cancelled' ELSE 'pending' END,last_error=CASE WHEN $2 THEN 'primary Slack delivery succeeded' ELSE NULL END WHERE parent_outbox_id=$1 AND mode='fallback' AND state='standby'").bind(outbox_id).bind(success).execute(&mut **tx).await?;
    if success {
        sqlx::query("UPDATE channel_deliveries SET state='cancelled',last_error='primary Slack delivery succeeded' WHERE parent_outbox_id=$1 AND mode='fallback' AND state='pending' AND (lease_until IS NULL OR lease_until<now())").bind(outbox_id).execute(&mut **tx).await?;
    }
    Ok(())
}
pub async fn finish_message(
    tx: &mut Transaction<'_, Postgres>,
    message_id: &str,
    success: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE channel_deliveries SET state=CASE WHEN $2 THEN 'cancelled' ELSE 'pending' END,last_error=CASE WHEN $2 THEN 'primary Slack delivery succeeded' ELSE NULL END WHERE message_id=$1 AND mode='fallback' AND state='standby'").bind(message_id).bind(success).execute(&mut **tx).await?;
    if success {
        sqlx::query("UPDATE channel_deliveries SET state='cancelled',last_error='primary Slack delivery succeeded' WHERE message_id=$1 AND mode='fallback' AND state='pending' AND (lease_until IS NULL OR lease_until<now())").bind(message_id).execute(&mut **tx).await?;
    }
    Ok(())
}
pub async fn finish_digest(
    tx: &mut Transaction<'_, Postgres>,
    lease: &str,
    success: bool,
) -> Result<(), sqlx::Error> {
    let ids: Vec<(i64,)> = sqlx::query_as("SELECT id FROM outbox WHERE lease_token=$1")
        .bind(lease)
        .fetch_all(&mut **tx)
        .await?;
    for (id,) in ids {
        finish_incident(tx, id, success).await?;
    }
    Ok(())
}

pub async fn reconcile(app: &App) -> Result<(), sqlx::Error> {
    let mut cursor = 0_i64;
    loop {
        let rows=sqlx::query("SELECT c.id,c.channel,c.destination,c.mode,coalesce(o.target,m.target) AS target FROM channel_deliveries c LEFT JOIN outbox o ON o.id=c.parent_outbox_id LEFT JOIN gateway_messages m ON m.id=c.message_id WHERE c.id>$1 AND c.state IN ('standby','pending') AND (c.lease_until IS NULL OR c.lease_until<now()) ORDER BY c.id LIMIT 500").bind(cursor).fetch_all(&app.db).await?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let id: i64 = row.get("id");
            cursor = id;
            let target: Option<String> = row.get("target");
            let channel: String = row.get("channel");
            let destination: String = row.get("destination");
            let mode: String = row.get("mode");
            let allowed = target
                .and_then(|name| app.config.groups.get(&name))
                .is_some_and(|group| {
                    group.destinations.iter().any(|d| {
                        d.channel.as_str() == channel
                            && d.address == destination
                            && d.mode.as_str() == mode
                    })
                });
            if !allowed {
                sqlx::query("UPDATE channel_deliveries SET state='cancelled',last_error='destination no longer configured' WHERE id=$1 AND state IN ('standby','pending') AND (lease_until IS NULL OR lease_until<now())").bind(id).execute(&app.db).await?;
            }
        }
    }
    Ok(())
}
pub async fn worker(app: Arc<App>) {
    loop {
        match tick(&app).await {
            Ok(worked) => {
                if let Err(error) = crate::worker_heartbeat(&app.db, "channels").await {
                    eprintln!("channels heartbeat: {error}");
                }
                if worked {
                    continue;
                }
            }
            Err(error) => eprintln!("channel delivery: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
pub(crate) async fn tick(app: &App) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    sqlx::query("UPDATE channel_deliveries c SET state='cancelled',last_error='incident state changed' FROM incidents i WHERE c.incident_id=i.id AND c.state IN ('standby','pending') AND (c.lease_until IS NULL OR c.lease_until<now()) AND (i.state<>c.incident_state OR i.severity<c.incident_priority)").execute(&app.db).await?;
    sqlx::query("UPDATE channel_deliveries c SET state='cancelled',last_error='primary operation was cancelled' FROM outbox o WHERE c.parent_outbox_id=o.id AND c.state IN ('standby','pending') AND (c.lease_until IS NULL OR c.lease_until<now()) AND o.sent_at IS NOT NULL AND (o.last_error LIKE 'cancelled%' OR o.last_error LIKE 'superseded%')").execute(&app.db).await?;
    let lease = uuid::Uuid::new_v4().to_string();
    let row=sqlx::query("UPDATE channel_deliveries SET lease_token=$1,lease_until=now()+interval '30 seconds' WHERE id=(SELECT id FROM channel_deliveries WHERE state='pending' AND due_at<=now() AND (lease_until IS NULL OR lease_until<now()) ORDER BY due_at,id FOR UPDATE SKIP LOCKED LIMIT 1) RETURNING *").bind(&lease).fetch_optional(&app.db).await?;
    let Some(row) = row else { return Ok(false) };
    let id: i64 = row.get("id");
    let channel: String = row.get("channel");
    let destination: String = row.get("destination");
    let mode: String = row.get("mode");
    let incident_id: Option<String> = row.get("incident_id");
    let message_id: Option<String> = row.get("message_id");
    let (group, title, body) = if let Some(incident_id) = &incident_id {
        let incident = sqlx::query(
            "SELECT i.*,o.target,(o.sent_at IS NOT NULL) AS primary_sent,o.last_error AS primary_error FROM incidents i JOIN outbox o ON o.id=$2 WHERE i.id=$1",
        )
        .bind(incident_id)
        .bind(row.get::<i64, _>("parent_outbox_id"))
        .fetch_one(&app.db)
        .await?;
        let primary_error: Option<String> = incident.get("primary_error");
        let primary_cancelled = primary_error
            .as_deref()
            .is_some_and(|error| error.starts_with("cancelled") || error.starts_with("superseded"));
        if incident.get::<String, _>("state") != row.get::<String, _>("incident_state")
            || incident.get::<i16, _>("severity") < row.get::<i16, _>("incident_priority")
            || primary_cancelled
            || mode == "fallback" && incident.get::<bool, _>("primary_sent")
        {
            cancel_leased(app, id, &lease, "incident or primary delivery changed").await?;
            return Ok(true);
        }
        let context:Option<(Value,)>=sqlx::query_as("SELECT payload FROM events WHERE incident_id=$1 ORDER BY (payload->>'sequence')::bigint DESC LIMIT 1").bind(incident_id).fetch_optional(&app.db).await?;
        let context = context.map(|v| v.0).unwrap_or_else(|| json!({}));
        let title = format!(
            "[P{}] {}",
            incident.get::<i16, _>("severity"),
            incident.get::<String, _>("incident_key")
        );
        let body = format!(
            "{} / {}\n상태: {}\n이벤트: {}건\n{}\n{}",
            incident.get::<String, _>("service"),
            incident.get::<String, _>("environment"),
            incident.get::<String, _>("state"),
            incident.get::<i64, _>("count"),
            context["summary"].as_str().unwrap_or(""),
            context["source_url"].as_str().unwrap_or("")
        );
        (incident.get::<String, _>("target"), title, body)
    } else {
        let message = sqlx::query("SELECT target,payload,(sent_at IS NOT NULL) AS primary_sent FROM gateway_messages WHERE id=$1")
            .bind(message_id.as_deref().ok_or("missing message ID")?)
            .fetch_one(&app.db)
            .await?;
        if mode == "fallback" && message.get::<bool, _>("primary_sent") {
            cancel_leased(app, id, &lease, "primary Slack delivery succeeded").await?;
            return Ok(true);
        }
        let payload: Value = message.get("payload");
        (
            message.get::<String, _>("target"),
            payload["title"].as_str().unwrap_or("").to_owned(),
            payload["body"].as_str().unwrap_or("").to_owned(),
        )
    };
    let allowed = app.config.groups.get(&group).is_some_and(|g| {
        g.destinations.iter().any(|d| {
            d.channel.as_str() == channel
                && d.address == destination
                && d.mode.as_str() == row.get::<String, _>("mode")
        })
    });
    if !allowed {
        cancel_leased(app, id, &lease, "destination no longer configured").await?;
        return Ok(true);
    }
    let outcome = match channel.as_str() {
        "email" => send_email(&app.channel_providers, &destination, &title, &body).await,
        "telegram" => {
            send_telegram(
                &app.channel_providers,
                &app.http,
                &destination,
                &title,
                &body,
            )
            .await
        }
        _ => DeliveryOutcome::Failed("unsupported channel".into()),
    };
    let (state, provider_id, delay, error) = match outcome {
        DeliveryOutcome::Sent(id) => ("sent", id, 0, String::new()),
        DeliveryOutcome::Retry(delay, error) => ("pending", None, delay, error),
        DeliveryOutcome::Failed(error) => ("failed", None, 0, error),
    };
    sqlx::query("UPDATE channel_deliveries SET state=$2,provider_message_id=$3,attempts=attempts+CASE WHEN $2='sent' THEN 0 ELSE 1 END,last_error=NULLIF($4,''),due_at=now()+$5*interval '1 second',sent_at=CASE WHEN $2='sent' THEN now() ELSE NULL END,content_snapshot=CASE WHEN $2='sent' THEN $7 ELSE content_snapshot END,lease_until=NULL WHERE id=$1 AND lease_token=$6").bind(id).bind(state).bind(provider_id).bind(error).bind(delay as f64).bind(lease).bind(json!({"title":title,"body":body})).execute(&app.db).await?;
    Ok(true)
}
async fn cancel_leased(app: &App, id: i64, lease: &str, reason: &str) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE channel_deliveries SET state='cancelled',last_error=$3,lease_until=NULL WHERE id=$1 AND lease_token=$2 AND state='pending'").bind(id).bind(lease).bind(reason).execute(&app.db).await?;
    Ok(())
}
enum DeliveryOutcome {
    Sent(Option<String>),
    Retry(i64, String),
    Failed(String),
}
async fn send_email(
    providers: &Providers,
    destination: &str,
    title: &str,
    body: &str,
) -> DeliveryOutcome {
    let Some(sender) = &providers.email else {
        return DeliveryOutcome::Failed("email provider unavailable".into());
    };
    let recipient: Mailbox = match destination.parse() {
        Ok(value) => value,
        Err(_) => return DeliveryOutcome::Failed("invalid email destination".into()),
    };
    let title = title.replace(['\r', '\n'], " ");
    let message = match Message::builder()
        .from(sender.from.clone())
        .to(recipient)
        .subject(title)
        .body(body.to_owned())
    {
        Ok(value) => value,
        Err(_) => return DeliveryOutcome::Failed("invalid email content".into()),
    };
    match sender.transport.send(message).await {
        Ok(_) => DeliveryOutcome::Sent(None),
        Err(error) => {
            let code = error
                .status()
                .map(|code| format!(" ({code})"))
                .unwrap_or_default();
            if error.is_permanent() || error.is_tls() || error.is_client() {
                DeliveryOutcome::Failed(format!("SMTP permanent failure{code}"))
            } else if error.is_timeout() {
                DeliveryOutcome::Retry(60, "SMTP timeout".into())
            } else {
                DeliveryOutcome::Retry(60, format!("SMTP temporary failure{code}"))
            }
        }
    }
}
async fn send_telegram(
    providers: &Providers,
    http: &reqwest::Client,
    destination: &str,
    title: &str,
    body: &str,
) -> DeliveryOutcome {
    let Some(sender) = &providers.telegram else {
        return DeliveryOutcome::Failed("Telegram provider unavailable".into());
    };
    let text = format!("{title}\n{body}");
    let result = http
        .post(format!("{}/bot{}/sendMessage", sender.base, sender.token))
        .json(&json!({"chat_id":destination,"text":text}))
        .send()
        .await;
    match result {
        Ok(response) => {
            let status = response.status();
            match response.json::<Value>().await {
                Ok(value)
                    if status.is_success()
                        && value["ok"] == true
                        && value["result"]["message_id"].as_i64().is_some() =>
                {
                    DeliveryOutcome::Sent(
                        value["result"]["message_id"]
                            .as_i64()
                            .map(|id| id.to_string()),
                    )
                }
                Ok(value) => {
                    let code = value["error_code"]
                        .as_u64()
                        .and_then(|v| u16::try_from(v).ok())
                        .unwrap_or(status.as_u16());
                    let error = format!("Telegram API error {code}");
                    if code == 429 || code == 408 || code >= 500 {
                        DeliveryOutcome::Retry(
                            value["parameters"]["retry_after"]
                                .as_i64()
                                .unwrap_or(60)
                                .max(1),
                            error,
                        )
                    } else if (400..500).contains(&code) {
                        DeliveryOutcome::Failed(error)
                    } else {
                        DeliveryOutcome::Retry(60, error)
                    }
                }
                Err(_) => {
                    if status.is_client_error() && status.as_u16() != 429 && status.as_u16() != 408
                    {
                        DeliveryOutcome::Failed(format!(
                            "Telegram response error {}",
                            status.as_u16()
                        ))
                    } else {
                        DeliveryOutcome::Retry(
                            60,
                            format!("Telegram response error {}", status.as_u16()),
                        )
                    }
                }
            }
        }
        Err(_) => DeliveryOutcome::Retry(60, "Telegram request failed".into()),
    }
}

#[cfg(test)]
#[path = "tests/channels.rs"]
mod tests;
