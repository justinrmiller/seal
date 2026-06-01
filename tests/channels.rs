mod common;

use common::TestServer;
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
    assert_eq!(res.status(), 200, "register {username}: {:?}", res.text().await);
    let body: Value = serde_json::from_str(&res.text().await.unwrap()).unwrap();
    body["token"].as_str().unwrap().to_string()
}

async fn create_channel(server: &TestServer, token: &str, name: &str, members: &[&str]) -> Value {
    let res = server
        .client()
        .post(server.url(&format!("/api/channels?token={token}")))
        .json(&json!({"name": name, "members": members}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "create {name}: {:?}", res.text().await);
    res.json().await.unwrap()
}

#[tokio::test]
async fn create_channel_basic() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    let body = create_channel(&s, &alice, "general", &["bob"]).await;
    assert_eq!(body["name"], "general");
    let members: Vec<&str> = body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(members.contains(&"alice"));
    assert!(members.contains(&"bob"));
}

#[tokio::test]
async fn create_channel_duplicate_name_fails() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    create_channel(&s, &alice, "unique-channel", &[]).await;
    let res = s
        .client()
        .post(s.url(&format!("/api/channels?token={alice}")))
        .json(&json!({"name": "unique-channel", "members": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert!(body["detail"].as_str().unwrap().to_lowercase().contains("already taken"));
}

#[tokio::test]
async fn create_channel_invalid_name_returns_400() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let res = s
        .client()
        .post(s.url(&format!("/api/channels?token={alice}")))
        .json(&json!({"name": "bad name!", "members": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn create_channel_nonexistent_member_returns_404() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let res = s
        .client()
        .post(s.url(&format!("/api/channels?token={alice}")))
        .json(&json!({"name": "test", "members": ["ghost"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn creator_auto_added_as_member() {
    let s = TestServer::spawn().await;
    let token = register(&s, "creator").await;
    let body = create_channel(&s, &token, "mychannel", &[]).await;
    let members: Vec<&str> = body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(members.contains(&"creator"));
}

#[tokio::test]
async fn list_channels_returns_only_mine() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    create_channel(&s, &alice, "ch1", &["bob"]).await;
    create_channel(&s, &alice, "ch2", &[]).await;
    let alice_list: Value = s
        .client()
        .get(s.url(&format!("/api/channels?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let alice_names: Vec<&str> = alice_list
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(alice_names.contains(&"ch1"));
    assert!(alice_names.contains(&"ch2"));

    let bob_list: Value = s
        .client()
        .get(s.url(&format!("/api/channels?token={bob}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_names: Vec<&str> = bob_list
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(bob_names.contains(&"ch1"));
    assert!(!bob_names.contains(&"ch2"));
}

#[tokio::test]
async fn list_channels_empty_when_none() {
    let s = TestServer::spawn().await;
    let token = register(&s, "solo").await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/channels?token={token}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res, json!([]));
}

#[tokio::test]
async fn browse_shows_non_member_channels() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    create_channel(&s, &alice, "open-channel", &[]).await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/channels/browse?token={bob}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = res
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"open-channel"), "got {names:?}");
}

#[tokio::test]
async fn browse_excludes_my_channels() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    create_channel(&s, &alice, "my-channel", &["bob"]).await;
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/channels/browse?token={bob}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let names: Vec<&str> = res
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"my-channel"));
}

#[tokio::test]
async fn join_channel_succeeds() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "joinable", &[]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/{ch_id}/join?token={bob}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    let members: Vec<&str> = body["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(members.contains(&"bob"));
}

#[tokio::test]
async fn join_when_already_member_returns_400() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "already-in", &["bob"]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/{ch_id}/join?token={bob}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn join_nonexistent_channel_returns_404() {
    let s = TestServer::spawn().await;
    let token = register(&s, "alice").await;
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/nonexistent-id/join?token={token}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}

#[tokio::test]
async fn invite_member_succeeds() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "invite-test", &[]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/{ch_id}/members?token={alice}")))
        .json(&json!({"username": "bob"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn invite_by_non_member_returns_403() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "no-invite", &[]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/{ch_id}/members?token={bob}")))
        .json(&json!({"username": "alice"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn invite_already_member_returns_400() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "dup-invite", &["bob"]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res = s
        .client()
        .post(s.url(&format!("/api/channels/{ch_id}/members?token={alice}")))
        .json(&json!({"username": "bob"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn get_channel_returns_info() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "info-test", &["bob"]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch_id}?token={bob}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["name"], "info-test");
}

#[tokio::test]
async fn get_channel_non_member_returns_403() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "private", &[]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res = s
        .client()
        .get(s.url(&format!("/api/channels/{ch_id}?token={bob}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn member_public_keys_returned() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "keys-test", &["bob"]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res: Value = s
        .client()
        .get(s.url(&format!("/api/channels/{ch_id}/members/public_keys?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entries = res.as_array().unwrap();
    let usernames: Vec<&str> = entries
        .iter()
        .map(|v| v["username"].as_str().unwrap())
        .collect();
    assert!(usernames.contains(&"alice"));
    assert!(usernames.contains(&"bob"));
    for m in entries {
        assert!(m["public_key_jwk"].is_string());
    }
}

#[tokio::test]
async fn member_public_keys_non_member_returns_403() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "secret", &[]).await;
    let ch_id = ch["id"].as_str().unwrap();
    let res = s
        .client()
        .get(s.url(&format!("/api/channels/{ch_id}/members/public_keys?token={bob}")))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 403);
}
