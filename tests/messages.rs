mod common;

use std::sync::Arc;

use arrow_array::{Float64Array, RecordBatch, StringArray};
use common::TestServer;
use seal_server::{db, db_ops};
use serde_json::{json, Value};

async fn register(server: &TestServer, username: &str) -> String {
    let res = server
        .client()
        .post(server.url("/api/register"))
        .json(&json!({
            "username": username,
            "password": "pw",
            "public_key_jwk": format!("k-{username}"),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "register {username}");
    let body: Value = serde_json::from_str(&res.text().await.unwrap()).unwrap();
    body["token"].as_str().unwrap().to_string()
}

async fn create_channel(server: &TestServer, token: &str, name: &str, members: &[&str]) -> String {
    let res = server
        .client()
        .post(server.url(&format!("/api/channels?token={token}")))
        .json(&json!({"name": name, "members": members}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    body["id"].as_str().unwrap().to_string()
}

async fn insert_dm_row(
    server: &TestServer,
    sender: &str,
    recipient: &str,
    channel_id: &str,
    timestamp: f64,
) {
    let tbl = db_ops::open(&server.state.conn, "messages").await.unwrap();
    let batch = RecordBatch::try_new(
        db::messages_schema(),
        vec![
            Arc::new(StringArray::from(vec![uuid::Uuid::new_v4().to_string()])),
            Arc::new(StringArray::from(vec![sender])),
            Arc::new(StringArray::from(vec![recipient])),
            Arc::new(StringArray::from(vec![channel_id])),
            Arc::new(StringArray::from(vec!["encrypted-data"])),
            Arc::new(StringArray::from(vec!["nonce-data"])),
            Arc::new(StringArray::from(vec!["ephemeral-key"])),
            Arc::new(Float64Array::from(vec![timestamp])),
            Arc::new(StringArray::from(vec!["text"])),
            Arc::new(StringArray::from(vec![""])),
        ],
    )
    .unwrap();
    db_ops::append(&tbl, batch).await.unwrap();
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

#[tokio::test]
async fn dm_history_empty() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/messages/bob?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res, json!([]));
}

#[tokio::test]
async fn dm_history_merges_self_and_received() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    insert_dm_row(&s, "bob", "alice", "", now()).await;
    insert_dm_row(&s, "alice", "bob", "self", now()).await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/messages/bob?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let msgs = res.as_array().unwrap();
    assert_eq!(msgs.len(), 2);
}

#[tokio::test]
async fn dm_after_timestamp_filter() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    let pivot = now();
    insert_dm_row(&s, "bob", "alice", "", pivot - 100.0).await;
    insert_dm_row(&s, "bob", "alice", "", pivot + 100.0).await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/messages/bob?token={alice}&after={pivot}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let msgs = res.as_array().unwrap();
    assert_eq!(msgs.len(), 1);
}

#[tokio::test]
async fn dm_invalid_after_returns_400() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/messages/bob?token={alice}&after=-1")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn dm_self_copy_does_not_appear_as_received() {
    let s = TestServer::spawn().await;
    register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    insert_dm_row(&s, "alice", "bob", "self", now()).await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/messages/alice?token={bob}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res, json!([]));
}

#[tokio::test]
async fn channel_messages_empty() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "test-ch", &["bob"]).await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res, json!([]));
}

#[tokio::test]
async fn channel_messages_non_member_returns_403() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "private-ch", &[]).await;
    let res = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={bob}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn post_channel_message_text() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "chat", &["bob"]).await;
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .json(&json!({
            "channel_id": ch,
            "envelopes": [
                {"target_user": "alice", "ciphertext": "ct-alice", "iv": "iv-a", "sender_public_key_jwk": "epk-a"},
                {"target_user": "bob",   "ciphertext": "ct-bob",   "iv": "iv-b", "sender_public_key_jwk": "epk-b"},
            ],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["group_id"].is_string());

    let alice_msgs: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(alice_msgs.as_array().unwrap().len(), 1);
    assert_eq!(alice_msgs[0]["ciphertext"], "ct-alice");

    let bob_msgs: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={bob}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bob_msgs.as_array().unwrap().len(), 1);
    assert_eq!(bob_msgs[0]["ciphertext"], "ct-bob");
}

#[tokio::test]
async fn post_channel_message_non_member_403() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "private", &[]).await;
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/{ch}/messages?token={bob}")))
        .json(&json!({"channel_id": ch, "envelopes": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn post_image_message_creates_attachment() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "imgch", &["bob"]).await;
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .json(&json!({
            "channel_id": ch,
            "envelopes": [
                {"target_user": "alice", "ciphertext": "sk-a", "iv": "iv-a", "sender_public_key_jwk": "epk-a"},
                {"target_user": "bob",   "ciphertext": "sk-b", "iv": "iv-b", "sender_public_key_jwk": "epk-b"},
            ],
            "message_type": "image",
            "attachment": {"encrypted_data": "encrypted-image-blob", "iv": "att-nonce"},
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    let msgs: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(msgs[0]["message_type"], "image");
    assert!(!msgs[0]["attachment_id"].as_str().unwrap().is_empty());
    let _ = bob; // bob not used after channel creation
}

#[tokio::test]
async fn get_attachment_member_can_fetch() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let ch = create_channel(&s, &alice, "imgch2", &[]).await;
    s.client()
        .post(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .json(&json!({
            "channel_id": ch,
            "envelopes": [{"target_user": "alice", "ciphertext": "sk", "iv": "iv", "sender_public_key_jwk": "epk"}],
            "message_type": "image",
            "attachment": {"encrypted_data": "the-encrypted-blob", "iv": "the-nonce"},
        }))
        .send()
        .await
        .unwrap();
    let msgs: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let att_id = msgs[0]["attachment_id"].as_str().unwrap();
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/attachments/{att_id}?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["encrypted_data"], "the-encrypted-blob");
    assert_eq!(res["iv"], "the-nonce");
}

#[tokio::test]
async fn get_attachment_non_member_403() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "imgch3", &[]).await;
    s.client()
        .post(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .json(&json!({
            "channel_id": ch,
            "envelopes": [{"target_user": "alice", "ciphertext": "sk", "iv": "iv", "sender_public_key_jwk": "epk"}],
            "message_type": "image",
            "attachment": {"encrypted_data": "secret-blob", "iv": "nonce"},
        }))
        .send()
        .await
        .unwrap();
    let msgs: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let att_id = msgs[0]["attachment_id"].as_str().unwrap();
    let res = s
        .client()
        .get(s.url(&format!("/api/attachments/{att_id}?token={bob}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn get_attachment_not_found_returns_404() {
    let s = TestServer::spawn().await;
    let token = register(&s, "alice").await;
    let res = s
        .client()
        .get(s.url(&format!("/api/attachments/nonexistent-id?token={token}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn text_message_has_no_attachment() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let ch = create_channel(&s, &alice, "textch", &[]).await;
    s.client()
        .post(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .json(&json!({
            "channel_id": ch,
            "envelopes": [{"target_user": "alice", "ciphertext": "ct", "iv": "iv", "sender_public_key_jwk": "epk"}],
        }))
        .send()
        .await
        .unwrap();
    let msgs: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(msgs[0]["message_type"], "text");
    assert_eq!(msgs[0]["attachment_id"], "");
}

#[tokio::test]
async fn channel_messages_after_filter() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let ch = create_channel(&s, &alice, "filter-ch", &[]).await;
    let pivot = now();
    insert_dm_row(&s, "alice", "alice", &ch, pivot - 100.0).await;
    insert_dm_row(&s, "alice", "alice", &ch, pivot + 100.0).await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch}/messages?token={alice}&after={pivot}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res.as_array().unwrap().len(), 1);
}
