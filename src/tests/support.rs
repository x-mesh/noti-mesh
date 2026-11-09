use crate::{App, admin, client_identity, detail, ingest, list, messages, policy};
use axum::{
    Router,
    routing::{get, post},
};
use sqlx::PgPool;
use std::sync::Arc;

pub(crate) const ADMIN_TOKEN: &str = "admin-test-token-with-at-least-32-characters";
pub(crate) const NAMED_ADMIN: &str = "named-admin-token-with-at-least-32-characters";
pub(crate) const VIEWER: &str = "viewer-admin-token-with-at-least-32-characters";

pub(crate) fn example_config() -> policy::Config {
    toml::from_str(include_str!("../../config.example.toml")).unwrap()
}

pub(crate) fn grant_admins(app: &mut App) {
    app.admin_token = Some(ADMIN_TOKEN.into());
    app.admin_tokens = vec![
        ("alice".into(), NAMED_ADMIN.into(), policy::AdminRole::Admin),
        ("bob".into(), VIEWER.into(), policy::AdminRole::Viewer),
    ];
}

fn database_url() -> Option<String> {
    match std::env::var("NOTI_TEST_DATABASE_URL") {
        Ok(url) => Some(url),
        // CI must fail instead of passing with every database test skipped.
        Err(_) if std::env::var_os("CI").is_some() => {
            panic!("set NOTI_TEST_DATABASE_URL to a disposable PostgreSQL database")
        }
        Err(_) => {
            eprintln!("skipping database test: NOTI_TEST_DATABASE_URL is not set");
            None
        }
    }
}

pub(crate) struct Fixture {
    pub(crate) app: Arc<App>,
    pub(crate) router: Router,
    admin: PgPool,
    schema: String,
}
impl Fixture {
    pub(crate) async fn new() -> Option<Self> {
        Self::with_app(|_| {}).await
    }
    pub(crate) async fn with_config(edit: impl FnOnce(&mut policy::Config)) -> Option<Self> {
        Self::with_app(|app| edit(&mut app.config)).await
    }
    pub(crate) async fn with_app(edit: impl FnOnce(&mut App)) -> Option<Self> {
        let url = database_url()?;
        let schema = format!("noti_mesh_test_{}", uuid::Uuid::new_v4().simple());
        let admin = PgPool::connect(&url).await.unwrap();
        // The schema name is built only from a UUID, so it cannot carry SQL.
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
            .execute(&admin)
            .await
            .unwrap();
        let search = schema.clone();
        let db = sqlx::postgres::PgPoolOptions::new()
            .after_connect(move |connection, _| {
                let q = search.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path',$1,false)")
                        .bind(q)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(&url)
            .await
            .unwrap();
        sqlx::migrate!().run(&db).await.unwrap();
        let mut app = App::for_test(db, example_config());
        edit(&mut app);
        app.config.validate().unwrap();
        let app = Arc::new(app);
        let router = Router::new()
            .route("/v1/events", post(ingest))
            .route("/v1/client", get(client_identity))
            .route("/v1/incidents", get(list))
            .route("/v1/incidents/{id}", get(detail))
            .route("/v1/messages", post(messages::ingest))
            .route("/v1/messages/{id}", get(messages::status))
            .route("/hooks/slack/{token}", post(crate::slack_hooks::ingest))
            .route("/hooks/alertmanager", post(crate::alertmanager::ingest))
            .route("/api/v2/alerts", post(crate::prometheus::ingest))
            .route("/hooks/uptime-kuma", post(crate::monitors::uptime_kuma))
            .route("/hooks/sentry/{token}", post(crate::monitors::sentry))
            .route("/hooks/datadog", post(crate::monitors::datadog))
            .route(
                "/hooks/grafana",
                post(crate::alertmanager::ingest).put(crate::alertmanager::ingest),
            )
            .with_state(app.clone());
        Some(Self {
            app,
            router,
            admin,
            schema,
        })
    }
    pub(crate) fn admin_router(&self) -> Router {
        admin::routes().with_state(self.app.clone())
    }
    pub(crate) async fn close(self) {
        self.app.db.close().await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "DROP SCHEMA {} CASCADE",
            self.schema
        )))
        .execute(&self.admin)
        .await
        .unwrap();
        self.admin.close().await;
    }
}
