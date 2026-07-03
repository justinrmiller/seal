//! Object-storage smoke test: connect the database to an S3-compatible
//! endpoint, run `init_db`, then write and read a row back.
//!
//! Ignored by default (normal CI has no object store). Run it against any
//! S3-compatible endpoint — moto (no Docker), LocalStack, MinIO, or real S3.
//!
//! Docker-free, using moto:
//!
//! ```text
//! uv venv /tmp/motoenv && uv pip install --python /tmp/motoenv/bin/python "moto[server]"
//! /tmp/motoenv/bin/moto_server -p 5001 &
//! AWS_ACCESS_KEY_ID=test AWS_SECRET_ACCESS_KEY=test AWS_DEFAULT_REGION=us-east-1 \
//!   aws --endpoint-url http://localhost:5001 s3 mb s3://seal
//!
//! SEAL_TEST_S3_URI=s3://seal/it.lance \
//! SEAL_TEST_S3_ENDPOINT=http://localhost:5001 \
//!   cargo test --test object_storage -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use seal_server::db_ops::Cell;
use seal_server::{db, db_ops};

/// Unlike the round-trip test below, this one needs NO object store and runs in
/// normal CI. It drives the startup path (connect + init_db) against an
/// unreachable endpoint with the retry budget zeroed and asserts it fails FAST
/// rather than hanging on lance-io's ~180s default. Regression test for the
/// bounded-retry improvement. (`db::connect` alone is lazy for dir-based
/// namespaces, so `init_db` is what forces the network round-trip.)
#[tokio::test]
async fn unreachable_object_store_fails_fast() {
    let mut opts = HashMap::new();
    opts.insert("aws_endpoint".into(), "http://127.0.0.1:1".into());
    opts.insert("allow_http".into(), "true".into());
    opts.insert("aws_region".into(), "us-east-1".into());
    opts.insert("aws_access_key_id".into(), "test".into());
    opts.insert("aws_secret_access_key".into(), "test".into());
    // Zero the retry budget so a refused connection surfaces immediately
    // instead of retrying for lance-io's default 180 seconds.
    opts.insert("client_max_retries".into(), "0".into());
    opts.insert("client_retry_timeout".into(), "1".into());

    let startup = async {
        let conn = db::connect(Path::new("s3://seal-unreachable/it.lance"), &opts).await?;
        db::init_db(&conn).await
    };
    let result = tokio::time::timeout(Duration::from_secs(30), startup)
        .await
        .expect("startup against an unreachable bucket should fail fast, not hang");

    assert!(
        result.is_err(),
        "startup against an unreachable object store must return an error"
    );
}

/// Build LanceDB storage options from the test environment. Falls back to the
/// conventional moto/LocalStack dummy credentials.
fn storage_options() -> HashMap<String, String> {
    let mut opts = HashMap::new();
    if let Ok(endpoint) = std::env::var("SEAL_TEST_S3_ENDPOINT") {
        opts.insert("aws_endpoint".into(), endpoint);
        // Custom endpoints are plain HTTP in local mocks.
        opts.insert("allow_http".into(), "true".into());
    }
    opts.insert(
        "aws_region".into(),
        std::env::var("SEAL_TEST_S3_REGION").unwrap_or_else(|_| "us-east-1".into()),
    );
    opts.insert(
        "aws_access_key_id".into(),
        std::env::var("AWS_ACCESS_KEY_ID").unwrap_or_else(|_| "test".into()),
    );
    opts.insert(
        "aws_secret_access_key".into(),
        std::env::var("AWS_SECRET_ACCESS_KEY").unwrap_or_else(|_| "test".into()),
    );
    opts
}

#[tokio::test]
#[ignore = "requires an S3-compatible endpoint; see the file header to run it"]
async fn s3_backed_database_round_trips() {
    let uri = std::env::var("SEAL_TEST_S3_URI")
        .expect("set SEAL_TEST_S3_URI (e.g. s3://seal/it.lance) to run this test");
    let opts = storage_options();

    // Connect + migrate against object storage.
    let conn = db::connect(Path::new(&uri), &opts)
        .await
        .expect("connect to object storage");
    db::init_db(&conn).await.expect("init_db on object storage");

    // Every table should have been created in the bucket.
    let tables: std::collections::HashSet<String> = conn
        .table_names()
        .execute()
        .await
        .expect("table_names")
        .into_iter()
        .collect();
    for expected in ["users", "messages", "channels", "channel_members", "attachments"] {
        assert!(tables.contains(expected), "missing table {expected}");
    }

    // Write a row and read it back — proving both directions over S3.
    let users = db_ops::open(&conn, "users").await.expect("open users");
    let username = format!("zoe-{}", uuid::Uuid::new_v4());
    let row = db_ops::mixed_row(
        db::users_schema(),
        &[
            Cell::Str(&username),
            Cell::Str("hash"),
            Cell::Str("public-key"),
        ],
    )
    .expect("build row");
    db_ops::append(&users, row).await.expect("append row");

    let found = db_ops::scan_where(&users, &format!("username = '{username}'"), Some(1))
        .await
        .expect("scan");
    assert_eq!(db_ops::total_rows(&found), 1, "row should be readable back");
    assert_eq!(
        db_ops::first_string(&found, "public_key_jwk").as_deref(),
        Some("public-key"),
    );
}
