use std::time::{SystemTime, UNIX_EPOCH};

use bcrypt::DEFAULT_COST;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::AppError;

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: u64,
}

pub fn hash_password(plain: &str) -> Result<String, AppError> {
    bcrypt::hash(plain, DEFAULT_COST)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("bcrypt hash failed: {e}")))
}

pub fn verify_password(plain: &str, hashed: &str) -> bool {
    bcrypt::verify(plain, hashed).unwrap_or(false)
}

fn algorithm_from(cfg: &Config) -> Result<Algorithm, AppError> {
    match cfg.jwt_algorithm.as_str() {
        "HS256" => Ok(Algorithm::HS256),
        "HS384" => Ok(Algorithm::HS384),
        "HS512" => Ok(Algorithm::HS512),
        other => Err(AppError::Internal(anyhow::anyhow!(
            "unsupported JWT algorithm: {other}"
        ))),
    }
}

pub fn create_token(cfg: &Config, username: &str) -> Result<String, AppError> {
    let algorithm = algorithm_from(cfg)?;
    let exp_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        + (cfg.token_expire_minutes.max(0) as u64) * 60;
    let claims = Claims {
        sub: username.to_string(),
        exp: exp_seconds,
    };
    encode(
        &Header::new(algorithm),
        &claims,
        &EncodingKey::from_secret(cfg.jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(anyhow::anyhow!("jwt encode failed: {e}")))
}

/// Mirrors `require_auth` in app/main.py: 401 "Invalid token" when decode fails.
pub fn require_auth(cfg: &Config, token: &str) -> Result<String, AppError> {
    decode_token(cfg, token).ok_or(AppError::Unauthorized("Invalid token"))
}

/// Returns `Some(username)` if the token validates, `None` on any error —
/// callers translate `None` into the same 401 the Python server returns.
pub fn decode_token(cfg: &Config, token: &str) -> Option<String> {
    let algorithm = algorithm_from(cfg).ok()?;
    let validation = Validation::new(algorithm);
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(cfg.jwt_secret.as_bytes()),
        &validation,
    )
    .ok()
    .map(|data| data.claims.sub)
}
