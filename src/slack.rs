use serde_json::Value;

pub struct SlackFailure {
    pub message: String,
    pub retry_after: i64,
    pub permanent: bool,
}

pub async fn response(
    result: Result<reqwest::Response, reqwest::Error>,
    allow_missing_ts: bool,
) -> Result<Value, SlackFailure> {
    let response = result.map_err(|_| SlackFailure {
        message: "Slack request failed; delivery outcome uncertain".into(),
        retry_after: 60,
        permanent: false,
    })?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(60)
        .max(1);
    let body: Value = response.json().await.map_err(|_| SlackFailure {
        message: format!("Slack response error {}", status.as_u16()),
        retry_after,
        permanent: status.is_client_error() && status.as_u16() != 408 && status.as_u16() != 429,
    })?;
    if status.is_success() && body["ok"] == true {
        if body["ts"].is_string() || allow_missing_ts {
            return Ok(body);
        }
        return Err(SlackFailure {
            message: "Slack response missing message ID; delivery outcome uncertain".into(),
            retry_after,
            permanent: false,
        });
    }
    let code = body["error"]
        .as_str()
        .filter(|value| {
            value.len() <= 80
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .unwrap_or("unknown_error");
    // Slack reports most errors with HTTP 200 and ok=false. Only these codes can succeed
    // on a later attempt; any other code needs a configuration or content change.
    let transient = matches!(
        code,
        "ratelimited"
            | "rate_limited"
            | "internal_error"
            | "fatal_error"
            | "service_unavailable"
            | "request_timeout"
            | "unknown_error"
    );
    let permanent = if status.as_u16() == 429 || status.as_u16() == 408 || status.is_server_error()
    {
        false
    } else {
        !transient || status.is_client_error()
    };
    Err(SlackFailure {
        message: format!("Slack API error: {code} (HTTP {})", status.as_u16()),
        retry_after,
        permanent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn classify(status: u16, body: &str) -> SlackFailure {
        let response = axum::http::Response::builder()
            .status(status)
            .body(body.to_owned())
            .unwrap();
        match self::response(Ok(reqwest::Response::from(response)), false).await {
            Ok(_) => panic!("expected a Slack failure"),
            Err(failure) => failure,
        }
    }
    #[tokio::test]
    async fn unlisted_caller_errors_stop_retrying() {
        for code in ["is_archived", "message_not_found", "invalid_blocks"] {
            let body = format!(r#"{{"ok":false,"error":"{code}"}}"#);
            assert!(classify(200, &body).await.permanent, "{code}");
        }
    }
    #[tokio::test]
    async fn transient_slack_errors_keep_retrying() {
        for code in [
            "ratelimited",
            "internal_error",
            "fatal_error",
            "service_unavailable",
            "request_timeout",
        ] {
            let body = format!(r#"{{"ok":false,"error":"{code}"}}"#);
            assert!(!classify(200, &body).await.permanent, "{code}");
        }
        assert!(!classify(200, r#"{"ok":false}"#).await.permanent);
        assert!(
            !classify(429, r#"{"ok":false,"error":"ratelimited"}"#)
                .await
                .permanent
        );
        assert!(!classify(503, r#"{"ok":false}"#).await.permanent);
    }
}
