use std::sync::Arc;

use arrow_array::{Array, Float64Array, RecordBatch, StringArray};
use axum::extract::{Path as AxPath, Query, State};
use axum::Json;
use lancedb::arrow::arrow_schema::SchemaRef;
use serde_json::json;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::db;
use crate::db_ops::{self, now_secs, Cell};
use crate::error::{AppError, AppResult};
use crate::models::{
    AfterQuery, ChannelEncryptedEnvelope, ChannelMessagePayload, StoredMessage, TokenQuery,
};
use crate::validate::{validate_after, validate_id, validate_username};
use crate::AppState;

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
        // Reject oversized attachments before writing them to (object) storage.
        // The bound is on the encrypted payload the client sent.
        let max = state.cfg.max_image_size_bytes;
        if att.encrypted_data.len() > max {
            return Err(AppError::PayloadTooLarge(format!(
                "Attachment exceeds the maximum size of {max} bytes"
            )));
        }
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

    let msg_group_id = Uuid::new_v4().to_string();
    // One row per envelope, written in a SINGLE append. On object storage each
    // append is a commit (a network round-trip and, on plain S3, a concurrent-
    // write hazard), so batching all envelopes into one commit cuts both.
    let per_envelope_ids: Vec<String> = payload
        .envelopes
        .iter()
        .map(|_| Uuid::new_v4().to_string())
        .collect();

    if !payload.envelopes.is_empty() {
        let messages = db_ops::open(&state.conn, "messages").await?;
        let batch = build_messages_batch(
            db::messages_schema(),
            sender,
            channel_id,
            ts,
            msg_type,
            &attachment_id,
            &payload.envelopes,
            &per_envelope_ids,
        )?;
        db_ops::append(&messages, batch).await?;
    }

    Ok(ChannelMessageStoreResult {
        group_id: msg_group_id,
        timestamp: ts,
        attachment_id,
        message_type: msg_type.to_string(),
        per_envelope_ids,
    })
}

/// Build a multi-row `messages` batch — one row per envelope — so all envelopes
/// of a channel message are persisted in a single LanceDB commit. Mirrors the
/// batch pattern used by `channels::build_member_batch`.
#[allow(clippy::too_many_arguments)]
fn build_messages_batch(
    schema: SchemaRef,
    sender: &str,
    channel_id: &str,
    timestamp: f64,
    message_type: &str,
    attachment_id: &str,
    envelopes: &[ChannelEncryptedEnvelope],
    ids: &[String],
) -> Result<RecordBatch, AppError> {
    let n = envelopes.len();
    let id_col: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
    let sender_col: Vec<&str> = (0..n).map(|_| sender).collect();
    let recipient_col: Vec<&str> = envelopes.iter().map(|e| e.target_user.as_str()).collect();
    let channel_col: Vec<&str> = (0..n).map(|_| channel_id).collect();
    let ciphertext_col: Vec<&str> = envelopes.iter().map(|e| e.ciphertext.as_str()).collect();
    let iv_col: Vec<&str> = envelopes.iter().map(|e| e.iv.as_str()).collect();
    let spk_col: Vec<&str> = envelopes
        .iter()
        .map(|e| e.sender_public_key_jwk.as_str())
        .collect();
    let ts_col: Vec<f64> = (0..n).map(|_| timestamp).collect();
    let mtype_col: Vec<&str> = (0..n).map(|_| message_type).collect();
    let att_col: Vec<&str> = (0..n).map(|_| attachment_id).collect();

    let columns: Vec<Arc<dyn Array>> = vec![
        Arc::new(StringArray::from(id_col)),
        Arc::new(StringArray::from(sender_col)),
        Arc::new(StringArray::from(recipient_col)),
        Arc::new(StringArray::from(channel_col)),
        Arc::new(StringArray::from(ciphertext_col)),
        Arc::new(StringArray::from(iv_col)),
        Arc::new(StringArray::from(spk_col)),
        Arc::new(Float64Array::from(ts_col)),
        Arc::new(StringArray::from(mtype_col)),
        Arc::new(StringArray::from(att_col)),
    ];
    RecordBatch::try_new(schema, columns)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("messages batch: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(target: &str, ct: &str) -> ChannelEncryptedEnvelope {
        ChannelEncryptedEnvelope {
            target_user: target.to_string(),
            ciphertext: ct.to_string(),
            iv: "iv".to_string(),
            sender_public_key_jwk: "spk".to_string(),
        }
    }

    #[test]
    fn build_messages_batch_makes_one_row_per_envelope() {
        let envelopes = vec![env("alice", "ct-a"), env("bob", "ct-b"), env("cara", "ct-c")];
        let ids: Vec<String> = vec!["m1".into(), "m2".into(), "m3".into()];
        let batch = build_messages_batch(
            db::messages_schema(),
            "sender",
            "chan-1",
            42.0,
            "text",
            "att-1",
            &envelopes,
            &ids,
        )
        .expect("batch");

        // One row per envelope, in a single batch (a single append/commit).
        assert_eq!(batch.num_rows(), 3);
        let msgs = db_ops::rows_to_stored_messages(&[batch]);
        assert_eq!(
            msgs.iter().map(|m| m.recipient.as_str()).collect::<Vec<_>>(),
            vec!["alice", "bob", "cara"]
        );
        assert_eq!(
            msgs.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["m1", "m2", "m3"]
        );
        // Constant columns are applied to every row.
        assert!(msgs.iter().all(|m| m.sender == "sender"
            && m.channel_id == "chan-1"
            && m.timestamp == 42.0
            && m.message_type == "text"
            && m.attachment_id == "att-1"));
    }
}
