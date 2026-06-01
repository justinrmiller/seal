use crate::config::Config;
use crate::error::AppError;

/// Mirrors `validate_username` in app/main.py: the message is hard-coded "1-64"
/// to match the existing pytest assertions.
pub fn validate_username<'a>(cfg: &Config, name: &'a str) -> Result<&'a str, AppError> {
    if cfg.safe_name_re.is_match(name) {
        Ok(name)
    } else {
        Err(AppError::BadRequest(
            "Username must be 1-64 alphanumeric characters, hyphens, or underscores".into(),
        ))
    }
}

#[allow(dead_code)]
pub fn validate_id<'a>(cfg: &Config, val: &'a str) -> Result<&'a str, AppError> {
    if cfg.safe_id_re.is_match(val) {
        Ok(val)
    } else {
        Err(AppError::BadRequest("Invalid ID format".into()))
    }
}

#[allow(dead_code)]
pub fn validate_after(after: f64) -> Result<f64, AppError> {
    if after.is_nan() || after.is_infinite() || after < 0.0 {
        Err(AppError::BadRequest("Invalid 'after' timestamp".into()))
    } else {
        Ok(after)
    }
}
