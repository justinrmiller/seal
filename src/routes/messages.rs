use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path as AxPath, Query, State};
use axum::Json;
use serde_json::json;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::db;
use crate::db_ops::{self, Cell};
use crate::error::{AppError, AppResult};
use crate::models::{AfterQuery, ChannelMessagePayload, StoredMessage, TokenQuery};
use crate::validate::{validate_after, validate_id, validate_username};
use crate::AppState;

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

async fn assert_channel_member(
    state: &AppState,
    channel_id: &str,
    username: &str,
) -> AppResult<()> {
    let members_table = db_ops::open(&state.conn, "channel_members").await?;
    let rows = db_ops::scan_where(
        &members_table,
        &format!("channel_id = '{channel_id}' AND username = '{username}'"),
        Some(1),
    )
    .await?;
    if db_ops::total_rows(&rows) == 0 {
        return Err(AppError::Forbidden("Not a member of this channel"));
    }
    Ok(())
}

/// GET /api/messages/{peer} — DM history merged from self-copies + received.
pub async fn get_dm_messages(
    State(state): State<AppState>,
    AxPath(peer): AxPath<String>,
    Query(qp): Query<AfterQuery>,
) -> AppResult<Json<Vec<StoredMessage>>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    validate_username(&state.cfg, &peer)?;
    validate_after(qp.after)?;

    let messages = db_ops::open(&state.conn, "messages").await?;
    let time_filter = if qp.after > 0.0 {
        format!(" AND timestamp > {}", qp.after)
    } else {
        String::new()
    };

    let sent_self = db_ops::scan_where(
        &messages,
        &format!(
            "sender = '{username}' AND recipient = '{peer}' AND channel_id = 'self'{time_filter}"
        ),
        None,
    )
    .await?;
    let received = db_ops::scan_where(
        &messages,
        &format!(
            "sender = '{peer}' AND recipient = '{username}' AND channel_id = ''{time_filter}"
        ),
        None,
    )
    .await?;

    let mut all = db_ops::rows_to_stored_messages(&sent_self);
    all.extend(db_ops::rows_to_stored_messages(&received));
    all.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Json(all))
}

/// GET /api/channels/{channel_id}/messages — channel history for the caller.
pub async fn get_channel_messages(
    State(state): State<AppState>,
    AxPath(channel_id): AxPath<String>,
    Query(qp): Query<AfterQuery>,
) -> AppResult<Json<Vec<StoredMessage>>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    validate_id(&state.cfg, &channel_id)?;
    validate_after(qp.after)?;
    assert_channel_member(&state, &channel_id, &username).await?;

    let messages = db_ops::open(&state.conn, "messages").await?;
    let time_filter = if qp.after > 0.0 {
        format!(" AND timestamp > {}", qp.after)
    } else {
        String::new()
    };
    let rows = db_ops::scan_where(
        &messages,
        &format!(
            "channel_id = '{channel_id}' AND recipient = '{username}'{time_filter}"
        ),
        None,
    )
    .await?;
    let mut all = db_ops::rows_to_stored_messages(&rows);
    all.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Json(all))
}

/// POST /api/channels/{channel_id}/messages — REST alternative to WebSocket
/// for sending a channel message. Stores one row per envelope; if the message
/// includes an attachment it is stored once in the attachments table.
pub async fn post_channel_message(
    State(state): State<AppState>,
    AxPath(channel_id): AxPath<String>,
    Query(qp): Query<TokenQuery>,
    Json(payload): Json<ChannelMessagePayload>,
) -> AppResult<Json<serde_json::Value>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    validate_id(&state.cfg, &channel_id)?;
    assert_channel_member(&state, &channel_id, &username).await?;

    let result = store_channel_message(&state, &username, &channel_id, &payload).await?;
    relay_channel_message(&state, &username, &payload, &result);
    Ok(Json(json!({
        "status": "ok",
        "group_id": result.group_id,
        "timestamp": result.timestamp,
    })))
}

/// Push one relay frame per envelope to every currently-connected recipient.
/// Shared by the REST and WS channel-message handlers.
pub fn relay_channel_message(
    state: &AppState,
    sender: &str,
    payload: &ChannelMessagePayload,
    result: &ChannelMessageStoreResult,
) {
    for (env, msg_id) in payload.envelopes.iter().zip(&result.per_envelope_ids) {
        let relay = json!({
            "type": "channel",
            "id": msg_id,
            "group_id": result.group_id,
            "sender": sender,
            "recipient": env.target_user,
            "channel_id": payload.channel_id,
            "ciphertext": env.ciphertext,
            "iv": env.iv,
            "sender_public_key_jwk": env.sender_public_key_jwk,
            "timestamp": result.timestamp,
            "message_type": result.message_type,
            "attachment_id": result.attachment_id,
        });
        state
            .ws_connections
            .send_to(&env.target_user, &relay.to_string());
    }
}

pub struct ChannelMessageStoreResult {
    pub group_id: String,
    pub timestamp: f64,
    pub attachment_id: String,
    pub message_type: String,
    pub per_envelope_ids: Vec<String>,
}

/// Shared persistence path used by REST and (eventually) the WebSocket handler.
pub async fn store_channel_message(
    state: &AppState,
    sender: &str,
    channel_id: &str,
    payload: &ChannelMessagePayload,
) -> AppResult<ChannelMessageStoreResult> {
    let msg_type = if payload.message_type == "image" {
        "image"
    } else {
        "text"
    };
    let ts = now_secs();

    let attachment_id = if let (Some(att), "image") = (payload.attachment.as_ref(), msg_type) {
        let id = Uuid::new_v4().to_string();
        let att_table = db_ops::open(&state.conn, "attachments").await?;
        let row = db_ops::mixed_row(
            db::attachments_schema(),
            &[
                Cell::Str(&id),
                Cell::Str(sender),
                Cell::Str(channel_id),
                Cell::LargeStr(&att.encrypted_data),
                Cell::Str(&att.iv),
                Cell::F64(ts),
            ],
        )?;
        db_ops::append(&att_table, row).await?;
        id
    } else {
        String::new()
    };

    let messages = db_ops::open(&state.conn, "messages").await?;
    let msg_group_id = Uuid::new_v4().to_string();
    let mut per_envelope_ids = Vec::with_capacity(payload.envelopes.len());

    for env in &payload.envelopes {
        let msg_id = Uuid::new_v4().to_string();
        let row = db_ops::mixed_row(
            db::messages_schema(),
            &[
                Cell::Str(&msg_id),
                Cell::Str(sender),
                Cell::Str(&env.target_user),
                Cell::Str(channel_id),
                Cell::Str(&env.ciphertext),
                Cell::Str(&env.iv),
                Cell::Str(&env.sender_public_key_jwk),
                Cell::F64(ts),
                Cell::Str(msg_type),
                Cell::Str(&attachment_id),
            ],
        )?;
        db_ops::append(&messages, row).await?;
        per_envelope_ids.push(msg_id);
    }

    Ok(ChannelMessageStoreResult {
        group_id: msg_group_id,
        timestamp: ts,
        attachment_id,
        message_type: msg_type.to_string(),
        per_envelope_ids,
    })
}
