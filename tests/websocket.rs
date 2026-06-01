mod common;

use common::TestServer;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

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
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
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

async fn open_ws(
    server: &TestServer,
    token: &str,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let url = server
        .base_url
        .replacen("http://", "ws://", 1)
        + &format!("/ws/chat?token={token}");
    let (ws, _resp) = tokio_tungstenite::connect_async(url).await.expect("ws connect");
    ws
}

async fn recv_json(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
) -> Value {
    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("recv timeout")
        .expect("ws closed")
        .expect("ws error");
    let text = match msg {
        Message::Text(t) => t.to_string(),
        Message::Binary(b) => String::from_utf8(b.to_vec()).unwrap(),
        other => panic!("unexpected ws message: {other:?}"),
    };
    serde_json::from_str(&text).expect("ws text not json")
}

async fn send_json(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    v: Value,
) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

#[tokio::test]
async fn connect_with_valid_token_succeeds() {
    let s = TestServer::spawn().await;
    let token = register(&s, "alice").await;
    let mut ws = open_ws(&s, &token).await;
    // Cleanly close.
    ws.close(None).await.unwrap();
}

#[tokio::test]
async fn connect_with_invalid_token_is_rejected() {
    let s = TestServer::spawn().await;
    let url = s.base_url.replacen("http://", "ws://", 1) + "/ws/chat?token=bad-token";
    let result = tokio_tungstenite::connect_async(url).await;
    // Either the upgrade fails or the first frame is a close.
    if let Ok((mut ws, _)) = result {
        // We should immediately receive a close frame and no application data.
        let next = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("timeout")
            .expect("closed");
        match next.expect("ws err") {
            Message::Close(_) => {}
            other => panic!("expected close, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn dm_relayed_to_recipient_and_acked() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let mut ws_alice = open_ws(&s, &alice).await;
    let mut ws_bob = open_ws(&s, &bob).await;

    send_json(
        &mut ws_alice,
        json!({
            "type": "dm",
            "recipient": "bob",
            "ciphertext": "encrypted-hello",
            "iv": "nonce123",
            "sender_public_key_jwk": "epk123",
            "self_ciphertext": "self-ct",
            "self_iv": "self-iv",
            "self_sender_public_key_jwk": "self-epk",
        }),
    )
    .await;

    let ack = recv_json(&mut ws_alice).await;
    assert!(ack.get("ack").is_some());
    assert_eq!(ack["sender"], "alice");
    assert_eq!(ack["recipient"], "bob");

    let bob_msg = recv_json(&mut ws_bob).await;
    assert_eq!(bob_msg["type"], "dm");
    assert_eq!(bob_msg["sender"], "alice");
    assert_eq!(bob_msg["ciphertext"], "encrypted-hello");
}

#[tokio::test]
async fn dm_stored_with_self_copy_visible_to_sender_history() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let mut ws = open_ws(&s, &alice).await;

    send_json(
        &mut ws,
        json!({
            "type": "dm",
            "recipient": "bob",
            "ciphertext": "ct-for-bob",
            "iv": "iv1",
            "sender_public_key_jwk": "epk1",
            "self_ciphertext": "ct-for-self",
            "self_iv": "iv-self",
            "self_sender_public_key_jwk": "epk-self",
        }),
    )
    .await;
    recv_json(&mut ws).await; // ack
    ws.close(None).await.unwrap();

    let alice_history: Value = s
        .client()
        .get(s.url(&format!("/api/messages/bob?token={alice}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let alice_msgs = alice_history.as_array().unwrap();
    assert_eq!(alice_msgs.len(), 1);
    assert_eq!(alice_msgs[0]["sender"], "alice");
    assert_eq!(alice_msgs[0]["ciphertext"], "ct-for-self");

    let bob_history: Value = s
        .client()
        .get(s.url(&format!("/api/messages/alice?token={bob}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bob_msgs = bob_history.as_array().unwrap();
    assert_eq!(bob_msgs.len(), 1);
    assert_eq!(bob_msgs[0]["sender"], "alice");
    assert_eq!(bob_msgs[0]["ciphertext"], "ct-for-bob");
}

#[tokio::test]
async fn channel_message_relayed_via_ws() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "ws-channel", &["bob"]).await;
    let mut ws_alice = open_ws(&s, &alice).await;
    let mut ws_bob = open_ws(&s, &bob).await;

    send_json(
        &mut ws_alice,
        json!({
            "type": "channel",
            "channel_id": ch,
            "envelopes": [
                {"target_user": "alice", "ciphertext": "ct-alice", "iv": "iv-a", "sender_public_key_jwk": "epk-a"},
                {"target_user": "bob",   "ciphertext": "ct-bob",   "iv": "iv-b", "sender_public_key_jwk": "epk-b"},
            ],
        }),
    )
    .await;

    // Alice receives her own envelope relay + the ack (any order).
    let m1 = recv_json(&mut ws_alice).await;
    let m2 = recv_json(&mut ws_alice).await;
    let frames = [m1, m2];
    let acks: Vec<_> = frames.iter().filter(|f| f.get("ack").is_some()).collect();
    assert_eq!(acks.len(), 1);

    // Bob receives the channel-relayed copy.
    let bob_msg = recv_json(&mut ws_bob).await;
    assert_eq!(bob_msg["type"], "channel");
    assert_eq!(bob_msg["sender"], "alice");
    assert_eq!(bob_msg["ciphertext"], "ct-bob");
    assert_eq!(bob_msg["channel_id"], ch);
}

#[tokio::test]
async fn channel_message_non_member_gets_error() {
    let s = TestServer::spawn().await;
    let alice = register(&s, "alice").await;
    let bob = register(&s, "bob").await;
    let ch = create_channel(&s, &alice, "members-only", &[]).await;
    let mut ws_bob = open_ws(&s, &bob).await;

    send_json(
        &mut ws_bob,
        json!({"type": "channel", "channel_id": ch, "envelopes": []}),
    )
    .await;
    let resp = recv_json(&mut ws_bob).await;
    assert!(resp.get("error").is_some(), "got: {resp}");
}
