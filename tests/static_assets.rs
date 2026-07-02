//! Covers the embedded index + static-asset handlers in `lib.rs`
//! (`index`, `serve_static`) which the API/WS tests never exercise.

mod common;

use common::TestServer;

#[tokio::test]
async fn index_is_served_as_html() {
    let s = TestServer::spawn().await;
    let res = s.client().get(s.url("/")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.contains("text/html"),
        "unexpected content-type: {content_type}"
    );
    assert!(!res.text().await.unwrap().is_empty());
}

#[tokio::test]
async fn static_asset_is_served_with_guessed_mime() {
    let s = TestServer::spawn().await;
    let res = s
        .client()
        .get(s.url("/static/style.css"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.contains("text/css"),
        "expected a CSS mime, got: {content_type}"
    );
}

#[tokio::test]
async fn nested_static_asset_is_served() {
    // Exercises a path with a subdirectory (the `{*path}` wildcard).
    let s = TestServer::spawn().await;
    let res = s
        .client()
        .get(s.url("/static/vendor/sodium.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(!res.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_static_asset_returns_404() {
    let s = TestServer::spawn().await;
    let res = s
        .client()
        .get(s.url("/static/does-not-exist.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
}
