mod anilist;
mod config;
mod db;
mod error;
mod plex;
mod qbittorrent;
mod rutracker;
mod tasks;
mod telegram;
mod tmdb;
mod web;

use std::sync::Arc;

use config::Config;
use tracing::info;
use web::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config_path = Config::config_path_from_args();
    let config = Config::load(&config_path)?;

    let db_path = Config::db_path();
    let pool = db::init_pool(&db_path.to_string_lossy())?;
    info!(path = %db_path.display(), "database initialized");

    let addr = format!("{}:{}", config.server.host, config.server.port);

    // Spawn auth task for RuTracker login management
    let rt_config = Arc::new(config.rutracker.clone());
    let auth_handle = rutracker::auth::spawn_auth_task(rt_config.clone());

    let rt_client =
        rutracker::client::RutrackerClient::new(&config.rutracker.url, auth_handle.clone());

    let tmdb_client = tmdb::client::TmdbClient::new(&config.tmdb.api_key)?;
    let anilist_client = anilist::client::AniListClient::new()?;

    let qbt_config = Arc::new(config.qbittorrent.clone());
    let qbt_client = qbittorrent::client::QbtClient::new(qbt_config)?;

    let telegram_bot = teloxide::Bot::new(&config.telegram.bot_token);
    let telegram_chat_id = config.telegram.chat_id;

    let state = Arc::new(AppState {
        db: pool,
        rutracker: rt_client,
        tmdb: tmdb_client,
        anilist: anilist_client,
        qbittorrent: tokio::sync::Mutex::new(qbt_client),
        auth_handle,
        telegram_bot: telegram_bot.clone(),
        telegram_chat_id,
        config,
        templates: web::init_templates(),
    });

    // Spawn background tasks before starting the server
    let _task_handles = tasks::spawn_tasks(state.clone());

    // Start Telegram bot in the background
    tokio::spawn(telegram::bot::run_bot(state.clone()));

    let app = web::build_router(state);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = %addr, "mview listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("shutting down gracefully");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => { info!("received ctrl+c"); }
        _ = terminate => { info!("received SIGTERM"); }
    }
}
