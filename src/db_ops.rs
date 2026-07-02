//! Small helpers around LanceDB Tables to keep route code uncluttered.

use std::sync::Arc;

use arrow_array::{
    Array, Float64Array, LargeStringArray, RecordBatch, RecordBatchIterator, StringArray,
};
use futures_util::TryStreamExt;
use lancedb::arrow::arrow_schema::SchemaRef;
use lancedb::connection::Connection;
use lancedb::query::{ExecutableQuery, QueryBase};
use lancedb::Table;

use crate::error::AppError;

/// Open a table by name. Wraps the lancedb error into AppError::Internal.
pub async fn open(conn: &Connection, name: &str) -> Result<Table, AppError> {
    conn.open_table(name)
        .execute()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("open_table({name}): {e}")))
}

/// Run a where-filtered scan and return all matching RecordBatches.
pub async fn scan_where(
    tbl: &Table,
    predicate: &str,
    limit: Option<usize>,
) -> Result<Vec<RecordBatch>, AppError> {
    let mut q = tbl.query().only_if(predicate);
    if let Some(n) = limit {
        q = q.limit(n);
    }
    let mut stream = q
        .execute()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("query: {e}")))?;
    let mut batches = Vec::new();
    while let Some(b) = stream
        .try_next()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("query stream: {e}")))?
    {
        batches.push(b);
    }
    Ok(batches)
}

/// Append rows to a table from a single RecordBatch.
pub async fn append(tbl: &Table, batch: RecordBatch) -> Result<(), AppError> {
    let schema = batch.schema();
    let iter = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
    let reader: Box<dyn arrow_array::RecordBatchReader + Send> = Box::new(iter);
    tbl.add(reader)
        .execute()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("add: {e}")))?;
    Ok(())
}

/// Convenience: get a Utf8 column from row 0 of the first non-empty batch.
pub fn first_string(batches: &[RecordBatch], column: &str) -> Option<String> {
    for b in batches {
        if b.num_rows() == 0 {
            continue;
        }
        let idx = b.schema().index_of(column).ok()?;
        let arr = b.column(idx).as_any().downcast_ref::<StringArray>()?;
        if arr.is_null(0) {
            return Some(String::new());
        }
        return Some(arr.value(0).to_string());
    }
    None
}

/// Convenience: get a LargeUtf8 column from row 0 of the first non-empty batch.
pub fn first_large_string(batches: &[RecordBatch], column: &str) -> Option<String> {
    for b in batches {
        if b.num_rows() == 0 {
            continue;
        }
        let idx = b.schema().index_of(column).ok()?;
        let arr = b.column(idx).as_any().downcast_ref::<LargeStringArray>()?;
        if arr.is_null(0) {
            return Some(String::new());
        }
        return Some(arr.value(0).to_string());
    }
    None
}

/// Convenience: build a single-row RecordBatch from `(column_name, value)` Utf8 pairs.
pub fn utf8_row(schema: SchemaRef, fields: &[(&str, &str)]) -> Result<RecordBatch, AppError> {
    let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(schema.fields().len());
    for f in schema.fields() {
        let name = f.name();
        let val = fields
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| *v)
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("missing field {name}")))?;
        columns.push(Arc::new(StringArray::from(vec![val])));
    }
    RecordBatch::try_new(schema, columns)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("record batch: {e}")))
}

pub fn total_rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(|b| b.num_rows()).sum()
}

/// Materialize messages-table rows into the API's StoredMessage shape.
pub fn rows_to_stored_messages(batches: &[RecordBatch]) -> Vec<crate::models::StoredMessage> {
    let mut out = Vec::new();
    for b in batches {
        let n = b.num_rows();
        if n == 0 {
            continue;
        }
        let getter = |name: &str| -> &StringArray {
            let idx = b.schema().index_of(name).expect("messages schema field");
            b.column(idx)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("Utf8 column")
        };
        let ts_idx = b.schema().index_of("timestamp").expect("timestamp field");
        let ts = b
            .column(ts_idx)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("Float64 timestamp");

        let id = getter("id");
        let sender = getter("sender");
        let recipient = getter("recipient");
        let channel_id = getter("channel_id");
        let ciphertext = getter("ciphertext");
        let iv = getter("iv");
        let spk = getter("sender_public_key_jwk");
        let mtype = getter("message_type");
        let att_id = getter("attachment_id");

        for i in 0..n {
            out.push(crate::models::StoredMessage {
                id: id.value(i).to_string(),
                sender: sender.value(i).to_string(),
                recipient: recipient.value(i).to_string(),
                channel_id: channel_id.value(i).to_string(),
                ciphertext: ciphertext.value(i).to_string(),
                iv: iv.value(i).to_string(),
                sender_public_key_jwk: spk.value(i).to_string(),
                timestamp: ts.value(i),
                message_type: mtype.value(i).to_string(),
                attachment_id: att_id.value(i).to_string(),
            });
        }
    }
    out
}

/// Build a single-row RecordBatch where each value is either a Utf8 string
/// or an f64. The `cells` slice must list every field in the schema in order.
pub enum Cell<'a> {
    Str(&'a str),
    LargeStr(&'a str),
    F64(f64),
}

pub fn mixed_row(schema: SchemaRef, cells: &[Cell<'_>]) -> Result<RecordBatch, AppError> {
    if cells.len() != schema.fields().len() {
        return Err(AppError::Internal(anyhow::anyhow!(
            "mixed_row: expected {} cells, got {}",
            schema.fields().len(),
            cells.len()
        )));
    }
    let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(cells.len());
    for cell in cells {
        match cell {
            Cell::Str(s) => columns.push(Arc::new(StringArray::from(vec![*s]))),
            Cell::LargeStr(s) => columns.push(Arc::new(LargeStringArray::from(vec![*s]))),
            Cell::F64(v) => columns.push(Arc::new(Float64Array::from(vec![*v]))),
        }
    }
    RecordBatch::try_new(schema, columns)
        .map_err(|e| AppError::Internal(anyhow::anyhow!("record batch: {e}")))
}

/// Collect every value from a Utf8 column across all batches into a Vec.
pub fn collect_string_column(batches: &[RecordBatch], column: &str) -> Vec<String> {
    let mut out = Vec::new();
    for b in batches {
        let Some(idx) = b.schema().index_of(column).ok() else {
            continue;
        };
        let Some(arr) = b.column(idx).as_any().downcast_ref::<StringArray>() else {
            continue;
        };
        for i in 0..arr.len() {
            if !arr.is_null(i) {
                out.push(arr.value(i).to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use lancedb::arrow::arrow_schema::{DataType, Field, Schema};

    /// A one-column Utf8 batch whose single row may be null.
    fn utf8_batch(column: &str, values: Vec<Option<&str>>) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new(column, DataType::Utf8, true)]));
        RecordBatch::try_new(schema, vec![Arc::new(StringArray::from(values))]).unwrap()
    }

    #[test]
    fn utf8_row_fills_fields_by_name_regardless_of_order() {
        let batch = utf8_row(
            db::users_schema(),
            &[
                ("public_key_jwk", "pk"),
                ("username", "alice"),
                ("password_hash", "h"),
            ],
        )
        .unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(first_string(&[batch.clone()], "username").as_deref(), Some("alice"));
        assert_eq!(first_string(&[batch], "public_key_jwk").as_deref(), Some("pk"));
    }

    #[test]
    fn utf8_row_errors_when_a_field_is_missing() {
        let err = utf8_row(db::users_schema(), &[("username", "alice")]).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn mixed_row_builds_mixed_typed_columns() {
        // attachments_schema: id, sender, channel_id (Utf8), encrypted_data
        // (LargeUtf8), iv (Utf8), timestamp (Float64).
        let batch = mixed_row(
            db::attachments_schema(),
            &[
                Cell::Str("att-1"),
                Cell::Str("alice"),
                Cell::Str("chan-1"),
                Cell::LargeStr("ciphertext"),
                Cell::Str("iv"),
                Cell::F64(42.0),
            ],
        )
        .unwrap();
        assert_eq!(batch.num_rows(), 1);
        assert_eq!(first_string(&[batch.clone()], "id").as_deref(), Some("att-1"));
        assert_eq!(
            first_large_string(&[batch], "encrypted_data").as_deref(),
            Some("ciphertext")
        );
    }

    #[test]
    fn mixed_row_errors_on_wrong_cell_count() {
        let err = mixed_row(db::attachments_schema(), &[Cell::Str("only-one")]).unwrap_err();
        assert!(matches!(err, AppError::Internal(_)));
    }

    #[test]
    fn first_string_reads_value_null_and_missing_cases() {
        // Present value.
        assert_eq!(
            first_string(&[utf8_batch("c", vec![Some("v")])], "c").as_deref(),
            Some("v")
        );
        // Null cell maps to an empty string, not None.
        assert_eq!(
            first_string(&[utf8_batch("c", vec![None])], "c").as_deref(),
            Some("")
        );
        // Missing column yields None.
        assert_eq!(first_string(&[utf8_batch("c", vec![Some("v")])], "nope"), None);
        // No non-empty batch yields None.
        assert_eq!(first_string(&[], "c"), None);
        assert_eq!(first_string(&[utf8_batch("c", vec![])], "c"), None);
    }

    #[test]
    fn first_string_skips_empty_batches_and_returns_first_populated() {
        let batches = vec![utf8_batch("c", vec![]), utf8_batch("c", vec![Some("second")])];
        assert_eq!(first_string(&batches, "c").as_deref(), Some("second"));
    }

    #[test]
    fn first_large_string_handles_null_and_wrong_type() {
        let schema = Arc::new(Schema::new(vec![Field::new("d", DataType::LargeUtf8, true)]));
        let null_batch =
            RecordBatch::try_new(schema, vec![Arc::new(LargeStringArray::from(vec![None as Option<&str>]))])
                .unwrap();
        assert_eq!(first_large_string(&[null_batch], "d").as_deref(), Some(""));
        // Column exists but is the wrong array type -> None (no panic).
        assert_eq!(first_large_string(&[utf8_batch("d", vec![Some("v")])], "d"), None);
    }

    #[test]
    fn total_rows_sums_across_batches() {
        let batches = vec![
            utf8_batch("c", vec![Some("a"), Some("b")]),
            utf8_batch("c", vec![Some("c")]),
            utf8_batch("c", vec![]),
        ];
        assert_eq!(total_rows(&batches), 3);
        assert_eq!(total_rows(&[]), 0);
    }

    #[test]
    fn collect_string_column_gathers_non_null_across_batches() {
        let batches = vec![
            utf8_batch("c", vec![Some("a"), None, Some("b")]),
            utf8_batch("c", vec![Some("c")]),
        ];
        assert_eq!(collect_string_column(&batches, "c"), vec!["a", "b", "c"]);
        // Missing column contributes nothing.
        assert!(collect_string_column(&batches, "missing").is_empty());
    }

    #[test]
    fn rows_to_stored_messages_maps_all_columns() {
        let batch = mixed_row(
            db::messages_schema(),
            &[
                Cell::Str("m1"),
                Cell::Str("alice"),
                Cell::Str("bob"),
                Cell::Str("chan"),
                Cell::Str("ct"),
                Cell::Str("iv"),
                Cell::Str("spk"),
                Cell::F64(123.5),
                Cell::Str("text"),
                Cell::Str("att"),
            ],
        )
        .unwrap();
        let msgs = rows_to_stored_messages(&[batch]);
        assert_eq!(msgs.len(), 1);
        let m = &msgs[0];
        assert_eq!(m.id, "m1");
        assert_eq!(m.sender, "alice");
        assert_eq!(m.recipient, "bob");
        assert_eq!(m.channel_id, "chan");
        assert_eq!(m.timestamp, 123.5);
        assert_eq!(m.message_type, "text");
        assert_eq!(m.attachment_id, "att");
        // Empty batches contribute no messages.
        assert!(rows_to_stored_messages(&[]).is_empty());
    }
}
