//! Shared test harness: spin up the seal-server router on a random port
//! against a fresh LanceDB directory, expose a base URL for the test.

use std::sync::Arc;

use seal_server::{config::Config, serve, AppState};
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

        // Reuse the same bootstrap + serve path that `main` uses in production.
        let state = AppState::bootstrap(Arc::new(cfg))
            .await
            .expect("bootstrap state");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let server_state = state.clone();
        let join = tokio::spawn(async move {
            serve(server_state, listener, async {
                let _ = rx.await;
            })
            .await
            .expect("serve");
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
        reqwest::Client::builder().build().expect("reqwest client")
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}
