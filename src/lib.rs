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

use std::sync::Arc;

use axum::extract::{Path as AxPath, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use include_dir::{include_dir, Dir};
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

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
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
