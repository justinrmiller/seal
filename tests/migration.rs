//! Verifies the `init_db` migration that adds `message_type` and `attachment_id`
//! columns to a pre-existing messages table written by the old Python schema.

use std::sync::Arc;

use arrow_array::{Float64Array, RecordBatch, RecordBatchIterator, StringArray};
use lancedb::arrow::arrow_schema::{DataType, Field, Schema};

/// The pre-migration messages schema (no `message_type`, no `attachment_id`).
fn legacy_messages_schema() -> Arc<Schema> {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, true),
        Field::new("sender", DataType::Utf8, true),
        Field::new("recipient", DataType::Utf8, true),
        Field::new("channel_id", DataType::Utf8, true),
        Field::new("ciphertext", DataType::Utf8, true),
        Field::new("iv", DataType::Utf8, true),
        Field::new("sender_public_key_jwk", DataType::Utf8, true),
        Field::new("timestamp", DataType::Float64, true),
    ]))
}

#[tokio::test]
async fn migrates_messages_table_with_legacy_schema() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("legacy.lance");

    // Step 1: write a messages table with the OLD schema and one row.
    let conn = seal_server::db::connect(&db_path, &Default::default()).await?;
    let legacy_schema = legacy_messages_schema();
    let row = RecordBatch::try_new(
        legacy_schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["m1"])),
            Arc::new(StringArray::from(vec!["alice"])),
            Arc::new(StringArray::from(vec!["bob"])),
            Arc::new(StringArray::from(vec![""])),
            Arc::new(StringArray::from(vec!["ct"])),
            Arc::new(StringArray::from(vec!["iv"])),
            Arc::new(StringArray::from(vec!["pk"])),
            Arc::new(Float64Array::from(vec![1.0_f64])),
        ],
    )?;
    let batches = RecordBatchIterator::new(vec![Ok(row)].into_iter(), legacy_schema.clone());
    let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(batches);
    conn.create_table("messages", reader).execute().await?;

    // The other four tables don't yet exist — init_db should create them
    // and migrate the messages table by adding the two missing columns.
    seal_server::db::init_db(&conn).await?;

    // Step 2: assert the new columns are present.
    let tbl = conn.open_table("messages").execute().await?;
    let schema = tbl.schema().await?;
    let field_names: std::collections::HashSet<String> =
        schema.fields().iter().map(|f| f.name().clone()).collect();
    assert!(
        field_names.contains("message_type"),
        "missing message_type after migration; got {field_names:?}"
    );
    assert!(
        field_names.contains("attachment_id"),
        "missing attachment_id after migration; got {field_names:?}"
    );

    // Step 3: read the row back and assert the default-filled values.
    use futures_util::TryStreamExt;
    use lancedb::query::{ExecutableQuery, QueryBase};
    let mut stream = tbl.query().limit(10).execute().await?;
    let mut batches: Vec<RecordBatch> = Vec::new();
    while let Some(b) = stream.try_next().await? {
        batches.push(b);
    }
    assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
    let batch = &batches[0];
    let mt_idx = batch
        .schema()
        .index_of("message_type")
        .expect("message_type column");
    let ai_idx = batch
        .schema()
        .index_of("attachment_id")
        .expect("attachment_id column");
    let mt = batch
        .column(mt_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("message_type is Utf8");
    let ai = batch
        .column(ai_idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("attachment_id is Utf8");
    assert_eq!(mt.value(0), "text");
    assert_eq!(ai.value(0), "");

    // Step 4: idempotency — calling init_db again must not change anything.
    seal_server::db::init_db(&conn).await?;
    let schema_after = conn
        .open_table("messages")
        .execute()
        .await?
        .schema()
        .await?;
    assert_eq!(schema_after.fields().len(), schema.fields().len());

    Ok(())
}

#[tokio::test]
async fn init_db_is_noop_on_current_schema() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let db_path = dir.path().join("fresh.lance");
    let conn = seal_server::db::connect(&db_path, &Default::default()).await?;
    seal_server::db::init_db(&conn).await?;
    // Second call must be a clean noop.
    seal_server::db::init_db(&conn).await?;
    let names: std::collections::HashSet<String> =
        conn.table_names().execute().await?.into_iter().collect();
    for required in [
        "users",
        "messages",
        "channels",
        "channel_members",
        "attachments",
    ] {
        assert!(names.contains(required), "missing table {required}");
    }
    Ok(())
}
