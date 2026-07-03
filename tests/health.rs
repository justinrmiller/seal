//! Liveness/readiness probe endpoints.

mod common;

use common::TestServer;

#[tokio::test]
async fn health_returns_ok_without_touching_the_db() {
    let server = TestServer::spawn().await;
    let res = server
        .client()
        .get(server.url("/health"))
        .send()
        .await
        .expect("request");
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.expect("json");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn readyz_reports_ready_when_the_db_is_reachable() {
    let server = TestServer::spawn().await;
    let res = server
        .client()
        .get(server.url("/readyz"))
        .send()
        .await
        .expect("request");
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.expect("json");
    assert_eq!(body["status"], "ready");
}
