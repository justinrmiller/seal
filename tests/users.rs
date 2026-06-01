mod common;

use std::sync::Arc;

use arrow_array::{Float64Array, RecordBatch, StringArray};
use common::TestServer;
use seal_server::{db, db_ops};
use serde_json::{json, Value};

/// Register a user and return the JWT.
async fn register(server: &TestServer, username: &str, public_key_jwk: &str) -> String {
    let res = server
        .client()
        .post(server.url("/api/register"))
        .json(&json!({
            "username": username,
            "password": "pw",
            "public_key_jwk": public_key_jwk,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "register failed: {:?}", res.text().await);
    let body: Value = serde_json::from_str(&res.text().await.unwrap()).unwrap();
    body["token"].as_str().unwrap().to_string()
}

/// Insert a single message row directly via the LanceDB connection on the
/// running server. Mirrors the Python test that adds a row to validate the
/// list_users DM-history path.
async fn insert_self_dm(server: &TestServer, sender: &str, recipient: &str) {
    let tbl = db_ops::open(&server.state.conn, "messages").await.unwrap();
    let schema = db::messages_schema();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(StringArray::from(vec![uuid::Uuid::new_v4().to_string()])),
            Arc::new(StringArray::from(vec![sender])),
            Arc::new(StringArray::from(vec![recipient])),
            Arc::new(StringArray::from(vec!["self"])),
            Arc::new(StringArray::from(vec!["ct"])),
            Arc::new(StringArray::from(vec!["iv"])),
            Arc::new(StringArray::from(vec!["spk"])),
            Arc::new(Float64Array::from(vec![now])),
            Arc::new(StringArray::from(vec!["text"])),
            Arc::new(StringArray::from(vec![""])),
        ],
    )
    .unwrap();
    db_ops::append(&tbl, batch).await.unwrap();
}

#[tokio::test]
async fn no_dm_contacts_initially() {
    let s = TestServer::spawn().await;
    let token = register(&s, "alice", "YWxpY2VrZXk=").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/users?token={token}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body, json!([]));
}

#[tokio::test]
async fn dm_contacts_appear_after_self_copy() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice", "YWxpY2VrZXk=").await;
    register(&s, "bob", "Ym9ia2V5").await;
    insert_self_dm(&s, "alice", "bob").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/users?token={alice}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    let usernames: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["username"].as_str().unwrap())
        .collect();
    assert!(usernames.contains(&"bob"), "got: {usernames:?}");
}

#[tokio::test]
async fn search_finds_users_by_prefix() {
    let s = TestServer::spawn().await;
    register(&s, "alice", "k1").await;
    let bob = register(&s, "bob", "k2").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/users/search?q=ali&token={bob}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["username"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"alice"), "got: {names:?}");
}

#[tokio::test]
async fn search_excludes_self() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice", "k1").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/users/search?q=ali&token={alice}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["username"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"alice"), "got: {names:?}");
}

#[tokio::test]
async fn search_no_results_returns_empty() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice", "k1").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/users/search?q=zzz&token={alice}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body, json!([]));
}

#[tokio::test]
async fn search_invalid_query_returns_400() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice", "k1").await;
    // Bad chars are validated against the username regex.
    let res = s
        .client()
        .get(s.url(&format!("/api/users/search?q=bad+query!&token={alice}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn public_key_lookup_succeeds() {
    let s = TestServer::spawn().await;
    register(&s, "alice", "YWxpY2VrZXk=").await;
    let bob = register(&s, "bob", "Ym9ia2V5").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/users/alice/public_key?token={bob}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["username"], "alice");
    assert_eq!(body["public_key_jwk"], "YWxpY2VrZXk=");
}

#[tokio::test]
async fn public_key_lookup_not_found() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice", "k").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/users/nobody/public_key?token={alice}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn list_users_rejects_bad_token() {
    let s = TestServer::spawn().await;
    let res = s
        .client()
        .get(s.url("/api/users?token=garbage"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}
