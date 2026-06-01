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
