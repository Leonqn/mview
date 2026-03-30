use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::db::models::{Episode, Notification, Season};
use crate::db::queries;
use crate::telegram::notifications as tg_notify;
use crate::web::AppState;

const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60); // 6 hours

/// Main loop: check TMDB for new seasons of tracked TV series.
pub async fn run(state: Arc<AppState>) {
    info!(
        interval_hours = CHECK_INTERVAL.as_secs() / 3600,
        "tmdb check started"
    );

    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    // Skip the immediate first tick
    interval.tick().await;

    loop {
        interval.tick().await;

        if let Err(error) = check_new_seasons(&state).await {
            error!(?error, "tmdb check error");
        }
    }
}

/// Single check iteration: check TMDB for new seasons of tracked series and new parts of collections.
pub async fn check_new_seasons(state: &Arc<AppState>) -> Result<()> {
    let media_list = {
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_all_media(&conn)
        })
        .await??
    };

    let series: Vec<_> = media_list
        .iter()
        .filter(|m| m.media_type == "series" && m.status == "tracking" && m.tmdb_id.is_some())
        .cloned()
        .collect();

    let collections: Vec<_> = media_list
        .iter()
        .filter(|m| m.media_type == "movie" && m.status == "tracking" && m.tmdb_id.is_some())
        .cloned()
        .collect();

    if series.is_empty() && collections.is_empty() {
        debug!("no tracked series or collections to check on tmdb");
        return Ok(());
    }

    if !series.is_empty() {
        info!(
            count = series.len(),
            "checking tracked series on tmdb for new seasons"
        );
        for media in &series {
            if let Err(error) = check_single_series(state, media).await {
                error!(
                    title = media.title,
                    tmdb_id = media.tmdb_id.unwrap_or(0),
                    ?error,
                    "failed to check series"
                );
            }
        }
    }

    if !collections.is_empty() {
        info!(
            count = collections.len(),
            "checking tracked movies on tmdb for new collection parts"
        );
        for media in &collections {
            if let Err(error) = check_single_collection(state, media).await {
                error!(
                    title = media.title,
                    tmdb_id = media.tmdb_id.unwrap_or(0),
                    ?error,
                    "failed to check collection"
                );
            }
        }
    }

    Ok(())
}

/// Check a single movie entry on TMDB. If it's a collection (multiple seasons in DB),
/// look for new parts not yet tracked.
async fn check_single_collection(
    state: &Arc<AppState>,
    media: &crate::db::models::Media,
) -> Result<()> {
    let tmdb_id = media.tmdb_id.unwrap();
    let media_id = media.id;

    let pool = state.db.clone();
    let db_seasons = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_seasons_for_media(&conn, media_id)
    })
    .await??;

    // Standalone movies have exactly one season — nothing to monitor for new parts
    if db_seasons.len() <= 1 {
        debug!(
            title = media.title,
            "single-film entry, skipping collection check"
        );
        return Ok(());
    }

    let existing_titles: std::collections::HashSet<String> =
        db_seasons.iter().filter_map(|s| s.title.clone()).collect();

    let collection = state.tmdb.get_collection(tmdb_id).await?;

    let mut new_parts = collection.parts.clone();
    new_parts.sort_by(|a, b| a.release_date.cmp(&b.release_date));
    let new_parts: Vec<_> = new_parts
        .into_iter()
        .filter(|p| !existing_titles.contains(&p.title))
        .collect();

    if new_parts.is_empty() {
        debug!(title = media.title, "no new collection parts on tmdb");
        return Ok(());
    }

    info!(
        count = new_parts.len(),
        title = media.title,
        "found new collection part(s) on tmdb"
    );

    let next_season_number = db_seasons
        .iter()
        .map(|s| s.season_number)
        .max()
        .unwrap_or(0)
        + 1;

    for (idx, part) in new_parts.iter().enumerate() {
        let season_number = next_season_number + idx as i64;
        let air_date = part
            .release_date
            .as_ref()
            .and_then(|d| d.get(..10).map(|s| s.to_string()));

        let season = Season {
            id: 0,
            media_id,
            season_number,
            title: Some(part.title.clone()),
            episode_count: Some(1),
            anilist_id: None,
            format: Some("MOVIE".to_string()),
            status: "ignored".to_string(),
            created_at: String::new(),
        };

        let episode = Episode {
            id: 0,
            season_id: 0,
            episode_number: 1,
            title: Some(part.title.clone()),
            air_date,
            downloaded: false,
            file_path: None,
        };

        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let season_id = queries::insert_season(&conn, &season)?;
            let mut ep = episode;
            ep.season_id = season_id;
            queries::insert_episode(&conn, &ep)
        })
        .await??;

        let notification_msg = format!("New film in collection: {} — {}", media.title, part.title);
        let notif_media_id = media.id;
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_notification(
                &conn,
                &Notification {
                    id: 0,
                    media_id: Some(notif_media_id),
                    message: notification_msg,
                    notification_type: "new_season".to_string(),
                    read: false,
                    created_at: String::new(),
                },
            )
        })
        .await??;

        let tg_msg = tg_notify::format_new_season(&media.title, season_number);
        if let Err(error) =
            tg_notify::send_notification(&state.telegram_bot, state.telegram_chat_id, &tg_msg).await
        {
            warn!(?error, "failed to send telegram notification");
        }

        info!(
            title = media.title,
            part = part.title,
            "added new collection part"
        );
    }

    Ok(())
}

/// Check a single series on TMDB for new seasons not yet in our DB.
async fn check_single_series(
    state: &Arc<AppState>,
    media: &crate::db::models::Media,
) -> Result<()> {
    let tmdb_id = media.tmdb_id.unwrap();

    // Get current seasons from our DB
    let media_id = media.id;
    let pool = state.db.clone();
    let db_seasons = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_seasons_for_media(&conn, media_id)
    })
    .await??;

    let db_season_numbers: std::collections::HashSet<i64> =
        db_seasons.iter().map(|s| s.season_number).collect();

    // Fetch TMDB TV details
    let tv_details = state.tmdb.get_tv(tmdb_id).await?;

    let tmdb_seasons = match tv_details.seasons {
        Some(ref seasons) => seasons,
        None => {
            debug!(title = media.title, "no seasons info from tmdb");
            return Ok(());
        }
    };

    // Find new seasons (skip specials, season 0)
    let new_seasons: Vec<_> = tmdb_seasons
        .iter()
        .filter(|s| s.season_number > 0 && !db_season_numbers.contains(&s.season_number))
        .collect();

    if new_seasons.is_empty() {
        debug!(title = media.title, "no new seasons on tmdb");
        return Ok(());
    }

    info!(
        count = new_seasons.len(),
        title = media.title,
        "found new season(s) on tmdb"
    );

    for tmdb_season in &new_seasons {
        // Insert the new season
        let season = Season {
            id: 0,
            media_id: media.id,
            season_number: tmdb_season.season_number,
            title: tmdb_season.name.clone(),
            episode_count: tmdb_season.episode_count,
            anilist_id: None,
            format: Some("TV".to_string()),
            status: "tracking".to_string(),
            created_at: String::new(),
        };

        let pool = state.db.clone();
        let season_clone = season.clone();
        let season_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_season(&conn, &season_clone)
        })
        .await??;

        // Fetch episode details for the new season
        match state
            .tmdb
            .get_season(tmdb_id, tmdb_season.season_number)
            .await
        {
            Ok(season_details) => {
                if let Some(episodes) = season_details.episodes {
                    for ep in episodes {
                        let episode = Episode {
                            id: 0,
                            season_id,
                            episode_number: ep.episode_number,
                            title: ep.name,
                            air_date: ep.air_date,
                            downloaded: false,
                            file_path: None,
                        };
                        let pool = state.db.clone();
                        tokio::task::spawn_blocking(move || {
                            let conn = pool.get()?;
                            queries::insert_episode(&conn, &episode)
                        })
                        .await??;
                    }
                }
            }
            Err(error) => {
                warn!(
                    season_number = tmdb_season.season_number,
                    ?error,
                    "failed to fetch season episodes from tmdb"
                );
            }
        }

        // Create notification about the new season (no auto-download)
        let notification_msg = format!(
            "New season available: {} - Season {}",
            media.title, tmdb_season.season_number
        );
        let notif_media_id = media.id;
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_notification(
                &conn,
                &Notification {
                    id: 0,
                    media_id: Some(notif_media_id),
                    message: notification_msg,
                    notification_type: "new_season".to_string(),
                    read: false,
                    created_at: String::new(),
                },
            )
        })
        .await??;

        // Send Telegram notification
        let tg_msg = tg_notify::format_new_season(&media.title, tmdb_season.season_number);
        if let Err(error) =
            tg_notify::send_notification(&state.telegram_bot, state.telegram_chat_id, &tg_msg).await
        {
            warn!(?error, "failed to send telegram notification");
        }

        info!(
            season_number = tmdb_season.season_number,
            title = media.title,
            "added new season and created notification"
        );
    }

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
    use crate::db::models::Media;
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
    async fn test_check_new_seasons_no_series() {
        let state = build_test_state();
        let result = check_new_seasons(&state).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_check_new_seasons_filters_correctly() {
        let state = build_test_state();

        // Insert a series (no TMDB ID - should be skipped)
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_media(
                &conn,
                &Media {
                    id: 0,
                    media_type: "series".to_string(),
                    title: "No TMDB ID".to_string(),
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
            )?;
            // Insert a movie (should be skipped)
            queries::insert_media(
                &conn,
                &Media {
                    id: 0,
                    media_type: "movie".to_string(),
                    title: "A Movie".to_string(),
                    title_original: None,
                    year: None,
                    tmdb_id: Some(999),
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
            // Insert a completed series (should be skipped)
            queries::insert_media(
                &conn,
                &Media {
                    id: 0,
                    media_type: "series".to_string(),
                    title: "Completed Series".to_string(),
                    title_original: None,
                    year: None,
                    tmdb_id: Some(888),
                    imdb_id: None,
                    kinopoisk_url: None,
                    world_art_url: None,
                    poster_url: None,
                    overview: None,
                    anilist_id: None,
                    status: "completed".to_string(),
                    created_at: String::new(),
                    updated_at: String::new(),
                },
            )
        })
        .await
        .unwrap()
        .unwrap();

        // Should succeed (no qualifying series to check)
        let result = check_new_seasons(&state).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_notification_creation_for_new_season() {
        let pool = db::init_pool(":memory:").unwrap();
        let conn = pool.get().unwrap();

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
        )
        .unwrap();

        // Insert a "new season" notification
        let notification = Notification {
            id: 0,
            media_id: Some(media_id),
            message: "New season available: Test Series - Season 2".to_string(),
            notification_type: "new_season".to_string(),
            read: false,
            created_at: String::new(),
        };
        let notif_id = queries::insert_notification(&conn, &notification).unwrap();
        assert!(notif_id > 0);

        let unread = queries::get_unread_notifications(&conn).unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].notification_type, "new_season");
        assert!(unread[0].message.contains("Season 2"));
    }
}
