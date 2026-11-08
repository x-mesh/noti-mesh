mod admin;
mod alertmanager;
mod channels;
mod evidence;
mod health;
mod messages;
mod monitors;
mod policy;
mod report;
mod silence;
mod slack;
mod slack_hooks;
mod socket_mode;
mod workflow;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::{sync::Arc, time::Duration};

#[derive(Clone)]
pub(crate) struct App {
    config: policy::Config,
    source_tokens: Vec<(String, String)>,
    signing_secret: String,
    db: PgPool,
    http: reqwest::Client,
    slack_token: String,
    ai_token: Option<String>,
    channel_providers: std::sync::Arc<channels::Providers>,
    admin_token: Option<String>,
    admin_tokens: Vec<(String, String, policy::AdminRole)>,
    development_mode: bool,
    mock_delivery: bool,
    delivery_health: Arc<health::DeliveryHealth>,
    slack_check_enabled: bool,
}
#[cfg(test)]
impl App {
    pub(crate) fn for_test(db: PgPool, config: policy::Config) -> Self {
        App {
            db,
            config,
            source_tokens: vec![(
                "monitoring".into(),
                "test-token-with-at-least-32-characters".into(),
            )],
            signing_secret: "secret".into(),
            http: reqwest::Client::new(),
            slack_token: "test".into(),
            ai_token: None,
            channel_providers: Arc::new(channels::Providers::default()),
            admin_token: None,
            admin_tokens: vec![],
            development_mode: false,
            mock_delivery: false,
            delivery_health: Arc::new(health::DeliveryHealth::default()),
            slack_check_enabled: false,
        }
    }
}
#[derive(Deserialize, Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[serde(deny_unknown_fields)]
struct Event {
    event_id: String,
    service: String,
    environment: String,
    incident_key: String,
    sequence: i64,
    severity: i16,
    recovered: bool,
    occurred_at: String,
    correlation_id: Option<String>,
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
}
type Failure = (StatusCode, String);
fn authenticate(headers: &HeaderMap, app: &App) -> Result<String, Failure> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    for (source, token) in &app.source_tokens {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(token.as_bytes()).expect("HMAC accepts all key lengths");
        mac.update(b"noti-mesh-auth");
        let expected = mac.finalize().into_bytes();
        let mut candidate = Hmac::<Sha256>::new_from_slice(provided.as_bytes())
            .expect("HMAC accepts all key lengths");
        candidate.update(b"noti-mesh-auth");
        if candidate.verify_slice(&expected).is_ok() {
            return Ok(source.clone());
        }
    }
    Err((StatusCode::UNAUTHORIZED, "unauthorized".into()))
}
async fn authenticate_client(
    headers: &HeaderMap,
    app: &App,
    service: Option<&str>,
) -> Result<String, Failure> {
    if let Ok(source) = authenticate(headers, app) {
        return Ok(source);
    }
    use sha2::{Digest, Sha256};
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if token.is_empty() {
        return Err((StatusCode::UNAUTHORIZED, "unauthorized".into()));
    }
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let row=sqlx::query("UPDATE client_tokens SET last_used_at=now() WHERE token_hash=$1 AND revoked_at IS NULL AND ($2::text IS NULL OR service=$2) RETURNING id").bind(hash).bind(service).fetch_optional(&app.db).await.map_err(db_error)?;
    row.map(|r| format!("client:{}", r.get::<String, _>("id")))
        .ok_or((StatusCode::UNAUTHORIZED, "unauthorized".into()))
}
async fn client_identity(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    let source = authenticate_client(&headers, &app, None).await?;
    let service = if let Some(id) = source.strip_prefix("client:") {
        sqlx::query_scalar::<_, String>(
            "SELECT service FROM client_tokens WHERE id=$1 AND revoked_at IS NULL",
        )
        .bind(id)
        .fetch_optional(&app.db)
        .await
        .map_err(db_error)?
    } else {
        None
    };
    Ok(Json(json!({"service":service})))
}
fn db_error(e: sqlx::Error) -> Failure {
    eprintln!("database: {e}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "database operation failed".into(),
    )
}
#[derive(Deserialize, Default)]
struct Preview {
    #[serde(default)]
    dry_run: bool,
}
async fn ingest(
    State(app): State<Arc<App>>,
    Query(preview): Query<Preview>,
    headers: HeaderMap,
    Json(event): Json<Event>,
) -> Result<(StatusCode, Json<Value>), Failure> {
    let source = authenticate_client(&headers, &app, Some(&event.service)).await?;
    accept_event(&app, source, event, preview.dry_run).await
}
/// With `dry_run`, validates and routes the event and returns the result without storing or sending it.
async fn accept_event(
    app: &App,
    source: String,
    event: Event,
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
    if !(1..=4).contains(&event.severity)
        || event.sequence < 0
        || [
            &event.event_id,
            &event.service,
            &event.environment,
            &event.incident_key,
            &event.occurred_at,
        ]
        .iter()
        .any(|s| s.is_empty() || s.len() > 256)
    {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid event".into()));
    }
    if chrono::DateTime::parse_from_rfc3339(&event.occurred_at).is_err()
        || event.error_code.as_ref().is_some_and(|code| {
            code.len() > 64
                || !code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid timestamp or error code".into(),
        ));
    }
    if event
        .summary
        .as_ref()
        .is_some_and(|value| value.trim().is_empty() || value.len() > 1000)
        || event.source_url.as_ref().is_some_and(|value| {
            value.len() > 2048
                || reqwest::Url::parse(value).ok().is_none_or(|url| {
                    !matches!(url.scheme(), "http" | "https")
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                })
        })
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid summary or source URL".into(),
        ));
    }
    let payload = serde_json::to_value(&event)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid event".into()))?;
    let (_, effective_priority, _) =
        app.config
            .route(&event.service, &event.environment, event.severity);
    if dry_run {
        let (group, priority, unclassified) =
            app.config
                .route(&event.service, &event.environment, event.severity);
        return Ok((
            StatusCode::OK,
            Json(
                json!({"dry_run":true,"event":payload,"route":{"group":group,"priority":priority,"unclassified":unclassified,"slack_channel":app.config.groups[group].channel}}),
            ),
        ));
    }
    let mut tx = app.db.begin().await.map_err(db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(1)")
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    if let Some(row) =
        sqlx::query("SELECT payload,incident_id FROM events WHERE source=$2 AND event_id=$1")
            .bind(&event.event_id)
            .bind(&source)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_error)?
    {
        if row.get::<Value, _>("payload") != payload {
            return Err((StatusCode::CONFLICT, "event ID content conflict".into()));
        }
        return Ok((
            StatusCode::OK,
            Json(json!({"incident_id":row.get::<String,_>("incident_id")})),
        ));
    }
    let old=sqlx::query("SELECT * FROM incidents WHERE source=$4 AND service=$1 AND environment=$2 AND incident_key=$3 FOR UPDATE").bind(&event.service).bind(&event.environment).bind(&event.incident_key).bind(&source).fetch_optional(&mut *tx).await.map_err(db_error)?;
    let id = old
        .as_ref()
        .map(|r| r.get::<String, _>("id"))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let first = old.is_none();
    let held = old
        .as_ref()
        .is_some_and(|row| row.get::<Option<String>, _>("silence_id").is_some());
    // Nobody heard about an incident that arrives already resolved, so it is recorded without a message.
    let mut notify = first && !event.recovered;
    let mut reopened = false;
    if let Some(row) = old {
        if event.sequence > row.get::<i64, _>("sequence") {
            if row.get::<String, _>("state") == "resolved" && !event.recovered {
                return Err((
                    StatusCode::CONFLICT,
                    "use a new incident key after recovery".into(),
                ));
            }
            let severity = effective_priority.min(row.get("severity"));
            reopened = severity < row.get::<i16, _>("severity") && !event.recovered;
            notify = reopened || event.recovered && row.get::<String, _>("state") != "resolved";
            sqlx::query("UPDATE incidents SET sequence=$2,severity=$3,state=CASE WHEN $4 THEN 'resolved' WHEN $5 THEN 'open' ELSE state END,owner=CASE WHEN $5 THEN NULL ELSE owner END,count=count+1,dirty=true,next_ai=CASE WHEN $4 OR $5 THEN now() ELSE next_ai END,updated_at=now() WHERE id=$1").bind(&id).bind(event.sequence).bind(severity).bind(event.recovered).bind(severity<row.get::<i16,_>("severity")).execute(&mut *tx).await.map_err(db_error)?;
        }
    } else {
        sqlx::query("INSERT INTO incidents(id,source,service,environment,incident_key,sequence,severity,state) VALUES($1,$8,$2,$3,$4,$5,$6,$7)").bind(&id).bind(&event.service).bind(&event.environment).bind(&event.incident_key).bind(event.sequence).bind(effective_priority).bind(if event.recovered{"resolved"}else{"open"}).bind(&source).execute(&mut *tx).await.map_err(db_error)?;
    }
    sqlx::query("INSERT INTO events(source,event_id,payload,incident_id) VALUES($4,$1,$2,$3)")
        .bind(&event.event_id)
        .bind(payload)
        .bind(&id)
        .bind(&source)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    if notify || first {
        let (group, priority, _) =
            app.config
                .route(&event.service, &event.environment, effective_priority);
        sqlx::query("UPDATE incidents SET policy_snapshot=$2,next_ai=CASE WHEN $3 THEN next_ai ELSE NULL END WHERE id=$1").bind(&id).bind(json!({"source_severity":event.severity,"effective_priority":priority,"responsible_group":group,"unclassified":app.config.route(&event.service,&event.environment,event.severity).2})).bind(notify).execute(&mut *tx).await.map_err(db_error)?;
    }
    if notify {
        // A held incident was never announced, so its later changes wait for the silence to end too.
        // Silences only hold incidents that nobody has seen; an announced incident keeps its messages.
        let announced = !first
            && sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM outbox WHERE incident_id=$1 AND provider_message_ts IS NOT NULL)")
                .bind(&id)
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        let silence = if held || event.recovered || announced {
            None
        } else {
            silence::active(&mut tx, &event.service, &event.environment)
                .await
                .map_err(db_error)?
        };
        if held || silence.is_some() {
            sqlx::query("UPDATE incidents SET silence_id=coalesce(silence_id,$2),next_escalation=NULL WHERE id=$1")
                .bind(&id)
                .bind(&silence)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
            notify = false;
        }
    }
    if notify {
        schedule_notification(
            &mut tx,
            app,
            &id,
            &event.service,
            &event.environment,
            effective_priority,
            event.recovered,
            reopened,
        )
        .await
        .map_err(db_error)?;
    }
    tx.commit().await.map_err(db_error)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"incident_id":id}))))
}
/// Queues the Slack message and channel deliveries for an incident change and resets its timers.
#[allow(clippy::too_many_arguments)] // Each value comes from a different incident column or event field.
pub(crate) async fn schedule_notification(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    app: &App,
    id: &str,
    service: &str,
    environment: &str,
    severity: i16,
    recovered: bool,
    reopened: bool,
) -> Result<(), sqlx::Error> {
    let (group, priority, _) = app.config.route(service, environment, severity);
    let delay = if priority <= 2 || recovered {
        0.0
    } else {
        app.config.timing.digest_seconds as f64
    };
    sqlx::query("UPDATE outbox SET sent_at=now(),last_error='superseded by state transition' WHERE incident_id=$1 AND sent_at IS NULL AND lease_until IS NULL AND failed_at IS NULL").bind(id).execute(&mut **tx).await?;
    let outbox_id:i64=sqlx::query_scalar("INSERT INTO outbox(incident_id,target,due_at,kind) VALUES($1,$2,CASE WHEN $3::double precision=0 THEN now() ELSE to_timestamp((floor(extract(epoch FROM now())/$3)+1)*$3) END,$4) RETURNING id").bind(id).bind(group).bind(delay).bind(if reopened {"reopen"} else {"update"}).fetch_one(&mut **tx).await?;
    channels::enqueue_incident(tx, &app.config.groups[group], id, outbox_id).await?;
    sqlx::query("UPDATE incidents SET next_summary=now()+$3*interval '1 second',summary_step=0,dirty=false,next_escalation=CASE WHEN $2::double precision IS NULL OR state='resolved' THEN NULL ELSE now()+$2*interval '1 second' END,escalation_step=0 WHERE id=$1").bind(id).bind(app.config.timing.ack(priority).map(|v|v as f64)).bind(app.config.timing.summary(0) as f64).execute(&mut **tx).await?;
    Ok(())
}
async fn list(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>, Failure> {
    let source = authenticate_client(&headers, &app, None).await?;
    let rows:Vec<(Value,)>=sqlx::query_as("SELECT to_jsonb(i) FROM incidents i WHERE ($1::text IS NULL OR source=$1) ORDER BY (state='resolved'),severity,created_at LIMIT 100").bind(source.starts_with("client:").then_some(source)).fetch_all(&app.db).await.map_err(db_error)?;
    Ok(Json(json!(
        rows.into_iter().map(|r| r.0).collect::<Vec<_>>()
    )))
}
async fn detail(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    let source = authenticate_client(&headers, &app, None).await?;
    let row: Option<(Value,)> = sqlx::query_as("SELECT to_jsonb(i) FROM incidents i WHERE id=$1")
        .bind(&id)
        .fetch_optional(&app.db)
        .await
        .map_err(db_error)?;
    let Some((incident,)) = row else {
        return Err((StatusCode::NOT_FOUND, "not found".into()));
    };
    if source.starts_with("client:") && incident["source"].as_str() != Some(source.as_str()) {
        return Err((StatusCode::NOT_FOUND, "incident not found".into()));
    }
    let events: Vec<(Value,)> = sqlx::query_as(
        "SELECT payload || jsonb_build_object('received_at',received_at) FROM events WHERE incident_id=$1 ORDER BY (payload->>'sequence')::bigint",
    )
    .bind(&id)
    .fetch_all(&app.db)
    .await
    .map_err(db_error)?;
    let deliveries: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(o)-'lease_token' FROM outbox o WHERE incident_id=$1 ORDER BY id",
    )
    .bind(&id)
    .fetch_all(&app.db)
    .await
    .map_err(db_error)?;
    let channels:Vec<(Value,)>=sqlx::query_as("SELECT jsonb_build_object('channel',channel,'state',state,'attempts',attempts,'sent_at',sent_at,'last_error',last_error) FROM channel_deliveries WHERE incident_id=$1 ORDER BY id DESC LIMIT 100").bind(&id).fetch_all(&app.db).await.map_err(db_error)?;
    let related:Vec<(String,)>=sqlx::query_as("SELECT DISTINCT e.incident_id FROM events e JOIN events own ON own.incident_id=$1 AND e.payload->>'correlation_id'=own.payload->>'correlation_id' WHERE e.incident_id<>$1 AND ($2::text IS NULL OR e.source=$2)").bind(&id).bind(source.starts_with("client:").then_some(source.as_str())).fetch_all(&app.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"incident":incident,"events":events.into_iter().map(|r|r.0).collect::<Vec<_>>(),"deliveries":deliveries.into_iter().map(|r|r.0).collect::<Vec<_>>(),"channels":channels.into_iter().map(|r|r.0).collect::<Vec<_>>(),"related_incidents":related.into_iter().map(|r|r.0).collect::<Vec<_>>()}),
    ))
}
async fn operations(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    authenticate(&headers, &app)?;
    let (value,):(Value,)=sqlx::query_as("WITH jobs AS (SELECT sent_at,attempts,due_at,(failed_at IS NOT NULL) AS terminal FROM outbox WHERE NOT slack_disabled UNION ALL SELECT sent_at,attempts,due_at,(failed_at IS NOT NULL) FROM gateway_messages WHERE NOT slack_disabled UNION ALL SELECT sent_at,attempts,due_at,(state='failed') FROM channel_deliveries WHERE state IN ('pending','sent','failed')) SELECT jsonb_build_object('pending',count(*) FILTER(WHERE sent_at IS NULL AND NOT terminal),'failed_attempts',coalesce(sum(attempts),0),'oldest_due_at',min(due_at) FILTER(WHERE sent_at IS NULL AND NOT terminal),'failed',count(*) FILTER(WHERE sent_at IS NULL AND terminal)) FROM jobs").fetch_one(&app.db).await.map_err(db_error)?;
    Ok(Json(value))
}
pub(crate) async fn worker_heartbeat(db: &PgPool, worker: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO worker_heartbeats(worker,last_tick) VALUES($1,now()) ON CONFLICT(worker) DO UPDATE SET last_tick=excluded.last_tick").bind(worker).execute(db).await?;
    Ok(())
}
async fn health(State(app): State<Arc<App>>) -> StatusCode {
    if health::workers_alive(&app.db).await {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // The runtime image has no HTTP client, so the container healthcheck calls the binary.
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        let status = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()?
            .get("http://127.0.0.1:8080/health")
            .send()
            .await?
            .status();
        return if status.is_success() {
            Ok(())
        } else {
            Err(format!("health check returned HTTP {}", status.as_u16()).into())
        };
    }
    let db = PgPool::connect(&std::env::var("DATABASE_URL")?).await?;
    sqlx::migrate!().run(&db).await?;
    let mut config: policy::Config = toml::from_str(&std::fs::read_to_string(
        std::env::var("NOTI_CONFIG").unwrap_or_else(|_| "config.toml".into()),
    )?)?;
    if std::env::var("NOTI_DEV_SLACK_LIVE").as_deref() == Ok("1") {
        if std::env::var("NOTI_DEV_MODE").as_deref() != Ok("1") {
            return Err("live Slack test requires development mode".into());
        }
        let token = std::env::var("SLACK_BOT_TOKEN")?;
        let response: Value = reqwest::Client::new()
            .post("https://slack.com/api/auth.test")
            .bearer_auth(&token)
            .send()
            .await?
            .json()
            .await?;
        if response["ok"] != true {
            return Err("Slack auth.test failed; check the bot token".into());
        }
        config.workspace = response["team_id"]
            .as_str()
            .ok_or("Slack workspace ID missing")?
            .to_owned();
        if let Some((channel, user)) = sqlx::query_as::<_, (String, String)>(
            "SELECT channel_id,user_id FROM slack_test_settings WHERE singleton",
        )
        .fetch_optional(&db)
        .await?
        {
            config.users.insert("dev-operator".into(), user);
            config
                .groups
                .get_mut("development")
                .ok_or("missing development group")?
                .channel = channel;
        }
    }
    config.validate()?;
    if config.ai.enabled {
        std::env::var("OPENAI_API_KEY")?;
    }
    let mut source_tokens = Vec::new();
    for (name, source) in &config.sources {
        let token = std::env::var(&source.token_env)?;
        if token.len() < 32 {
            return Err("source tokens require 32 characters".into());
        }
        source_tokens.push((name.clone(), token));
    }
    if let Ok(value) = std::env::var("NOTI_ADMIN_TOKEN")
        && ((!value.is_empty() && value.len() < 32)
            || source_tokens.iter().any(|(_, token)| token == &value))
    {
        return Err(
            "NOTI_ADMIN_TOKEN requires 32 characters and must differ from source tokens".into(),
        );
    }
    let mut admin_tokens = Vec::new();
    for (name, admin) in &config.admins {
        let value = std::env::var(&admin.token_env)?;
        if value.len() < 32
            || source_tokens.iter().any(|(_, token)| token == &value)
            || admin_tokens.iter().any(|(_, token, _)| token == &value)
            || std::env::var("NOTI_ADMIN_TOKEN").is_ok_and(|token| token == value)
        {
            return Err("named admin tokens require 32 characters and must be distinct".into());
        }
        admin_tokens.push((name.clone(), value, admin.role.clone()));
    }
    let slack_required = config.groups.values().any(|group| group.slack_enabled);
    let signing_secret = if slack_required {
        std::env::var(&config.signing_secret_env)?
    } else {
        String::new()
    };
    let slack_token = if slack_required {
        std::env::var("SLACK_BOT_TOKEN")?
    } else {
        String::new()
    };
    if slack_required
        && (slack_token.trim().is_empty()
            || signing_secret.trim().is_empty()
            || config.workspace.trim().is_empty())
    {
        return Err("Slack token, signing secret, and workspace are required".into());
    }

    let development_mode = std::env::var("NOTI_DEV_MODE").as_deref() == Ok("1");
    let mock_delivery = std::env::var("NOTI_LOCAL_MOCKS").as_deref() == Ok("1");
    if mock_delivery {
        let slack =
            reqwest::Url::parse(&config.api_url).map_err(|_| "invalid development Slack URL")?;
        let email_local = config.channels.email.as_ref().is_none_or(|email| {
            matches!(email.security, policy::SmtpSecurity::Local)
                && matches!(email.host.as_str(), "127.0.0.1" | "localhost" | "::1")
        });
        let telegram_local = config.channels.telegram.as_ref().is_none_or(|telegram| {
            reqwest::Url::parse(&telegram.api_base).is_ok_and(|url| {
                url.scheme() == "http"
                    && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            })
        });
        if !development_mode
            || slack.scheme() != "http"
            || slack.host_str() != Some("slack")
            || !email_local
            || !telegram_local
        {
            return Err("local mock mode requires development and local provider endpoints".into());
        }
    }
    let channel_providers = Arc::new(channels::Providers::configure(
        &config.channels,
        development_mode,
    )?);
    let app = Arc::new(App {
        db,
        channel_providers,
        source_tokens,
        signing_secret,
        slack_token,
        ai_token: std::env::var("OPENAI_API_KEY").ok(),
        admin_tokens,
        development_mode,
        mock_delivery,
        delivery_health: Arc::new(health::DeliveryHealth::default()),
        slack_check_enabled: slack_required,
        admin_token: std::env::var("NOTI_ADMIN_TOKEN")
            .ok()
            .filter(|s| !s.is_empty()),
        config,
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?,
    });
    channels::reconcile(&app).await?;
    if let Ok(token) = std::env::var("SLACK_APP_TOKEN")
        && !token.is_empty()
    {
        tokio::spawn(socket_mode::worker(app.clone(), token));
    }
    if app.slack_check_enabled {
        tokio::spawn(health::slack_worker(app.clone()));
    }
    tokio::spawn(workflow::worker(app.clone()));
    tokio::spawn(messages::worker(app.clone()));
    tokio::spawn(channels::worker(app.clone()));
    if app.config.ai.enabled {
        tokio::spawn(workflow::ai_worker(app.clone()));
    }
    let router = Router::new()
        .route("/v1/events", post(ingest))
        .route("/v1/client", get(client_identity))
        .route("/v1/messages", post(messages::ingest))
        .route("/v1/messages/{id}", get(messages::status))
        .route("/hooks/slack/{token}", post(slack_hooks::ingest))
        .route("/hooks/alertmanager", post(alertmanager::ingest))
        .route("/hooks/uptime-kuma", post(monitors::uptime_kuma))
        .route("/hooks/sentry/{token}", post(monitors::sentry))
        .route("/hooks/datadog", post(monitors::datadog))
        .route(
            "/hooks/grafana",
            post(alertmanager::ingest).put(alertmanager::ingest),
        )
        .route("/v1/incidents", get(list))
        .route("/v1/incidents/{id}", get(detail))
        .route("/health", get(health))
        .route("/health/delivery", get(health::delivery))
        .route("/v1/operations", get(operations))
        .route("/v1/slack/actions", post(workflow::action))
        .merge(admin::routes())
        .with_state(app);
    axum::serve(tokio::net::TcpListener::bind("0.0.0.0:8080").await?, router).await?;
    Ok(())
}

#[cfg(test)]
#[path = "tests/integration.rs"]
mod integration;
#[cfg(test)]
#[path = "tests/support.rs"]
mod test_support;
