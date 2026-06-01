//! Shared test harness: spin up the seal-server router on a random port
//! against a fresh LanceDB directory, expose a base URL for the test.

use std::net::SocketAddr;
use std::sync::Arc;

use seal_server::{
    build_router,
    config::Config,
    db,
    rate_limit::RateLimiter,
    ws::WsConnections,
    AppState,
};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

#[allow(dead_code)]
pub struct TestServer {
    pub base_url: String,
    pub state: AppState,
    _shutdown: tokio::sync::oneshot::Sender<()>,
    _join: JoinHandle<()>,
    _tempdir: TempDir,
}

impl TestServer {
    pub async fn spawn() -> Self {
        let tempdir = tempfile::tempdir().expect("create tempdir");
        let db_path = tempdir.path().join("test.lance");
        let cfg = Config::for_test(db_path, "test-secret-key-for-tests".into())
            .expect("load test config");

        let conn = db::connect(&cfg.database_path).await.expect("db connect");
        db::init_db(&conn).await.expect("init_db");

        let state = AppState {
            cfg: Arc::new(cfg),
            conn,
            rate_limiter: Arc::new(RateLimiter::new()),
            ws_connections: Arc::new(WsConnections::new()),
        };

        let router = build_router(state.clone())
            .into_make_service_with_connect_info::<SocketAddr>();
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });

        Self {
            base_url: format!("http://{addr}"),
            state,
            _shutdown: tx,
            _join: join,
            _tempdir: tempdir,
        }
    }

    pub fn client(&self) -> reqwest::Client {
        reqwest::Client::builder()
            .build()
            .expect("reqwest client")
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}
