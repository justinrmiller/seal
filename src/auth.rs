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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg_with(secret: &str, algorithm: &str) -> Config {
        let mut c = Config::for_test(PathBuf::from("unused.lance"), secret.into())
            .expect("build test config");
        c.jwt_algorithm = algorithm.to_string();
        c
    }

    fn cfg() -> Config {
        cfg_with("test-secret", "HS256")
    }

    #[test]
    fn algorithm_from_maps_supported_variants() {
        assert!(matches!(
            algorithm_from(&cfg_with("s", "HS256")),
            Ok(Algorithm::HS256)
        ));
        assert!(matches!(
            algorithm_from(&cfg_with("s", "HS384")),
            Ok(Algorithm::HS384)
        ));
        assert!(matches!(
            algorithm_from(&cfg_with("s", "HS512")),
            Ok(Algorithm::HS512)
        ));
    }

    #[test]
    fn algorithm_from_rejects_unsupported() {
        let err = algorithm_from(&cfg_with("s", "RS256")).expect_err("unsupported");
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn password_hash_round_trips_and_rejects_wrong_input() {
        let hash = hash_password("correct horse").unwrap();
        assert_ne!(hash, "correct horse", "must not store plaintext");
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong", &hash));
    }

    #[test]
    fn verify_password_returns_false_on_malformed_hash() {
        // A non-bcrypt string must not panic and must not verify.
        assert!(!verify_password("anything", "not-a-bcrypt-hash"));
    }

    #[test]
    fn token_round_trips_for_each_supported_algorithm() {
        for algorithm in ["HS256", "HS384", "HS512"] {
            let c = cfg_with("shared-secret", algorithm);
            let token = create_token(&c, "alice").unwrap();
            assert_eq!(decode_token(&c, &token).as_deref(), Some("alice"));
            assert_eq!(require_auth(&c, &token).unwrap(), "alice");
        }
    }

    #[test]
    fn decode_token_rejects_wrong_secret() {
        let signer = cfg_with("secret-a", "HS256");
        let verifier = cfg_with("secret-b", "HS256");
        let token = create_token(&signer, "alice").unwrap();
        assert_eq!(decode_token(&verifier, &token), None);
    }

    #[test]
    fn decode_token_rejects_garbage() {
        let c = cfg();
        assert_eq!(decode_token(&c, "not.a.jwt"), None);
        assert_eq!(decode_token(&c, ""), None);
    }

    #[test]
    fn require_auth_maps_invalid_token_to_unauthorized() {
        let c = cfg();
        let err = require_auth(&c, "garbage").expect_err("invalid");
        assert!(matches!(err, AppError::Unauthorized(_)));
    }

    #[test]
    fn expired_token_is_rejected() {
        // Sign a token whose exp is well in the past (beyond jsonwebtoken's
        // default 60s leeway), using the same secret, and confirm it fails.
        let c = cfg();
        let past = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            - 3600;
        let claims = Claims {
            sub: "alice".to_string(),
            exp: past,
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(c.jwt_secret.as_bytes()),
        )
        .unwrap();
        assert_eq!(decode_token(&c, &token), None);
    }
}
