use auren_server::api;
use auren_server::state::AppState;

use anyhow::Context;
use axum::Router;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=debug".into()),
        )
        .init();

    let config = auren_server::config::AppConfig::from_env()?;

    // Create the persistent directories at startup (spec section 4).
    for dir in [&config.skills_dir, &config.workspace_dir] {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory {dir}"))?;
    }
    if let Some(parent) = std::path::Path::new(&config.database_url)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        let _ = std::fs::create_dir_all(parent);
    }
    let pool = auren_server::db::connect(&config.database_url).await?;
    auren_server::db::migrate(&pool).await.context("database migrations failed")?;

    let state = AppState {
        db: pool,
        config: config.clone(),
        events: std::sync::Arc::new(auren_server::events::EventBus::new()),
        ws_tickets: std::sync::Arc::new(auren_server::events::TicketStore::new()),
        rate_limiter: std::sync::Arc::new(auren_server::api::rate_limit::RateLimiter::new()),
    };

    // Background scheduler and queue worker.
    tokio::spawn(auren_server::services::worker::run(state.clone()));
    let app: Router = api::router(state).layer(TraceLayer::new_for_http());

    let addr = config.bind_addr.clone();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    tracing::info!("Auren-AI server listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}