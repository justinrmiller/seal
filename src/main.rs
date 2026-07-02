use std::net::SocketAddr;
use std::sync::Arc;

use seal_server::{
    build_router, config::Config, db, rate_limit::RateLimiter, ws::WsConnections, AppState,
};

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

    let conn = db::connect(&cfg.database_path, &cfg.storage_options).await?;
    db::init_db(&conn).await?;

    let state = AppState {
        cfg: cfg.clone(),
        conn,
        rate_limiter: Arc::new(RateLimiter::new()),
        ws_connections: Arc::new(WsConnections::new()),
    };

    let app = build_router(state);

    let addr: SocketAddr = format!("{}:{}", cfg.app_host, cfg.app_port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
