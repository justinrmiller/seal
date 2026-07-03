//! Liveness and readiness probes for orchestrated / containerized deploys.
//!
//! `/health` is a pure liveness check (the process is up and serving) and never
//! touches the database. `/readyz` is a readiness check that pings the database
//! so a broken bucket, expired credentials, or unreachable object store surfaces
//! as a `503` an orchestrator can act on — instead of a crash loop or silent
//! failure on the first real request.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::AppState;

/// GET /health — liveness. Always `200` while the process is serving. No DB.
pub async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({ "status": "ok" })))
}

/// GET /readyz — readiness. Cheaply pings the database (lists table names); the
/// same call `init_db` uses. `200` when the store is reachable, `503` otherwise.
pub async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    match state.conn.table_names().execute().await {
        Ok(_) => (StatusCode::OK, Json(json!({ "status": "ready" }))),
        Err(e) => {
            tracing::warn!("readiness check failed: {e}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "unavailable" })),
            )
        }
    }
}
