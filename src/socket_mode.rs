use crate::{App, workflow};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::Message;

pub async fn worker(app: Arc<App>, token: String) {
    loop {
        if let Err(message) = connect(&app, &token).await {
            app.delivery_health
                .socket_disconnected_since(std::time::Instant::now());
            eprintln!("Slack Socket Mode: {message}");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn connect(app: &App, token: &str) -> Result<(), &'static str> {
    let response: Value = app
        .http
        .post("https://slack.com/api/apps.connections.open")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "connection request failed")?
        .json()
        .await
        .map_err(|_| "invalid connection response")?;
    if response["ok"] != true {
        return Err("connection request rejected; check app token");
    }
    let url = response["url"].as_str().ok_or("connection URL missing")?;
    let parsed = reqwest::Url::parse(url).map_err(|_| "invalid connection URL")?;
    if parsed.scheme() != "wss"
        || !parsed
            .host_str()
            .is_some_and(|h| h == "slack.com" || h.ends_with(".slack.com"))
    {
        return Err("untrusted connection URL");
    }
    let (mut socket, _) = tokio_tungstenite::connect_async(url)
        .await
        .map_err(|_| "WebSocket connection failed")?;
    app.delivery_health.socket_connected();
    eprintln!("Slack Socket Mode connected");
    while let Some(frame) = socket.next().await {
        let frame = frame.map_err(|_| "WebSocket read failed")?;
        if frame.is_close() {
            break;
        }
        let Message::Text(text) = frame else { continue };
        let envelope: Value =
            serde_json::from_str(&text).map_err(|_| "invalid WebSocket message")?;
        if envelope["type"] == "disconnect" {
            break;
        }
        // Slack splits interactive requests across every open connection of the app, so a
        // second consumer silently takes some button presses away from this service.
        if envelope["type"] == "hello" {
            eprintln!(
                "Slack Socket Mode open connections for this app: {}",
                envelope["num_connections"]
            );
        }
        let Some(id) = envelope["envelope_id"].as_str() else {
            continue;
        };
        eprintln!(
            "Slack envelope received: type={} payload={} action={}",
            envelope["type"],
            envelope["payload"]["type"],
            envelope["payload"]["actions"][0]["action_id"]
        );
        if envelope["type"] != "interactive"
            || !matches!(
                envelope["payload"]["actions"][0]["action_id"].as_str(),
                Some("confirm" | "ack" | "message_confirm")
            )
        {
            continue;
        }
        {
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                workflow::action_payload(Arc::new(app.clone()), envelope["payload"].clone()),
            )
            .await;
            match result {
                Ok(Ok(_)) => {}
                Ok(Err((status, reason))) => {
                    eprintln!("Slack action rejected: {status} {reason}");
                    if status.is_server_error() {
                        continue;
                    }
                }
                Err(_) => {
                    eprintln!("Slack action storage timed out");
                    continue;
                }
            }
        }
        socket
            .send(Message::Text(json!({"envelope_id":id}).to_string().into()))
            .await
            .map_err(|_| "WebSocket acknowledgement failed")?;
    }
    Err("WebSocket disconnected")
}
