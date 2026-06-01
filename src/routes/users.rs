use std::collections::BTreeSet;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path as AxPath, Query, State};
use axum::Json;

use crate::auth::require_auth;
use crate::db_ops;
use crate::error::{AppError, AppResult};
use crate::models::{
    SearchQuery, TokenQuery, UserListItem, UserPublicKeyResponse,
};
use crate::validate::validate_username;
use crate::AppState;

/// GET /api/users — list DM peers (users the caller has exchanged DMs with).
/// Mirrors app/main.py:list_users.
pub async fn list_users(
    State(state): State<AppState>,
    Query(q): Query<TokenQuery>,
) -> AppResult<Json<Vec<UserListItem>>> {
    let username = require_auth(&state.cfg, &q.token)?;

    let messages = db_ops::open(&state.conn, "messages").await?;
    let sent = db_ops::scan_where(
        &messages,
        &format!("sender = '{username}' AND channel_id = 'self'"),
        None,
    )
    .await?;
    let received = db_ops::scan_where(
        &messages,
        &format!("recipient = '{username}' AND channel_id = ''"),
        None,
    )
    .await?;

    let mut peers: BTreeSet<String> = BTreeSet::new();
    peers.extend(db_ops::collect_string_column(&sent, "recipient"));
    peers.extend(db_ops::collect_string_column(&received, "sender"));

    Ok(Json(
        peers
            .into_iter()
            .map(|username| UserListItem { username })
            .collect(),
    ))
}

/// GET /api/users/search — prefix-search the users table, excluding self,
/// max 20 results. Rate-limited. Mirrors app/main.py:search_users.
pub async fn search_users(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(q): Query<SearchQuery>,
) -> AppResult<Json<Vec<UserListItem>>> {
    state.rate_limiter.check(addr.ip())?;
    let username = require_auth(&state.cfg, &q.token)?;
    if q.q.is_empty() {
        return Err(AppError::BadRequest("q is required".into()));
    }
    validate_username(&state.cfg, &q.q)?;

    let users = db_ops::open(&state.conn, "users").await?;
    let rows = db_ops::scan_where(&users, &format!("username LIKE '{}%'", q.q), None).await?;
    let mut all = db_ops::collect_string_column(&rows, "username");
    all.retain(|u| u != &username);
    all.truncate(20);

    Ok(Json(
        all.into_iter()
            .map(|username| UserListItem { username })
            .collect(),
    ))
}

/// GET /api/users/{target}/public_key — fetch a user's public key.
/// Mirrors app/main.py:get_public_key.
pub async fn get_public_key(
    State(state): State<AppState>,
    AxPath(target): AxPath<String>,
    Query(q): Query<TokenQuery>,
) -> AppResult<Json<UserPublicKeyResponse>> {
    require_auth(&state.cfg, &q.token)?;
    validate_username(&state.cfg, &target)?;

    let users = db_ops::open(&state.conn, "users").await?;
    let rows = db_ops::scan_where(&users, &format!("username = '{target}'"), Some(1)).await?;
    let public_key_jwk = db_ops::first_string(&rows, "public_key_jwk")
        .ok_or_else(|| AppError::NotFound("User not found".into()))?;

    Ok(Json(UserPublicKeyResponse {
        username: target,
        public_key_jwk,
    }))
}
