use std::sync::Arc;

use seal_server::{config::Config, run};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();

    let cfg = Arc::new(Config::load()?);
    tracing::info!(
        "starting {} on {}:{}",
        cfg.app_title,
        cfg.app_host,
        cfg.app_port
    );

    // All wiring lives in `seal_server::run` (bootstrap + bind + serve) so it is
    // exercised by tests; `main` is just the process/tracing entrypoint.
    run(cfg).await
}
