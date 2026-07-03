use std::sync::Arc;

use anyhow::Context;
use lancedb::arrow::arrow_schema::{DataType, Field, Schema, SchemaRef};
use lancedb::connection::Connection;
use lancedb::table::NewColumnTransform;

use crate::config::{is_object_store_uri, redact_location};

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

pub async fn connect(
    location: &std::path::Path,
    storage_options: &std::collections::HashMap<String, String>,
) -> anyhow::Result<Connection> {
    let path_str = location
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("non-UTF8 database path: {}", location.display()))?;

    let remote = is_object_store_uri(path_str);
    let mut options = storage_options.clone();
    if remote {
        // Object storage: hand the URI to LanceDB and let object_store manage it.
        // No local directory to create. Bound the retry budget so an unreachable
        // bucket fails fast (lance-io's default is 180s) — the operator can still
        // override via the `storage:` block. `client_retry_timeout` is the exact
        // case-insensitive key lance-io reads from this map.
        if !options
            .keys()
            .any(|k| k.eq_ignore_ascii_case("client_retry_timeout"))
        {
            options.insert("client_retry_timeout".to_string(), "30".to_string());
        }
    } else if let Some(parent) = location.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating data directory {}", parent.display()))?;
    }

    let mut builder = lancedb::connect(path_str);
    if !options.is_empty() {
        builder = builder.storage_options(options);
    }
    builder.execute().await.with_context(|| {
        let redacted = redact_location(path_str);
        if remote {
            // Remote failures are usually credentials/region/endpoint/TLS, none
            // of which LanceDB's raw error makes obvious.
            format!(
                "connecting to object-store database at {redacted}. Check the bucket exists and \
                 that credentials, region, and endpoint are set (via the `storage:` block or \
                 standard cloud env vars); for a custom HTTP endpoint set `allow_http=true`."
            )
        } else {
            format!("connecting to database at {redacted}")
        }
    })
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
