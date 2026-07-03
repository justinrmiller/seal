use std::collections::HashSet;
use std::sync::Arc;

use arrow_array::{Array, Float64Array, RecordBatch, StringArray};
use axum::extract::{Path as AxPath, Query, State};
use axum::Json;
use lancedb::arrow::arrow_schema::SchemaRef;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::db;
use crate::db_ops::{self, now_secs, Cell};
use crate::error::{AppError, AppResult};
use crate::models::{
    AddChannelMemberRequest, ChannelBrowseItem, ChannelInfo, ChannelMemberKey,
    CreateChannelRequest, TokenQuery,
};
use crate::validate::{validate_id, validate_username};
use crate::AppState;

async fn list_channel_member_usernames(
    state: &AppState,
    channel_id: &str,
) -> AppResult<Vec<String>> {
    let members_table = db_ops::open(&state.conn, "channel_members").await?;
    let rows =
        db_ops::scan_where(&members_table, &format!("channel_id = '{channel_id}'"), None).await?;
    Ok(db_ops::collect_string_column(&rows, "username"))
}

async fn is_channel_member(
    state: &AppState,
    channel_id: &str,
    username: &str,
) -> AppResult<bool> {
    let members_table = db_ops::open(&state.conn, "channel_members").await?;
    let rows = db_ops::scan_where(
        &members_table,
        &format!("channel_id = '{channel_id}' AND username = '{username}'"),
        Some(1),
    )
    .await?;
    Ok(db_ops::total_rows(&rows) > 0)
}

async fn fetch_channel(state: &AppState, channel_id: &str) -> AppResult<Option<ChannelRow>> {
    let channels = db_ops::open(&state.conn, "channels").await?;
    let rows = db_ops::scan_where(&channels, &format!("id = '{channel_id}'"), Some(1)).await?;
    if db_ops::total_rows(&rows) == 0 {
        return Ok(None);
    }
    Ok(Some(ChannelRow {
        id: channel_id.to_string(),
        name: db_ops::first_string(&rows, "name").unwrap_or_default(),
        created_by: db_ops::first_string(&rows, "created_by").unwrap_or_default(),
    }))
}

struct ChannelRow {
    id: String,
    name: String,
    created_by: String,
}

async fn channel_info_for(state: &AppState, ch: ChannelRow) -> AppResult<ChannelInfo> {
    let members = list_channel_member_usernames(state, &ch.id).await?;
    Ok(ChannelInfo {
        id: ch.id,
        name: ch.name,
        created_by: ch.created_by,
        members,
    })
}

/// POST /api/channels — create a channel and add the creator + named members.
pub async fn create_channel(
    State(state): State<AppState>,
    Query(qp): Query<TokenQuery>,
    Json(req): Json<CreateChannelRequest>,
) -> AppResult<Json<ChannelInfo>> {
    let creator = require_auth(&state.cfg, &qp.token)?;
    if !state.cfg.safe_name_re.is_match(&req.name) {
        return Err(AppError::BadRequest(
            "Channel name must be 1-64 alphanumeric characters, hyphens, or underscores".into(),
        ));
    }
    for m in &req.members {
        validate_username(&state.cfg, m)?;
    }

    // Reject duplicate names.
    let channels = db_ops::open(&state.conn, "channels").await?;
    let dup = db_ops::scan_where(&channels, &format!("name = '{}'", req.name), Some(1)).await?;
    if db_ops::total_rows(&dup) > 0 {
        return Err(AppError::BadRequest(format!(
            "Channel name '{}' is already taken",
            req.name
        )));
    }

    // De-dup and verify every named member exists, plus the creator.
    let mut all_members: Vec<String> = Vec::with_capacity(req.members.len() + 1);
    all_members.push(creator.clone());
    for m in &req.members {
        if !all_members.contains(m) {
            all_members.push(m.clone());
        }
    }
    let users_table = db_ops::open(&state.conn, "users").await?;
    for member in &all_members {
        let rows = db_ops::scan_where(
            &users_table,
            &format!("username = '{member}'"),
            Some(1),
        )
        .await?;
        if db_ops::total_rows(&rows) == 0 {
            return Err(AppError::NotFound(format!("User '{member}' not found")));
        }
    }

    let channel_id = Uuid::new_v4().to_string();
    let now = now_secs();

    // Insert channel row.
    let channel_row = db_ops::mixed_row(
        db::channels_schema(),
        &[
            Cell::Str(&channel_id),
            Cell::Str(&req.name),
            Cell::Str(&creator),
            Cell::F64(now),
        ],
    )?;
    db_ops::append(&channels, channel_row).await?;

    // Insert membership rows in a single batch.
    let members_table = db_ops::open(&state.conn, "channel_members").await?;
    let members_batch = build_member_batch(
        db::channel_members_schema(),
        &channel_id,
        &all_members,
        now,
    )?;
    db_ops::append(&members_table, members_batch).await?;

    Ok(Json(ChannelInfo {
        id: channel_id,
        name: req.name,
        created_by: creator,
        members: all_members,
    }))
}

fn build_member_batch(
    schema: SchemaRef,
    channel_id: &str,
    members: &[String],
    joined_at: f64,
) -> Result<RecordBatch, AppError> {
    let n = members.len();
    let channel_ids: Vec<&str> = (0..n).map(|_| channel_id).collect();
    let usernames: Vec<&str> = members.iter().map(|s| s.as_str()).collect();
    let joined_ats: Vec<f64> = (0..n).map(|_| joined_at).collect();
    let columns: Vec<Arc<dyn Array>> = vec![
        Arc::new(StringArray::from(channel_ids)),
        Arc::new(StringArray::from(usernames)),
        Arc::new(Float64Array::from(joined_ats)),
    ];
    RecordBatch::try_new(schema, columns)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("member batch: {e}")))
}

/// GET /api/channels — channels the caller is a member of.
pub async fn list_channels(
    State(state): State<AppState>,
    Query(qp): Query<TokenQuery>,
) -> AppResult<Json<Vec<ChannelInfo>>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    let members_table = db_ops::open(&state.conn, "channel_members").await?;
    let my_rows =
        db_ops::scan_where(&members_table, &format!("username = '{username}'"), None).await?;
    let my_channel_ids = db_ops::collect_string_column(&my_rows, "channel_id");

    let mut out = Vec::with_capacity(my_channel_ids.len());
    for ch_id in my_channel_ids {
        if let Some(ch) = fetch_channel(&state, &ch_id).await? {
            out.push(channel_info_for(&state, ch).await?);
        }
    }
    Ok(Json(out))
}

/// GET /api/channels/browse — channels the caller is NOT a member of.
pub async fn browse_channels(
    State(state): State<AppState>,
    Query(qp): Query<TokenQuery>,
) -> AppResult<Json<Vec<ChannelBrowseItem>>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    let members_table = db_ops::open(&state.conn, "channel_members").await?;
    let my_rows =
        db_ops::scan_where(&members_table, &format!("username = '{username}'"), None).await?;
    let my_channel_ids: HashSet<String> = db_ops::collect_string_column(&my_rows, "channel_id")
        .into_iter()
        .collect();

    let channels = db_ops::open(&state.conn, "channels").await?;
    let mut stream = lancedb::query::ExecutableQuery::execute(&channels.query())
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("scan channels: {e}")))?;
    use futures_util::TryStreamExt;
    let mut all_batches: Vec<RecordBatch> = Vec::new();
    while let Some(b) = stream
        .try_next()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("scan channels: {e}")))?
    {
        all_batches.push(b);
    }
    let ids = db_ops::collect_string_column(&all_batches, "id");
    let names = db_ops::collect_string_column(&all_batches, "name");
    let created_bys = db_ops::collect_string_column(&all_batches, "created_by");

    let mut out = Vec::with_capacity(ids.len());
    for ((id, name), created_by) in ids.into_iter().zip(names).zip(created_bys) {
        if my_channel_ids.contains(&id) {
            continue;
        }
        let member_count = list_channel_member_usernames(&state, &id).await?.len();
        out.push(ChannelBrowseItem {
            id,
            name,
            created_by,
            member_count,
        });
    }
    Ok(Json(out))
}

/// POST /api/channels/{channel_id}/join
pub async fn join_channel(
    State(state): State<AppState>,
    AxPath(channel_id): AxPath<String>,
    Query(qp): Query<TokenQuery>,
) -> AppResult<Json<ChannelInfo>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    validate_id(&state.cfg, &channel_id)?;

    let ch = fetch_channel(&state, &channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Channel not found".into()))?;
    if is_channel_member(&state, &channel_id, &username).await? {
        return Err(AppError::BadRequest("Already a member".into()));
    }

    let members_table = db_ops::open(&state.conn, "channel_members").await?;
    let row = db_ops::mixed_row(
        db::channel_members_schema(),
        &[
            Cell::Str(&channel_id),
            Cell::Str(&username),
            Cell::F64(now_secs()),
        ],
    )?;
    db_ops::append(&members_table, row).await?;

    Ok(Json(channel_info_for(&state, ch).await?))
}

/// GET /api/channels/{channel_id}
pub async fn get_channel(
    State(state): State<AppState>,
    AxPath(channel_id): AxPath<String>,
    Query(qp): Query<TokenQuery>,
) -> AppResult<Json<ChannelInfo>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    validate_id(&state.cfg, &channel_id)?;
    if !is_channel_member(&state, &channel_id, &username).await? {
        return Err(AppError::Forbidden("Not a member of this channel"));
    }
    let ch = fetch_channel(&state, &channel_id)
        .await?
        .ok_or_else(|| AppError::NotFound("Channel not found".into()))?;
    Ok(Json(channel_info_for(&state, ch).await?))
}

/// POST /api/channels/{channel_id}/members
pub async fn add_channel_member(
    State(state): State<AppState>,
    AxPath(channel_id): AxPath<String>,
    Query(qp): Query<TokenQuery>,
    Json(req): Json<AddChannelMemberRequest>,
) -> AppResult<Json<serde_json::Value>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    validate_id(&state.cfg, &channel_id)?;
    validate_username(&state.cfg, &req.username)?;

    if !is_channel_member(&state, &channel_id, &username).await? {
        return Err(AppError::Forbidden("Not a member of this channel"));
    }

    let users_table = db_ops::open(&state.conn, "users").await?;
    let user_rows = db_ops::scan_where(
        &users_table,
        &format!("username = '{}'", req.username),
        Some(1),
    )
    .await?;
    if db_ops::total_rows(&user_rows) == 0 {
        return Err(AppError::NotFound("User not found".into()));
    }

    if is_channel_member(&state, &channel_id, &req.username).await? {
        return Err(AppError::BadRequest("User is already a member".into()));
    }

    let members_table = db_ops::open(&state.conn, "channel_members").await?;
    let row = db_ops::mixed_row(
        db::channel_members_schema(),
        &[
            Cell::Str(&channel_id),
            Cell::Str(&req.username),
            Cell::F64(now_secs()),
        ],
    )?;
    db_ops::append(&members_table, row).await?;

    Ok(Json(serde_json::json!({"status": "ok"})))
}

/// GET /api/channels/{channel_id}/members/public_keys
pub async fn get_channel_member_keys(
    State(state): State<AppState>,
    AxPath(channel_id): AxPath<String>,
    Query(qp): Query<TokenQuery>,
) -> AppResult<Json<Vec<ChannelMemberKey>>> {
    let username = require_auth(&state.cfg, &qp.token)?;
    validate_id(&state.cfg, &channel_id)?;
    if !is_channel_member(&state, &channel_id, &username).await? {
        return Err(AppError::Forbidden("Not a member of this channel"));
    }

    let members = list_channel_member_usernames(&state, &channel_id).await?;
    let users_table = db_ops::open(&state.conn, "users").await?;
    let mut out = Vec::with_capacity(members.len());
    for m in members {
        let rows =
            db_ops::scan_where(&users_table, &format!("username = '{m}'"), Some(1)).await?;
        if let Some(pk) = db_ops::first_string(&rows, "public_key_jwk") {
            out.push(ChannelMemberKey {
                username: m,
                public_key_jwk: pk,
            });
        }
    }
    Ok(Json(out))
}
