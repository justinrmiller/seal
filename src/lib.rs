pub mod auth;
pub mod config;
pub mod db;
pub mod db_ops;
pub mod error;
pub mod models;
pub mod rate_limit;
pub mod routes;
pub mod validate;
pub mod ws;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path as AxPath, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use include_dir::{include_dir, Dir};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::rate_limit::RateLimiter;
use crate::ws::WsConnections;

static STATIC_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");
static TEMPLATES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub conn: lancedb::connection::Connection,
    pub rate_limiter: Arc<RateLimiter>,
    pub ws_connections: Arc<WsConnections>,
}

impl AppState {
    /// Open the configured database, run migrations, and assemble the shared
    /// application state. Shared by `main` (via [`run`]) and the integration
    /// test harness so the startup path is exercised by every test.
    pub async fn bootstrap(cfg: Arc<Config>) -> anyhow::Result<Self> {
        let conn = db::connect(&cfg.database_path, &cfg.storage_options).await?;
        db::init_db(&conn).await?;
        Ok(Self {
            cfg,
            conn,
            rate_limiter: Arc::new(RateLimiter::new()),
            ws_connections: Arc::new(WsConnections::new()),
        })
    }
}

/// Serve the application on `listener` until `shutdown` resolves.
pub async fn serve(
    state: AppState,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let app = build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await?;
    Ok(())
}

/// Full server entrypoint: bootstrap state, bind the configured address, and
/// serve until Ctrl-C. Lives here rather than in `main` so its building blocks
/// ([`AppState::bootstrap`] and [`serve`]) are covered by tests.
pub async fn run(cfg: Arc<Config>) -> anyhow::Result<()> {
    let state = AppState::bootstrap(cfg.clone()).await?;
    let addr: SocketAddr = format!("{}:{}", cfg.app_host, cfg.app_port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    serve(state, listener, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(routes::health::health))
        .route("/readyz", get(routes::health::readyz))
        .route("/api/register", post(routes::auth::register))
        .route("/api/login", post(routes::auth::login))
        .route("/api/users", get(routes::users::list_users))
        .route("/api/users/search", get(routes::users::search_users))
        .route(
            "/api/users/{target}/public_key",
            get(routes::users::get_public_key),
        )
        .route(
            "/api/channels",
            get(routes::channels::list_channels).post(routes::channels::create_channel),
        )
        .route(
            "/api/channels/browse",
            get(routes::channels::browse_channels),
        )
        .route(
            "/api/channels/{channel_id}",
            get(routes::channels::get_channel),
        )
        .route(
            "/api/channels/{channel_id}/join",
            post(routes::channels::join_channel),
        )
        .route(
            "/api/channels/{channel_id}/members",
            post(routes::channels::add_channel_member),
        )
        .route(
            "/api/channels/{channel_id}/members/public_keys",
            get(routes::channels::get_channel_member_keys),
        )
        .route(
            "/api/messages/{peer}",
            get(routes::messages::get_dm_messages),
        )
        .route(
            "/api/channels/{channel_id}/messages",
            get(routes::messages::get_channel_messages)
                .post(routes::messages::post_channel_message),
        )
        .route(
            "/api/attachments/{attachment_id}",
            get(routes::attachments::get_attachment),
        )
        .route("/ws/chat", get(routes::ws::ws_chat))
        .route("/static/{*path}", get(serve_static))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn index(State(_): State<AppState>) -> Response {
    match TEMPLATES_DIR.get_file("index.html") {
        Some(file) => match file.contents_utf8() {
            Some(body) => Html(body.to_string()).into_response(),
            None => internal_error("index.html is not valid UTF-8"),
        },
        None => internal_error("index.html missing from embedded templates"),
    }
}

async fn serve_static(AxPath(path): AxPath<String>) -> Response {
    match STATIC_DIR.get_file(&path) {
        Some(file) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            (
                [(header::CONTENT_TYPE, mime.essence_str().to_string())],
                file.contents(),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

fn internal_error(msg: &'static str) -> Response {
    tracing::error!("{msg}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "text/plain")],
        msg,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_state() -> (AppState, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = Config::for_test(dir.path().join("t.lance"), "lib-test-secret".into())
            .expect("test config");
        let state = AppState::bootstrap(Arc::new(cfg))
            .await
            .expect("bootstrap");
        (state, dir)
    }

    #[tokio::test]
    async fn bootstrap_opens_db_and_creates_all_tables() {
        let (state, _dir) = test_state().await;
        let names: std::collections::HashSet<String> = state
            .conn
            .table_names()
            .execute()
            .await
            .expect("table_names")
            .into_iter()
            .collect();
        for table in ["users", "messages", "channels", "channel_members", "attachments"] {
            assert!(names.contains(table), "bootstrap should create {table}");
        }
    }

    #[tokio::test]
    async fn serve_responds_then_shuts_down_gracefully() {
        let (state, _dir) = test_state().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(serve(state, listener, async {
            let _ = rx.await;
        }));

        // The server is up and routes requests (exercises build_router + index).
        let res = reqwest::get(format!("http://{addr}/"))
            .await
            .expect("request");
        assert_eq!(res.status(), 200);

        // Signalling shutdown lets the server future resolve cleanly.
        let _ = tx.send(());
        handle
            .await
            .expect("join")
            .expect("serve returns Ok on graceful shutdown");
    }
}
