use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::Json;

use crate::auth::{create_token, hash_password, verify_password};
use crate::db;
use crate::db_ops::{self, total_rows};
use crate::error::{AppError, AppResult};
use crate::models::{LoginRequest, RegisterRequest, TokenResponse};
use crate::validate::validate_username;
use crate::AppState;

pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<RegisterRequest>,
) -> AppResult<Json<TokenResponse>> {
    state.rate_limiter.check(addr.ip())?;
    validate_username(&state.cfg, &req.username)?;

    let users = db_ops::open(&state.conn, "users").await?;
    let existing =
        db_ops::scan_where(&users, &format!("username = '{}'", req.username), Some(1)).await?;
    if total_rows(&existing) > 0 {
        return Err(AppError::BadRequest("Username already taken".into()));
    }

    let row = db_ops::utf8_row(
        db::users_schema(),
        &[
            ("username", &req.username),
            ("password_hash", &hash_password(&req.password)?),
            ("public_key_jwk", &req.public_key_jwk),
        ],
    )?;
    db_ops::append(&users, row).await?;

    Ok(Json(TokenResponse {
        token: create_token(&state.cfg, &req.username)?,
        username: req.username,
    }))
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<LoginRequest>,
) -> AppResult<Json<TokenResponse>> {
    state.rate_limiter.check(addr.ip())?;
    validate_username(&state.cfg, &req.username)?;

    let users = db_ops::open(&state.conn, "users").await?;
    let rows =
        db_ops::scan_where(&users, &format!("username = '{}'", req.username), Some(1)).await?;
    let hash = db_ops::first_string(&rows, "password_hash");
    let valid = match hash {
        Some(h) => verify_password(&req.password, &h),
        None => false,
    };
    if !valid {
        return Err(AppError::Unauthorized("Invalid credentials"));
    }

    Ok(Json(TokenResponse {
        token: create_token(&state.cfg, &req.username)?,
        username: req.username,
    }))
}
