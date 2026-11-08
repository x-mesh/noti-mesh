use crate::{App, Failure, db_error};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::Row;
use std::sync::Arc;

pub(crate) struct Principal {
    kind: &'static str,
    actor: Option<String>,
    role: &'static str,
}
impl Principal {
    /// Names the administrator in audit columns; shared tokens have no name.
    pub(crate) fn label(&self) -> String {
        self.actor.clone().unwrap_or_else(|| self.kind.to_owned())
    }
}
fn identify(headers: &HeaderMap, app: &App) -> Result<Principal, Failure> {
    if app.development_mode {
        return Ok(Principal {
            kind: "development",
            actor: None,
            role: "admin",
        });
    }
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let matches = |expected: &str| {
        let mut key = Hmac::<Sha256>::new_from_slice(expected.as_bytes())
            .expect("HMAC accepts all key lengths");
        key.update(b"admin");
        let mut candidate = Hmac::<Sha256>::new_from_slice(supplied.as_bytes())
            .expect("HMAC accepts all key lengths");
        candidate.update(b"admin");
        candidate.verify_slice(&key.finalize().into_bytes()).is_ok()
    };
    for (name, token, role) in &app.admin_tokens {
        if matches(token) {
            return Ok(Principal {
                kind: "named_admin",
                actor: Some(name.clone()),
                role: role.as_str(),
            });
        }
    }
    if let Some(token) = app.admin_token.as_deref()
        && matches(token)
    {
        return Ok(Principal {
            kind: "shared_admin",
            actor: None,
            role: "admin",
        });
    }
    if app.admin_token.is_none() && app.admin_tokens.is_empty() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "admin access is not configured".into(),
        ));
    }
    Err((StatusCode::UNAUTHORIZED, "invalid admin token".into()))
}
pub fn authorize(headers: &HeaderMap, app: &App) -> Result<(), Failure> {
    identify(headers, app).map(|_| ())
}
pub(crate) fn authorize_write(headers: &HeaderMap, app: &App) -> Result<Principal, Failure> {
    let principal = identify(headers, app)?;
    if principal.role != "admin" {
        return Err((StatusCode::FORBIDDEN, "read-only administrator".into()));
    }
    Ok(principal)
}
fn asset(content_type: &'static str, content: &'static str) -> Response {
    ([(header::CONTENT_TYPE,content_type),(header::CACHE_CONTROL,"no-store"),(header::X_CONTENT_TYPE_OPTIONS,"nosniff"),(header::CONTENT_SECURITY_POLICY,"default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")],content).into_response()
}
/// Serves the API reference page.
#[derive(Deserialize)]
struct DocQuery {
    lang: Option<String>,
}
/// English for `?lang=en`, or without `lang` when the browser does not prefer Korean.
fn english_docs(query: &DocQuery, headers: &HeaderMap) -> bool {
    match query.lang.as_deref() {
        Some(lang) => lang == "en",
        None => headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_ascii_lowercase())
            .is_some_and(|v| !v.is_empty() && !v.starts_with("ko")),
    }
}
/// The English document replaces each Korean string with its entry in `openapi.en.json`.
fn openapi_english() -> &'static str {
    static DOCUMENT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DOCUMENT.get_or_init(|| {
        let words: std::collections::HashMap<String, String> =
            serde_json::from_str(include_str!("../web/openapi.en.json"))
                .expect("openapi.en.json is a string map");
        fn translate(value: &mut Value, words: &std::collections::HashMap<String, String>) {
            match value {
                Value::String(text) => {
                    if let Some(english) = words.get(text.as_str()) {
                        *text = english.clone();
                    }
                }
                Value::Array(items) => items.iter_mut().for_each(|v| translate(v, words)),
                Value::Object(map) => map.values_mut().for_each(|v| translate(v, words)),
                _ => {}
            }
        }
        let mut document: Value = serde_json::from_str(include_str!("../web/openapi.json"))
            .expect("openapi.json is valid JSON");
        translate(&mut document, &words);
        serde_json::to_string_pretty(&document).expect("JSON serializes")
    })
}
async fn openapi_document(Query(query): Query<DocQuery>, headers: HeaderMap) -> Response {
    if english_docs(&query, &headers) {
        asset("application/json", openapi_english())
    } else {
        asset("application/json", include_str!("../web/openapi.json"))
    }
}
async fn api_reference(
    State(app): State<Arc<App>>,
    Query(query): Query<DocQuery>,
    headers: HeaderMap,
) -> Response {
    let lang = if english_docs(&query, &headers) {
        "en"
    } else {
        "ko"
    };
    let mut config = json!({
        "url":format!("/openapi.json?lang={lang}"),
        "withDefaultFonts":false,
        "telemetry":false,
        "hideClientButton":true,
        "showDeveloperTools":"never",
        "agent":{"disabled":true},
        "mcp":{"disabled":true},
        "authentication":{"preferredSecurityScheme":"bearerAuth"},
        // Scalar 1.75.1 has no option for the hosted SDK upsell; this is its GenerateSdkButton scope id.
        "customCss":"[data-v-c400330d]{display:none!important}"
    });
    // Development mode already disables admin authentication, so the local source token can be prefilled.
    if app.development_mode
        && let Some((_, token)) = app.source_tokens.first()
    {
        config["authentication"]["securitySchemes"] = json!({"bearerAuth":{"token":token}});
    }
    let attribute = config
        .to_string()
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let page = format!(
        "<!doctype html><html lang=\"{lang}\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"><link rel=\"icon\" href=\"data:,\"><title>API · noti-mesh</title></head><body><script id=\"api-reference\" type=\"application/json\" data-configuration=\"{attribute}\"></script><script src=\"/admin/scalar.js\"></script></body></html>"
    );
    // Scalar sets inline styles without a nonce; scripts and connections stay limited to this origin.
    let policy = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8".to_owned()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CONTENT_SECURITY_POLICY, policy.to_owned()),
        ],
        page,
    )
        .into_response()
}
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/admin/api", get(api_reference))
        .route("/openapi.json", get(openapi_document))
        .route(
            "/admin/scalar.js",
            get(|| async {
                asset(
                    "text/javascript",
                    include_str!("../web/vendor/scalar/standalone.js"),
                )
            }),
        )
        .route(
            "/admin/scalar-license",
            get(|| async {
                asset(
                    "text/plain; charset=utf-8",
                    include_str!("../web/vendor/scalar/LICENSE"),
                )
            }),
        )
        .route(
            "/",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_str!("../web/index.html"),
                )
            }),
        )
        .route(
            "/admin",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_str!("../web/index.html"),
                )
            }),
        )
        .route(
            "/admin/app.css",
            get(|| async { asset("text/css", include_str!("../web/app.css")) }),
        )
        .route(
            "/admin/app.js",
            get(|| async { asset("text/javascript", include_str!("../web/app.js")) }),
        )
        .route(
            "/admin/theme.js",
            get(|| async { asset("text/javascript", include_str!("../web/theme.js")) }),
        )
        .route(
            "/admin/i18n.js",
            get(|| async { asset("text/javascript", include_str!("../web/i18n.js")) }),
        )
        .route(
            "/admin/kit.css",
            get(|| async { asset("text/css", include_str!("../web/vendor/kit.css")) }),
        )
        .route(
            "/admin/kit.js",
            get(|| async { asset("text/javascript", include_str!("../web/vendor/kit.js")) }),
        )
        .route(
            "/admin/jkit-license",
            get(|| async {
                asset(
                    "text/plain; charset=utf-8",
                    include_str!("../web/vendor/LICENSE"),
                )
            }),
        )
        .route(
            "/v1/admin-mode",
            get(|State(app): State<Arc<App>>| async move {
                Json(json!({"development":app.development_mode}))
            }),
        )
        .route("/v1/admin/messages", get(crate::messages::list))
        .route(
            "/v1/admin/channel-deliveries/{id}/retry",
            post(retry_channel),
        )
        .route(
            "/v1/admin/slack-deliveries/{id}/retry",
            post(retry_slack_incident),
        )
        .route("/v1/admin/messages/{id}/retry", post(retry_slack_message))
        .route("/v1/admin/retries", get(retry_history))
        .route("/v1/admin/flows", get(flows))
        .route(
            "/v1/admin/slack-test",
            get(slack_test).post(save_slack_test),
        )
        .route("/v1/admin/tokens", get(tokens).post(create_token))
        .route("/v1/admin/tokens/{id}/revoke", post(revoke_token))
        .route("/v1/admin/tokens/{id}/rotate", post(rotate_token))
        .route("/v1/admin/tokens/{id}/activity", get(token_activity))
        .route("/v1/admin", get(overview))
        .route("/v1/admin/incidents", get(incidents))
        .route("/v1/admin/incidents/{id}", get(detail))
        .route(
            "/v1/admin/incidents/{id}/report",
            get(crate::report::incident),
        )
        .route("/v1/admin/incidents/{id}/ack", post(ack))
        .route(
            "/v1/admin/silences",
            get(crate::silence::list).post(crate::silence::create),
        )
        .route(
            "/v1/admin/silences/{id}/cancel",
            post(crate::silence::cancel),
        )
        .route("/v1/admin/evidence.csv", get(crate::evidence::export))
}
async fn slack_items(
    app: &App,
    method: &str,
    field: &str,
    types: Option<&str>,
) -> Result<Vec<Value>, Failure> {
    let mut cursor = String::new();
    let mut items = Vec::new();
    loop {
        let mut request = app
            .http
            .get(format!("{}/{}", app.config.api_url, method))
            .bearer_auth(&app.slack_token)
            .query(&[("limit", "200"), ("cursor", cursor.as_str())]);
        if let Some(types) = types {
            request = request.query(&[("types", types), ("exclude_archived", "true")]);
        }
        let data: Value = request
            .send()
            .await
            .map_err(|_| (StatusCode::BAD_GATEWAY, "Slack request failed".into()))?
            .json()
            .await
            .map_err(|_| {
                (
                    StatusCode::BAD_GATEWAY,
                    "Slack returned invalid data".into(),
                )
            })?;
        if data["ok"] != true {
            let code = data["error"].as_str().unwrap_or("unknown_error");
            return Err((StatusCode::BAD_GATEWAY, format!("Slack: {code}")));
        }
        items.extend(data[field].as_array().cloned().unwrap_or_default());
        cursor = data["response_metadata"]["next_cursor"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        if cursor.is_empty() {
            break;
        }
    }
    Ok(items)
}
async fn slack_test_channels(app: &App) -> Result<(Vec<Value>, bool), Failure> {
    let mut channels = slack_items(
        app,
        "users.conversations",
        "channels",
        Some("public_channel"),
    )
    .await?;
    let private_available = match slack_items(
        app,
        "users.conversations",
        "channels",
        Some("private_channel"),
    )
    .await
    {
        Ok(private) => {
            channels.extend(private);
            true
        }
        Err((_, message)) if message == "Slack: missing_scope" => false,
        Err(error) => return Err(error),
    };
    Ok((channels, private_available))
}
fn slack_test_allowed(app: &App) -> Result<(), Failure> {
    if !app.development_mode || std::env::var("NOTI_DEV_SLACK_LIVE").as_deref() != Ok("1") {
        return Err((StatusCode::NOT_FOUND, "Slack test mode unavailable".into()));
    }
    Ok(())
}
async fn slack_test(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    authorize(&headers, &app)?;
    slack_test_allowed(&app)?;
    let (channels, private_available) = slack_test_channels(&app).await?;
    let users = slack_items(&app, "users.list", "members", None).await?;
    let channels: Vec<_> = channels
        .iter()
        .map(|c| json!({"id":c["id"],"name":c["name"]}))
        .collect();
    let users: Vec<_> = users.iter().filter(|u| u["deleted"] != true && u["is_bot"] != true && u["id"].as_str().is_some()).map(|u| json!({"id":u["id"],"name":u["real_name"].as_str().or_else(||u["name"].as_str()).unwrap_or("")})).collect();
    Ok(Json(
        json!({"workspace":app.config.workspace,"private_available":private_available,"channels":channels,"users":users,"selected_channel":app.config.groups["development"].channel,"selected_user":app.config.users["dev-operator"]}),
    ))
}
#[derive(Deserialize)]
struct SlackTestSelection {
    channel_id: String,
    user_id: String,
}
async fn save_slack_test(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(selection): Json<SlackTestSelection>,
) -> Result<StatusCode, Failure> {
    authorize_write(&headers, &app)?;
    slack_test_allowed(&app)?;
    let (channels, _) = slack_test_channels(&app).await?;
    let users = slack_items(&app, "users.list", "members", None).await?;
    if !channels.iter().any(|c| c["id"] == selection.channel_id)
        || !users
            .iter()
            .any(|u| u["id"] == selection.user_id && u["deleted"] != true && u["is_bot"] != true)
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid Slack channel or user".into(),
        ));
    }
    sqlx::query("INSERT INTO slack_test_settings(singleton,channel_id,user_id) VALUES(true,$1,$2) ON CONFLICT(singleton) DO UPDATE SET channel_id=excluded.channel_id,user_id=excluded.user_id")
        .bind(&selection.channel_id).bind(&selection.user_id).execute(&app.db).await.map_err(db_error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn overview(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>, Failure> {
    let principal = identify(&headers, &app)?;
    let (counts,):(Value,)=sqlx::query_as("SELECT jsonb_build_object('open',count(*) FILTER(WHERE state='open'),'acknowledged',count(*) FILTER(WHERE state='acknowledged'),'resolved',count(*) FILTER(WHERE state='resolved'),'urgent',count(*) FILTER(WHERE severity<=2 AND state<>'resolved')) FROM incidents").fetch_one(&app.db).await.map_err(db_error)?;
    let (delivery,):(Value,)=sqlx::query_as("WITH jobs AS (SELECT sent_at,attempts,due_at,(failed_at IS NOT NULL) AS terminal FROM outbox WHERE NOT slack_disabled UNION ALL SELECT sent_at,attempts,due_at,(failed_at IS NOT NULL) FROM gateway_messages WHERE NOT slack_disabled UNION ALL SELECT sent_at,attempts,due_at,(state='failed') FROM channel_deliveries WHERE state IN ('pending','sent','failed')) SELECT jsonb_build_object('pending',count(*) FILTER(WHERE sent_at IS NULL AND NOT terminal),'retrying',count(*) FILTER(WHERE sent_at IS NULL AND NOT terminal AND attempts>0),'oldest_due_at',min(due_at) FILTER(WHERE sent_at IS NULL AND NOT terminal),'standby',(SELECT count(*) FROM channel_deliveries WHERE state='standby'),'failed',count(*) FILTER(WHERE sent_at IS NULL AND terminal)) FROM jobs").fetch_one(&app.db).await.map_err(db_error)?;
    // Incident events keep their source severity 1-4; ordinary messages map their level onto the same scale.
    let (levels,):(Value,)=sqlx::query_as("WITH received AS (SELECT (payload->>'severity')::int AS rank,received_at AS at FROM events WHERE received_at>now()-interval '24 hours' UNION ALL SELECT CASE level WHEN 'crit' THEN 1 WHEN 'error' THEN 2 WHEN 'warn' THEN 3 ELSE 4 END,created_at FROM gateway_messages WHERE created_at>now()-interval '24 hours' UNION ALL SELECT CASE m.level WHEN 'crit' THEN 1 WHEN 'error' THEN 2 WHEN 'warn' THEN 3 ELSE 4 END,d.received_at FROM message_duplicates d JOIN gateway_messages m ON m.id=d.message_id WHERE d.received_at>now()-interval '24 hours') SELECT jsonb_agg(jsonb_build_object('level',(ARRAY['crit','error','warn','info'])[r.rank],'last_hour',(SELECT count(*) FROM received x WHERE x.rank=r.rank AND x.at>now()-interval '1 hour'),'last_day',(SELECT count(*) FROM received x WHERE x.rank=r.rank),'open',(SELECT count(*) FROM incidents i WHERE i.severity=r.rank AND i.state<>'resolved'),'unacknowledged',(SELECT count(*) FROM incidents i WHERE i.severity=r.rank AND i.state='open')) ORDER BY r.rank) FROM generate_series(1,4) AS r(rank)").fetch_one(&app.db).await.map_err(db_error)?;
    let groups:Vec<Value>=app.config.groups.iter().map(|(name,g)|json!({"name":name,"channel":g.channel,"slack_enabled":g.slack_enabled,"members":g.members,"backup":g.backup,"destinations":g.destinations.iter().map(|d|json!({"channel":d.channel.as_str(),"address":d.address,"mode":d.mode.as_str()})).collect::<Vec<_>>()})).collect();
    let users: Vec<Value> = app
        .config
        .users
        .iter()
        .map(|(name, id)| json!({"name":name,"id":id}))
        .collect();
    let routes:Vec<Value>=app.config.routes.iter().map(|r|json!({"service":r.service,"environment":r.environment,"group":r.group,"priority_floor":r.priority_floor})).collect();
    Ok(Json(
        json!({"administrator":{"name":principal.actor,"role":principal.role,"identity":principal.kind},"mock_delivery":app.mock_delivery,"slack_test":app.development_mode && std::env::var("NOTI_DEV_SLACK_LIVE").as_deref()==Ok("1"),"counts":counts,"levels":levels,"delivery":delivery,"groups":groups,"users":users,"routes":routes,"default_group":app.config.default_group,"ai_enabled":app.config.ai.enabled,"timing":{"summary_seconds":app.config.timing.summary_seconds,"digest_seconds":app.config.timing.digest_seconds,"p1_ack_seconds":app.config.timing.p1_ack_seconds,"p2_ack_seconds":app.config.timing.p2_ack_seconds,"escalation_seconds":app.config.timing.escalation_seconds}}),
    ))
}
#[derive(Default, Deserialize)]
struct Filter {
    #[serde(default)]
    q: String,
    #[serde(default)]
    state: String,
    priority: Option<i16>,
    #[serde(default)]
    offset: i64,
}
async fn incidents(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(filter): Query<Filter>,
) -> Result<Json<Value>, Failure> {
    authorize(&headers, &app)?;
    if filter.offset < 0
        || filter.q.len() > 256
        || filter.priority.is_some_and(|p| !(1..=4).contains(&p))
        || !["", "active", "open", "acknowledged", "resolved"].contains(&filter.state.as_str())
    {
        return Err((StatusCode::UNPROCESSABLE_ENTITY, "invalid filter".into()));
    }
    let rows:Vec<(Value,)>=sqlx::query_as("SELECT to_jsonb(i) FROM incidents i WHERE ($1='' OR strpos(lower(service),lower($1))>0 OR strpos(lower(incident_key),lower($1))>0 OR strpos(lower(environment),lower($1))>0) AND ($2='' OR state=$2 OR ($2='active' AND state<>'resolved')) AND ($3::smallint IS NULL OR severity=$3) ORDER BY (state='resolved'),severity,created_at,id LIMIT 51 OFFSET $4").bind(filter.q).bind(filter.state).bind(filter.priority).bind(filter.offset).fetch_all(&app.db).await.map_err(db_error)?;
    let more = rows.len() > 50;
    Ok(Json(
        json!({"items":rows.into_iter().take(50).map(|r|r.0).collect::<Vec<_>>(),"has_more":more}),
    ))
}
async fn detail(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    authorize(&headers, &app)?;
    let (incident,): (Value,) = sqlx::query_as("SELECT to_jsonb(i)||jsonb_build_object('source_name',coalesce((SELECT t.name FROM client_tokens t WHERE 'client:'||t.id=i.source),i.source)) FROM incidents i WHERE id=$1")
        .bind(&id)
        .fetch_optional(&app.db)
        .await
        .map_err(db_error)?
        .ok_or((StatusCode::NOT_FOUND, "incident not found".into()))?;
    let events:Vec<(Value,)>=sqlx::query_as("SELECT payload || jsonb_build_object('received_at',received_at) FROM events WHERE incident_id=$1 ORDER BY (payload->>'sequence')::bigint DESC LIMIT 100").bind(&id).fetch_all(&app.db).await.map_err(db_error)?;
    let deliveries:Vec<(Value,)>=sqlx::query_as("SELECT to_jsonb(o)-'lease_token' FROM outbox o WHERE incident_id=$1 ORDER BY id DESC LIMIT 100").bind(&id).fetch_all(&app.db).await.map_err(db_error)?;
    let actions: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(a) FROM actions a WHERE incident_id=$1 ORDER BY created_at DESC LIMIT 100",
    )
    .bind(&id)
    .fetch_all(&app.db)
    .await
    .map_err(db_error)?;
    let confirmations:Vec<(Value,)>=sqlx::query_as("SELECT to_jsonb(c) FROM confirmations c WHERE incident_id=$1 ORDER BY created_at DESC LIMIT 100").bind(&id).fetch_all(&app.db).await.map_err(db_error)?;
    let channels:Vec<(Value,)>=sqlx::query_as("SELECT to_jsonb(c)-'lease_token' FROM channel_deliveries c WHERE incident_id=$1 ORDER BY id DESC LIMIT 100").bind(&id).fetch_all(&app.db).await.map_err(db_error)?;
    let related:Vec<(Value,)>=sqlx::query_as("SELECT DISTINCT to_jsonb(i) FROM incidents i JOIN events e ON e.incident_id=i.id JOIN events own ON own.incident_id=$1 AND own.payload->>'correlation_id'=e.payload->>'correlation_id' WHERE i.id<>$1 LIMIT 20").bind(&id).fetch_all(&app.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"incident":incident,"events":events.into_iter().map(|r|r.0).collect::<Vec<_>>(),"deliveries":deliveries.into_iter().map(|r|r.0).collect::<Vec<_>>(),"actions":actions.into_iter().map(|r|r.0).collect::<Vec<_>>(),"confirmations":confirmations.into_iter().map(|r|r.0).collect::<Vec<_>>(),"channels":channels.into_iter().map(|r|r.0).collect::<Vec<_>>(),"related":related.into_iter().map(|r|r.0).collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize)]
struct Assignment {
    actor: String,
    sequence: i64,
}
async fn ack(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<Assignment>,
) -> Result<Json<Value>, Failure> {
    let principal = authorize_write(&headers, &app)?;
    let actor = app
        .config
        .users
        .get(&input.actor)
        .ok_or((StatusCode::UNPROCESSABLE_ENTITY, "unknown user".into()))?;
    let mut tx = app.db.begin().await.map_err(db_error)?;
    let row = sqlx::query("SELECT * FROM incidents WHERE id=$1 FOR UPDATE")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_error)?
        .ok_or((StatusCode::NOT_FOUND, "incident not found".into()))?;
    let (group, _, _) = app.config.route(
        row.get("service"),
        row.get("environment"),
        row.get("severity"),
    );
    if ![
        group,
        app.config.groups[group].backup.as_str(),
        app.config.default_group.as_str(),
    ]
    .iter()
    .any(|g| app.config.groups[*g].members.contains(&input.actor))
    {
        return Err((
            StatusCode::FORBIDDEN,
            "user is not a responsible group member".into(),
        ));
    }
    if row.get::<String, _>("state") != "open" || row.get::<i64, _>("sequence") != input.sequence {
        return Err((
            StatusCode::CONFLICT,
            "incident changed; refresh before assignment".into(),
        ));
    }
    sqlx::query(
        "UPDATE incidents SET state='acknowledged',owner=$2,next_escalation=NULL WHERE id=$1",
    )
    .bind(&id)
    .bind(actor)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    sqlx::query(
        "INSERT INTO actions(id,incident_id,actor,initiator_kind,initiator) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(format!("admin:{}", uuid::Uuid::new_v4()))
    .bind(&id)
    .bind(actor)
    .bind(principal.kind)
    .bind(&principal.actor)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    sqlx::query("UPDATE outbox SET sent_at=now(),last_error='cancelled by acknowledgment' WHERE incident_id=$1 AND kind='escalation' AND sent_at IS NULL AND lease_until IS NULL").bind(&id).execute(&mut *tx).await.map_err(db_error)?;
    let outbox_id: i64 =
        sqlx::query_scalar("INSERT INTO outbox(incident_id,target) VALUES($1,$2) RETURNING id")
            .bind(&id)
            .bind(group)
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
    crate::channels::enqueue_incident(&mut tx, &app.config.groups[group], &id, outbox_id)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(Json(
        json!({"incident_id":id,"owner":actor,"state":"acknowledged"}),
    ))
}

async fn tokens(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>, Failure> {
    authorize(&headers, &app)?;
    let rows: Vec<(Value,)> = sqlx::query_as(
        "SELECT to_jsonb(t)-'token_hash'||jsonb_build_object('audit',(SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY a.created_at DESC),'[]'::jsonb) FROM client_token_audit a WHERE a.token_id=t.id)) FROM client_tokens t ORDER BY created_at DESC LIMIT 200",
    )
    .fetch_all(&app.db)
    .await
    .map_err(db_error)?;
    Ok(Json(
        json!({"items":rows.into_iter().map(|r|r.0).collect::<Vec<_>>()}),
    ))
}
#[derive(Deserialize)]
struct NewToken {
    name: String,
    service: String,
}
async fn create_token(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(input): Json<NewToken>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), Failure> {
    let principal = authorize_write(&headers, &app)?;
    if input.name.trim().is_empty()
        || input.name.len() > 128
        || input.service.trim().is_empty()
        || input.service.len() > 256
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid client name or service".into(),
        ));
    }
    use sha2::{Digest, Sha256};
    let id = uuid::Uuid::new_v4().to_string();
    let token = format!(
        "nm_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let mut tx = app.db.begin().await.map_err(db_error)?;
    sqlx::query("INSERT INTO client_tokens(id,name,service,token_hash) VALUES($1,$2,$3,$4)")
        .bind(&id)
        .bind(input.name.trim())
        .bind(input.service.trim())
        .bind(hash)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    sqlx::query(
        "INSERT INTO client_token_audit(token_id,action,actor_kind,actor) VALUES($1,'created',$2,$3)",
    )
    .bind(&id)
    .bind(principal.kind)
    .bind(&principal.actor)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("valid header"),
    );
    Ok((
        StatusCode::CREATED,
        response_headers,
        Json(json!({"id":id,"token":token})),
    ))
}
async fn revoke_token(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let principal = authorize_write(&headers, &app)?;
    let mut tx = app.db.begin().await.map_err(db_error)?;
    let result =
        sqlx::query("UPDATE client_tokens SET revoked_at=now() WHERE id=$1 AND revoked_at IS NULL")
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "active token not found".into()));
    }
    sqlx::query(
        "INSERT INTO client_token_audit(token_id,action,actor_kind,actor) VALUES($1,'revoked',$2,$3)",
    )
    .bind(&id)
    .bind(principal.kind)
    .bind(&principal.actor)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rotate_token(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<(StatusCode, HeaderMap, Json<Value>), Failure> {
    let principal = authorize_write(&headers, &app)?;
    use sha2::{Digest, Sha256};
    let token = format!(
        "nm_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let hash = hex::encode(Sha256::digest(token.as_bytes()));
    let mut tx = app.db.begin().await.map_err(db_error)?;
    let result=sqlx::query("UPDATE client_tokens SET token_hash=$2,rotated_at=now(),last_used_at=NULL WHERE id=$1 AND revoked_at IS NULL").bind(&id).bind(hash).execute(&mut *tx).await.map_err(db_error)?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::NOT_FOUND, "active token not found".into()));
    }
    sqlx::query(
        "INSERT INTO client_token_audit(token_id,action,actor_kind,actor) VALUES($1,'rotated',$2,$3)",
    )
    .bind(&id)
    .bind(principal.kind)
    .bind(&principal.actor)
    .execute(&mut *tx)
    .await
    .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CACHE_CONTROL,
        "no-store".parse().expect("valid header"),
    );
    Ok((
        StatusCode::OK,
        response_headers,
        Json(json!({"id":id,"token":token})),
    ))
}

async fn token_activity(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    authorize(&headers, &app)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM client_tokens WHERE id=$1)")
        .bind(&id)
        .fetch_one(&app.db)
        .await
        .map_err(db_error)?;
    if !exists {
        return Err((StatusCode::NOT_FOUND, "client not found".into()));
    }
    let source = format!("client:{id}");
    let rows:Vec<(Value,)>=sqlx::query_as("WITH items AS (SELECT 'event'::text AS kind,e.event_id AS event_id,e.incident_id AS reference_id,i.incident_key AS title,e.received_at,e.payload->>'occurred_at' AS occurred_at FROM events e JOIN incidents i ON i.id=e.incident_id WHERE e.source=$1 UNION ALL SELECT 'message'::text,m.event_id,m.id,m.payload->>'title',m.created_at,NULL::text FROM gateway_messages m WHERE m.source=$1) SELECT jsonb_build_object('kind',kind,'event_id',event_id,'reference_id',reference_id,'title',title,'received_at',received_at,'occurred_at',occurred_at) FROM items ORDER BY received_at DESC NULLS LAST,event_id DESC LIMIT 100").bind(source).fetch_all(&app.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"items":rows.into_iter().map(|r|r.0).collect::<Vec<_>>()}),
    ))
}
async fn record_retry(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    kind: &str,
    id: &str,
    principal: &Principal,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO delivery_retry_audit(target_kind,target_id,actor_kind,actor) VALUES($1,$2,$3,$4)").bind(kind).bind(id).bind(principal.kind).bind(&principal.actor).execute(&mut **tx).await?;
    Ok(())
}
async fn retry_channel(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, Failure> {
    let principal = authorize_write(&headers, &app)?;
    let mut tx = app.db.begin().await.map_err(db_error)?;
    let result=sqlx::query("UPDATE channel_deliveries SET state='pending',due_at=now(),lease_until=NULL,lease_token=NULL WHERE id=$1 AND state='failed'").bind(id).execute(&mut *tx).await.map_err(db_error)?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::CONFLICT, "delivery is not failed".into()));
    }
    record_retry(&mut tx, "channel", &id.to_string(), &principal)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn retry_slack_incident(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Result<StatusCode, Failure> {
    let principal = authorize_write(&headers, &app)?;
    let mut tx = app.db.begin().await.map_err(db_error)?;
    let result=sqlx::query("UPDATE outbox SET failed_at=NULL,due_at=now(),lease_until=NULL,lease_token=NULL WHERE id=$1 AND failed_at IS NOT NULL AND sent_at IS NULL AND NOT slack_disabled").bind(id).execute(&mut *tx).await.map_err(db_error)?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::CONFLICT, "Slack delivery is not failed".into()));
    }
    record_retry(&mut tx, "slack_incident", &id.to_string(), &principal)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn retry_slack_message(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, Failure> {
    let principal = authorize_write(&headers, &app)?;
    let mut tx = app.db.begin().await.map_err(db_error)?;
    let result=sqlx::query("UPDATE gateway_messages SET failed_at=NULL,due_at=now(),lease_until=NULL,lease_token=NULL WHERE id=$1 AND failed_at IS NOT NULL AND sent_at IS NULL AND NOT slack_disabled").bind(&id).execute(&mut *tx).await.map_err(db_error)?;
    if result.rows_affected() == 0 {
        return Err((StatusCode::CONFLICT, "Slack delivery is not failed".into()));
    }
    record_retry(&mut tx, "slack_message", &id, &principal)
        .await
        .map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn retry_history(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
) -> Result<Json<Value>, Failure> {
    authorize(&headers, &app)?;
    let rows: Vec<(Value,)> =
        sqlx::query_as("SELECT to_jsonb(a) FROM delivery_retry_audit a ORDER BY id DESC LIMIT 100")
            .fetch_all(&app.db)
            .await
            .map_err(db_error)?;
    Ok(Json(
        json!({"items":rows.into_iter().map(|r|r.0).collect::<Vec<_>>()}),
    ))
}

async fn flows(State(app): State<Arc<App>>, headers: HeaderMap) -> Result<Json<Value>, Failure> {
    authorize(&headers, &app)?;
    let rows:Vec<(Value,)>=sqlx::query_as("SELECT jsonb_build_object('incident_id',i.id,'incident_key',i.incident_key,'source_name',coalesce((SELECT t.name FROM client_tokens t WHERE 'client:'||t.id=i.source),i.source),'service',i.service,'severity',i.severity,'state',i.state,'delivery',to_jsonb(o)-'lease_token','confirmations',(SELECT count(*) FROM confirmations c WHERE c.incident_id=i.id AND c.channel=o.provider_channel AND c.message_ts=o.provider_message_ts),'channels',(SELECT coalesce(jsonb_agg(to_jsonb(d)-'lease_token' ORDER BY d.id),'[]'::jsonb) FROM channel_deliveries d WHERE d.parent_outbox_id=o.id)) FROM outbox o JOIN incidents i ON i.id=o.incident_id ORDER BY o.id DESC LIMIT 100").fetch_all(&app.db).await.map_err(db_error)?;
    let channels:Vec<(Value,)>=sqlx::query_as("SELECT jsonb_build_object('delivery',to_jsonb(c)-'lease_token','kind',CASE WHEN c.incident_id IS NULL THEN 'message' ELSE 'incident' END,'incident_id',c.incident_id,'title',coalesce(i.incident_key,m.payload->>'title'),'service',coalesce(i.service,m.payload->>'service'),'source_name',coalesce((SELECT t.name FROM client_tokens t WHERE 'client:'||t.id=coalesce(i.source,m.source)),i.source,m.source)) FROM channel_deliveries c LEFT JOIN incidents i ON i.id=c.incident_id LEFT JOIN gateway_messages m ON m.id=c.message_id ORDER BY c.id DESC LIMIT 100").fetch_all(&app.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"items":rows.into_iter().map(|r|r.0).collect::<Vec<_>>(),"channels":channels.into_iter().map(|r|r.0).collect::<Vec<_>>()}),
    ))
}
