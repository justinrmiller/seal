use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(&'static str),
    #[error("{0}")]
    Forbidden(&'static str),
    #[error("{0}")]
    NotFound(String),
    #[error("Too many requests. Try again later.")]
    TooManyRequests,
    #[error("internal error: {0}")]
    Internal(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, detail) = match &self {
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, (*msg).to_string()),
            AppError::Forbidden(msg) => (StatusCode::FORBIDDEN, (*msg).to_string()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::TooManyRequests => (
                StatusCode::TOO_MANY_REQUESTS,
                "Too many requests. Try again later.".to_string(),
            ),
            AppError::Internal(e) => {
                tracing::error!("internal error: {e:?}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Internal server error".to_string(),
                )
            }
        };
        (status, Json(json!({ "detail": detail }))).into_response()
    }
}

#[allow(dead_code)]
pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    async fn status_and_detail(err: AppError) -> (StatusCode, String) {
        let resp = err.into_response();
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        (status, body["detail"].as_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn each_variant_maps_to_its_status_and_detail() {
        let cases = [
            (
                AppError::BadRequest("bad".into()),
                StatusCode::BAD_REQUEST,
                "bad",
            ),
            (
                AppError::Unauthorized("no auth"),
                StatusCode::UNAUTHORIZED,
                "no auth",
            ),
            (
                AppError::Forbidden("nope"),
                StatusCode::FORBIDDEN,
                "nope",
            ),
            (
                AppError::NotFound("missing".into()),
                StatusCode::NOT_FOUND,
                "missing",
            ),
            (
                AppError::TooManyRequests,
                StatusCode::TOO_MANY_REQUESTS,
                "Too many requests. Try again later.",
            ),
        ];
        for (err, want_status, want_detail) in cases {
            let (status, detail) = status_and_detail(err).await;
            assert_eq!(status, want_status);
            assert_eq!(detail, want_detail);
        }
    }

    #[tokio::test]
    async fn internal_error_is_masked_as_500() {
        // The underlying anyhow message must not leak into the response body.
        let (status, detail) =
            status_and_detail(AppError::Internal(anyhow::anyhow!("secret db path leaked"))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(detail, "Internal server error");
        assert!(!detail.contains("secret"));
    }

    #[tokio::test]
    async fn anyhow_converts_into_internal_via_from() {
        // The `#[from]` on Internal lets `?` turn an anyhow::Error into AppError.
        fn fails() -> Result<(), AppError> {
            Err(anyhow::anyhow!("boom"))?;
            Ok(())
        }
        let (status, _) = status_and_detail(fails().unwrap_err()).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }
}
