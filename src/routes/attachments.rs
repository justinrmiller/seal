use axum::extract::{Path as AxPath, Query, State};
use axum::Json;

use crate::auth::require_auth;
use crate::db_ops;
use crate::error::{AppError, AppResult};
use crate::models::{AttachmentResponse, TokenQuery};
use crate::validate::validate_id;
use crate::AppState;

/// GET /api/attachments/{attachment_id} — retrieve an encrypted attachment blob.
/// Mirrors app/main.py:get_attachment.
pub async fn get_attachment(
    State(state): State<AppState>,
    AxPath(attachment_id): AxPath<String>,
    Query(qp): Query<TokenQuery>,
) -> AppResult<Json<AttachmentResponse>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    validate_id(&state.cfg, &attachment_id)?;

    let att_table = db_ops::open(&state.conn, "attachments").await?;
    let rows = db_ops::scan_where(
        &att_table,
        &format!("id = '{attachment_id}'"),
        Some(1),
    )
    .await?;
    if db_ops::total_rows(&rows) == 0 {
        return Err(AppError::NotFound("Attachment not found".into()));
    }

    let channel_id = db_ops::first_string(&rows, "channel_id").unwrap_or_default();
    if !channel_id.is_empty() {
        let members_table = db_ops::open(&state.conn, "channel_members").await?;
        let mem_rows = db_ops::scan_where(
            &members_table,
            &format!("channel_id = '{channel_id}' AND username = '{username}'"),
            Some(1),
        )
        .await?;
        if db_ops::total_rows(&mem_rows) == 0 {
            return Err(AppError::Forbidden("Not a member of this channel"));
        }
    }

    let encrypted_data = db_ops::first_large_string(&rows, "encrypted_data")
        .unwrap_or_default();
    let iv = db_ops::first_string(&rows, "iv").unwrap_or_default();

    Ok(Json(AttachmentResponse {
        id: attachment_id,
        encrypted_data,
        iv,
    }))
}
