use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::db::models::Notification;
use crate::db::queries;
use crate::rutracker::monitor;
use crate::telegram::notifications as tg_notify;
use crate::web::AppState;

const CHECK_INTERVAL: Duration = Duration::from_secs(30 * 60); // 30 minutes

/// Main loop: check all active auto-update torrents for RuTracker distribution updates.
pub async fn run(state: Arc<AppState>) {
    info!(
        interval_mins = CHECK_INTERVAL.as_secs() / 60,
        "rutracker check started"
    );

    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    // Skip the immediate first tick to avoid hammering RuTracker on startup
    interval.tick().await;

    loop {
        interval.tick().await;

        if let Err(error) = check_torrents(&state).await {
            error!(?error, "rutracker check error");
        }
    }
}

/// Single check iteration: get all auto-update torrents, check each for updates on RuTracker.
pub async fn check_torrents(state: &Arc<AppState>) -> Result<()> {
    let torrents = {
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_auto_update_torrents(&conn)
        })
        .await??
    };

    if torrents.is_empty() {
        debug!("no active auto-update torrents to check");
        return Ok(());
    }

    info!(
        count = torrents.len(),
        "checking auto-update torrents on rutracker"
    );

    for torrent in &torrents {
        if let Err(error) = check_single_torrent(state, torrent).await {
            error!(
                title = torrent.title,
                topic_id = torrent.rutracker_topic_id,
                ?error,
                "failed to check torrent"
            );
        }
    }

    Ok(())
}

/// Check a single torrent for updates on RuTracker.
/// If updated, download the new .torrent and re-add to qBittorrent.
async fn check_single_torrent(
    state: &Arc<AppState>,
    torrent: &crate::db::models::Torrent,
) -> Result<()> {
    let topic_id = &torrent.rutracker_topic_id;

    // If the torrent's season is already (or just became) completed, disable
    // auto_update and skip the network call — nothing will change on rutracker.
    let torrent_id = torrent.id;
    let media_id = torrent.media_id;
    let season_number = torrent.season_number;
    let pool = state.db.clone();
    let skip = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let Some(sn) = season_number else {
            return Ok::<bool, anyhow::Error>(false);
        };
        let seasons = queries::get_seasons_for_media(&conn, media_id)?;
        let Some(season) = seasons.iter().find(|s| s.season_number == sn) else {
            return Ok(false);
        };
        queries::check_and_complete_season(&conn, season.id)?;
        let refreshed = queries::get_seasons_for_media(&conn, media_id)?;
        let is_completed = refreshed
            .iter()
            .find(|s| s.season_number == sn)
            .map(|s| s.status == "completed")
            .unwrap_or(false);
        if is_completed {
            queries::update_torrent_auto_update(&conn, torrent_id, false)?;
        }
        Ok(is_completed)
    })
    .await??;
    if skip {
        debug!(
            title = torrent.title,
            topic_id, "season already completed, auto_update disabled"
        );
        return Ok(());
    }

    // Fetch current topic info from RuTracker
    let topic_info = state.rutracker.parse_topic(topic_id).await?;

    // Compare torrent hash (primary) or registered_at (fallback) to detect update
    let result = monitor::check_update(
        torrent,
        topic_info.registered_at.as_deref(),
        topic_info.torrent_hash.as_deref(),
    );

    // Update last_checked_at regardless
    let torrent_id = torrent.id;
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::update_torrent_checked(&conn, torrent_id)
    })
    .await??;

    // Update title in DB if it changed on rutracker
    if topic_info.title != torrent.title {
        let torrent_id = torrent.id;
        let new_title = topic_info.title.clone();
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::update_torrent_title(&conn, torrent_id, &new_title)
        })
        .await??;
    }

    if !result.has_update {
        // No download needed — safe to persist new metadata now
        if let Some(ref new_date) = result.new_registered_at {
            let torrent_id = torrent.id;
            let new_date = new_date.clone();
            let pool = state.db.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                queries::update_torrent_registered_at(&conn, torrent_id, &new_date)
            })
            .await??;
        }
        if let Some(ref new_hash) = result.new_torrent_hash {
            let torrent_id = torrent.id;
            let new_hash = new_hash.clone();
            let pool = state.db.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                queries::update_torrent_hash(&conn, torrent_id, &new_hash)
            })
            .await??;
        }
        return Ok(());
    }

    info!(
        title = torrent.title,
        topic_id, "torrent updated, re-downloading"
    );

    // Download updated .torrent file
    let torrent_bytes = state.rutracker.download_torrent(topic_id).await?;

    let filename = format!("{}.torrent", topic_id);
    let save_path = state.config.paths.download_dir.clone();

    // Re-add to qBittorrent (qBittorrent handles replacing existing torrents)
    {
        let mut qbt = state.qbittorrent.lock().await;
        qbt.ensure_logged_in().await?;
        qbt.add_torrent(&torrent_bytes, &filename, &save_path, "mview")
            .await?;
    }

    // Use torrent_hash from rutracker as qbt_hash
    let new_qbt_hash = result.new_torrent_hash.clone();

    // Download succeeded — now persist all new metadata and reset status
    let torrent_id = torrent.id;
    let new_registered_at = result.new_registered_at.clone();
    let new_torrent_hash = result.new_torrent_hash.clone();
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        if let Some(ref date) = new_registered_at {
            queries::update_torrent_registered_at(&conn, torrent_id, date)?;
        }
        if let Some(ref hash) = new_torrent_hash {
            queries::update_torrent_hash(&conn, torrent_id, hash)?;
        }
        if let Some(ref hash) = new_qbt_hash {
            queries::update_torrent_qbt_hash(&conn, torrent_id, hash)?;
        }
        queries::update_torrent_status(&conn, torrent_id, "active")
    })
    .await??;

    // Create notification about the update
    let media_id = torrent.media_id;
    let notification_msg = format!("Torrent updated: {}", topic_info.title);
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::insert_notification(
            &conn,
            &Notification {
                id: 0,
                media_id: Some(media_id),
                message: notification_msg,
                notification_type: "torrent_update".to_string(),
                read: false,
                created_at: String::new(),
            },
        )
    })
    .await??;

    // Send Telegram notification
    let tg_msg = tg_notify::format_torrent_update(&topic_info.title);
    if let Err(error) =
        tg_notify::send_notification(&state.telegram_bot, state.telegram_chat_id, &tg_msg).await
    {
        warn!(?error, "failed to send telegram notification");
    }

    info!(
        title = torrent.title,
        "torrent re-added to qbittorrent after update"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        Config, PathsConfig, PlexConfig, QbittorrentConfig, RutrackerConfig, ServerConfig,
        TelegramConfig, TmdbConfig,
    };
    use crate::db;
    use crate::db::models::{Media, Torrent};
    use crate::qbittorrent::client::QbtClient;
    use crate::rutracker::client::RutrackerClient;
    use crate::tmdb::client::TmdbClient;
    use crate::web::AppState;

    fn build_test_state() -> Arc<AppState> {
        let config = Config {
            rutracker: RutrackerConfig {
                url: "http://127.0.0.1:19999".to_string(),
                username: "user".to_string(),
                password: "pass".to_string(),
            },
            qbittorrent: QbittorrentConfig {
                url: "http://localhost:8080".to_string(),
                username: "admin".to_string(),
                password: "admin".to_string(),
            },
            tmdb: TmdbConfig {
                api_key: "fake".to_string(),
            },
            plex: PlexConfig::default(),
            telegram: TelegramConfig::default(),
            paths: PathsConfig {
                download_dir: "/tmp/downloads".to_string(),
                movies_dir: "/tmp/movies".to_string(),
                tv_dir: "/tmp/tv".to_string(),
                anime_dir: "/tmp/anime".to_string(),
            },
            server: ServerConfig::default(),
        };

        let rt_config = Arc::new(config.rutracker.clone());
        let auth_handle = crate::rutracker::auth::spawn_auth_task(rt_config);
        let rt_client = RutrackerClient::new(&config.rutracker.url, auth_handle.clone());
        let tmdb_client = TmdbClient::new(&config.tmdb.api_key).unwrap();
        let qbt_config = Arc::new(config.qbittorrent.clone());
        let qbt_client = QbtClient::new(qbt_config).unwrap();

        Arc::new(AppState {
            db: db::init_pool(":memory:").unwrap(),
            rutracker: rt_client,
            tmdb: tmdb_client,
            anilist: crate::anilist::client::AniListClient::new().unwrap(),
            qbittorrent: tokio::sync::Mutex::new(qbt_client),
            auth_handle,
            telegram_bot: teloxide::Bot::new("fake:token"),
            telegram_chat_id: 0,
            config,
            templates: crate::web::init_templates(),
        })
    }

    #[tokio::test]
    async fn test_check_torrents_no_auto_update() {
        let state = build_test_state();

        // With empty DB, should return Ok without errors
        let result = check_torrents(&state).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_check_torrents_with_torrent_in_db() {
        let state = build_test_state();

        // Insert a media and auto-update torrent
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let media_id = queries::insert_media(
                &conn,
                &Media {
                    id: 0,
                    media_type: "series".to_string(),
                    title: "Test Series".to_string(),
                    title_original: None,
                    year: Some(2024),
                    tmdb_id: Some(12345),
                    imdb_id: None,
                    kinopoisk_url: None,
                    world_art_url: None,
                    poster_url: None,
                    overview: None,
                    anilist_id: None,
                    status: "tracking".to_string(),
                    created_at: String::new(),
                    updated_at: String::new(),
                },
            )?;
            queries::insert_torrent(
                &conn,
                &Torrent {
                    id: 0,
                    media_id,
                    rutracker_topic_id: "123456".to_string(),
                    title: "Test.Series.S01".to_string(),
                    quality: Some("1080p".to_string()),
                    size_bytes: Some(5_000_000_000),
                    seeders: Some(10),
                    season_number: Some(1),
                    episode_info: Some("1-12".to_string()),
                    registered_at: Some("2024-01-15".to_string()),
                    last_checked_at: None,
                    torrent_hash: None,
                    qbt_hash: Some("abc123".to_string()),
                    status: "active".to_string(),
                    auto_update: true,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
            )
        })
        .await
        .unwrap()
        .unwrap();

        // Verify the torrent is found by the query
        let pool = state.db.clone();
        let torrents = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_auto_update_torrents(&conn)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(torrents.len(), 1);
        assert_eq!(torrents[0].rutracker_topic_id, "123456");
        assert!(torrents[0].auto_update);
    }

    #[test]
    fn test_update_torrent_registered_at() {
        let pool = db::init_pool(":memory:").unwrap();
        let conn = pool.get().unwrap();

        let media_id = queries::insert_media(
            &conn,
            &Media {
                id: 0,
                media_type: "series".to_string(),
                title: "Test".to_string(),
                title_original: None,
                year: None,
                tmdb_id: None,
                imdb_id: None,
                kinopoisk_url: None,
                world_art_url: None,
                poster_url: None,
                overview: None,
                anilist_id: None,
                status: "tracking".to_string(),
                created_at: String::new(),
                updated_at: String::new(),
            },
        )
        .unwrap();

        let torrent_id = queries::insert_torrent(
            &conn,
            &Torrent {
                id: 0,
                media_id,
                rutracker_topic_id: "123".to_string(),
                title: "Test".to_string(),
                quality: None,
                size_bytes: None,
                seeders: None,
                season_number: None,
                episode_info: None,
                registered_at: Some("2024-01-01".to_string()),
                last_checked_at: None,
                torrent_hash: None,
                qbt_hash: None,
                status: "active".to_string(),
                auto_update: true,
                created_at: String::new(),
                updated_at: String::new(),
            },
        )
        .unwrap();

        queries::update_torrent_registered_at(&conn, torrent_id, "2024-02-15").unwrap();

        let torrent = queries::get_torrent(&conn, torrent_id).unwrap().unwrap();
        assert_eq!(torrent.registered_at, Some("2024-02-15".to_string()));
    }
}
