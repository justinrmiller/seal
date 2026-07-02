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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg() -> Config {
        Config::for_test(PathBuf::from("unused.lance"), "test-secret".into())
            .expect("build test config")
    }

    fn is_bad_request(err: AppError) -> bool {
        matches!(err, AppError::BadRequest(_))
    }

    #[test]
    fn validate_username_accepts_allowed_characters() {
        let c = cfg();
        for name in ["alice", "Bob_99", "a-b-c", "A", &"x".repeat(64)] {
            assert_eq!(validate_username(&c, name).unwrap(), name);
        }
    }

    #[test]
    fn validate_username_rejects_invalid_and_out_of_range() {
        let c = cfg();
        // Empty, illegal characters, and longer than the 64-char maximum.
        for name in ["", "bad user", "no!", "space ", &"x".repeat(65)] {
            let err = validate_username(&c, name).expect_err("should reject");
            assert!(is_bad_request(err));
        }
    }

    #[test]
    fn validate_id_accepts_up_to_max_length_and_rejects_bad_input() {
        let c = cfg();
        assert_eq!(validate_id(&c, "chan_1").unwrap(), "chan_1");
        // id_max_length is 128 in the test config.
        assert_eq!(validate_id(&c, &"a".repeat(128)).unwrap().len(), 128);
        assert!(is_bad_request(validate_id(&c, "").unwrap_err()));
        assert!(is_bad_request(validate_id(&c, "has/slash").unwrap_err()));
        assert!(is_bad_request(validate_id(&c, &"a".repeat(129)).unwrap_err()));
    }

    #[test]
    fn validate_after_accepts_zero_and_positive() {
        assert_eq!(validate_after(0.0).unwrap(), 0.0);
        assert_eq!(validate_after(1_700_000_000.5).unwrap(), 1_700_000_000.5);
    }

    #[test]
    fn validate_after_rejects_negative_nan_and_infinite() {
        for bad in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(is_bad_request(validate_after(bad).unwrap_err()));
        }
    }
}
