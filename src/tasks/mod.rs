pub mod anilist_check;
pub mod download_monitor;
pub mod rutracker_check;
pub mod tmdb_check;

use std::sync::Arc;

use tracing::info;

use crate::web::AppState;

/// Spawn all background tasks. Returns JoinHandles for graceful shutdown.
pub fn spawn_tasks(state: Arc<AppState>) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::new();

    info!("spawning background tasks");

    handles.push(tokio::spawn(download_monitor::run(state.clone())));
    handles.push(tokio::spawn(rutracker_check::run(state.clone())));
    handles.push(tokio::spawn(tmdb_check::run(state.clone())));
    handles.push(tokio::spawn(anilist_check::run(state.clone())));

    handles
}
