mod common;

use common::TestServer;
use serde_json::{json, Value};

async fn post_json(server: &TestServer, path: &str, body: Value) -> reqwest::Response {
    server
        .client()
        .post(server.url(path))
        .json(&body)
        .send()
        .await
        .expect("request")
}

#[tokio::test]
async fn register_success() {
    let s = TestServer::spawn().await;
    let res = post_json(
        &s,
        "/api/register",
        json!({
            "username": "newuser",
            "password": "password123",
            "public_key_jwk": "dGVzdGtleQ==",
        }),
    )
    .await;
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["username"], "newuser");
    assert!(!body["token"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn register_duplicate_returns_400() {
    let s = TestServer::spawn().await;
    let first = post_json(
        &s,
        "/api/register",
        json!({"username": "existing", "password": "p1", "public_key_jwk": "a2V5"}),
    )
    .await;
    assert_eq!(first.status(), 200);
    let dup = post_json(
        &s,
        "/api/register",
        json!({"username": "existing", "password": "p2", "public_key_jwk": "a2V5"}),
    )
    .await;
    assert_eq!(dup.status(), 400);
    let body: Value = dup.json().await.unwrap();
    assert!(
        body["detail"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("already taken"),
        "unexpected body: {body}"
    );
}

#[tokio::test]
async fn register_invalid_username_returns_400() {
    let s = TestServer::spawn().await;
    let res = post_json(
        &s,
        "/api/register",
        json!({"username": "bad user!", "password": "p", "public_key_jwk": "a2V5"}),
    )
    .await;
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn register_empty_username_returns_400() {
    let s = TestServer::spawn().await;
    let res = post_json(
        &s,
        "/api/register",
        json!({"username": "", "password": "p", "public_key_jwk": "a2V5"}),
    )
    .await;
    assert_eq!(res.status(), 400);
}

#[tokio::test]
async fn register_missing_fields_returns_422() {
    let s = TestServer::spawn().await;
    let res = post_json(&s, "/api/register", json!({"username": "x"})).await;
    assert_eq!(res.status(), 422);
}

#[tokio::test]
async fn login_success_returns_token() {
    let s = TestServer::spawn().await;
    post_json(
        &s,
        "/api/register",
        json!({"username": "alice", "password": "secret", "public_key_jwk": "a2V5"}),
    )
    .await
    .error_for_status()
    .unwrap();
    let res = post_json(
        &s,
        "/api/login",
        json!({"username": "alice", "password": "secret"}),
    )
    .await;
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["username"], "alice");
    assert!(!body["token"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn login_wrong_password_returns_401() {
    let s = TestServer::spawn().await;
    post_json(
        &s,
        "/api/register",
        json!({"username": "bob", "password": "correct", "public_key_jwk": "a2V5"}),
    )
    .await
    .error_for_status()
    .unwrap();
    let res = post_json(
        &s,
        "/api/login",
        json!({"username": "bob", "password": "wrong"}),
    )
    .await;
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn login_nonexistent_user_returns_401() {
    let s = TestServer::spawn().await;
    let res = post_json(
        &s,
        "/api/login",
        json!({"username": "ghost", "password": "p"}),
    )
    .await;
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn rate_limit_register_kicks_in_at_21st_request() {
    let s = TestServer::spawn().await;
    for i in 0..20 {
        let res = post_json(
            &s,
            "/api/register",
            json!({
                "username": format!("rl{i}"),
                "password": "p",
                "public_key_jwk": "a2V5",
            }),
        )
        .await;
        assert!(
            res.status() == 200 || res.status() == 400,
            "iteration {i} got status {}",
            res.status()
        );
    }
    let res = post_json(
        &s,
        "/api/register",
        json!({"username": "rl_over", "password": "p", "public_key_jwk": "a2V5"}),
    )
    .await;
    assert_eq!(res.status(), 429);
}

#[tokio::test]
async fn rate_limit_login_kicks_in_at_21st_request() {
    let s = TestServer::spawn().await;
    post_json(
        &s,
        "/api/register",
        json!({"username": "rluser", "password": "p", "public_key_jwk": "a2V5"}),
    )
    .await
    .error_for_status()
    .unwrap();
    // After the register call we've used 1 of 20 slots; do 19 more logins to hit the cap,
    // then the 21st (counting the original register) returns 429.
    for _ in 0..19 {
        let res = post_json(
            &s,
            "/api/login",
            json!({"username": "rluser", "password": "p"}),
        )
        .await;
        assert_eq!(res.status(), 200);
    }
    let res = post_json(
        &s,
        "/api/login",
        json!({"username": "rluser", "password": "p"}),
    )
    .await;
    assert_eq!(res.status(), 429);
}

#[tokio::test]
async fn login_uses_bcrypt_python_compatible_hashes() {
    // Pre-computed bcrypt hash of "password123" (cost=12, $2b$) — generated by the
    // Python bcrypt library. Confirms the Rust bcrypt crate verifies hashes written
    // by the Python server so existing user rows remain valid.
    let python_hash = "$2b$12$sv1BSkiWX/zCN2JJw38rNuX9ocwonvZSeeSQ/7CEqczRSnfklkWty";
    assert!(bcrypt::verify("password123", python_hash).unwrap());
    assert!(!bcrypt::verify("wrong", python_hash).unwrap());
}
