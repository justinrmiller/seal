use std::sync::Arc;

use lancedb::arrow::arrow_schema::{DataType, Field, Schema, SchemaRef};
use lancedb::connection::Connection;
use lancedb::table::NewColumnTransform;

pub fn users_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("username", DataType::Utf8, true),
        Field::new("password_hash", DataType::Utf8, true),
        Field::new("public_key_jwk", DataType::Utf8, true),
    ]))
}

pub fn messages_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, true),
        Field::new("sender", DataType::Utf8, true),
        Field::new("recipient", DataType::Utf8, true),
        Field::new("channel_id", DataType::Utf8, true),
        Field::new("ciphertext", DataType::Utf8, true),
        Field::new("iv", DataType::Utf8, true),
        Field::new("sender_public_key_jwk", DataType::Utf8, true),
        Field::new("timestamp", DataType::Float64, true),
        Field::new("message_type", DataType::Utf8, true),
        Field::new("attachment_id", DataType::Utf8, true),
    ]))
}

pub fn attachments_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, true),
        Field::new("sender", DataType::Utf8, true),
        Field::new("channel_id", DataType::Utf8, true),
        Field::new("encrypted_data", DataType::LargeUtf8, true),
        Field::new("iv", DataType::Utf8, true),
        Field::new("timestamp", DataType::Float64, true),
    ]))
}

pub fn channels_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, true),
        Field::new("name", DataType::Utf8, true),
        Field::new("created_by", DataType::Utf8, true),
        Field::new("created_at", DataType::Float64, true),
    ]))
}

pub fn channel_members_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("channel_id", DataType::Utf8, true),
        Field::new("username", DataType::Utf8, true),
        Field::new("joined_at", DataType::Float64, true),
    ]))
}

pub async fn connect(path: &std::path::Path) -> anyhow::Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF8 database path: {}", path.display()))?;
    let conn = lancedb::connect(path_str).execute().await?;
    Ok(conn)
}

pub async fn init_db(conn: &Connection) -> anyhow::Result<()> {
    let existing: std::collections::HashSet<String> = conn
        .table_names()
        .execute()
        .await?
        .into_iter()
        .collect();

    if !existing.contains("users") {
        conn.create_empty_table("users", users_schema())
            .execute()
            .await?;
    }
    if !existing.contains("messages") {
        conn.create_empty_table("messages", messages_schema())
            .execute()
            .await?;
    } else {
        migrate_messages_table(conn).await?;
    }
    if !existing.contains("channels") {
        conn.create_empty_table("channels", channels_schema())
            .execute()
            .await?;
    }
    if !existing.contains("channel_members") {
        conn.create_empty_table("channel_members", channel_members_schema())
            .execute()
            .await?;
    }
    if !existing.contains("attachments") {
        conn.create_empty_table("attachments", attachments_schema())
            .execute()
            .await?;
    }
    Ok(())
}

/// Mirror of the Python migration in app/database.py: add `message_type` and
/// `attachment_id` columns to the messages table if they are missing.
async fn migrate_messages_table(conn: &Connection) -> anyhow::Result<()> {
    let tbl = conn.open_table("messages").execute().await?;
    let schema = tbl.schema().await?;
    let field_names: std::collections::HashSet<String> =
        schema.fields().iter().map(|f| f.name().clone()).collect();

    let mut new_columns: Vec<(String, String)> = Vec::new();
    if !field_names.contains("message_type") {
        new_columns.push(("message_type".to_string(), "'text'".to_string()));
    }
    if !field_names.contains("attachment_id") {
        new_columns.push(("attachment_id".to_string(), "''".to_string()));
    }
    if new_columns.is_empty() {
        return Ok(());
    }

    tracing::info!(
        "migrating messages table: adding columns {:?}",
        new_columns.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    tbl.add_columns(NewColumnTransform::SqlExpressions(new_columns), None)
        .await?;
    Ok(())
}
