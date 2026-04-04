use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use chrono::{Datelike, Local};
use tracing::{debug, error, info, warn};

use crate::db::models::Notification;
use crate::db::queries;
use crate::search;
use crate::telegram::notifications as tg_notify;
use crate::web::AppState;

const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

pub async fn run(state: Arc<AppState>) {
    info!(
        interval_minutes = CHECK_INTERVAL.as_secs() / 60,
        "torrent search started"
    );

    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    interval.tick().await;

    loop {
        interval.tick().await;

        if let Err(error) = search_missing_torrents(&state).await {
            error!(?error, "torrent search error");
        }
    }
}

async fn search_missing_torrents(state: &Arc<AppState>) -> Result<()> {
    let pool = state.db.clone();
    let candidates = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let media_list = queries::get_all_media(&conn)?;
        let today = Local::now().format("%Y-%m-%d").to_string();
        let current_year = Local::now().year();

        let mut results = Vec::new();

        for media in &media_list {
            if media.status != "tracking" {
                continue;
            }

            let seasons = queries::get_tracking_seasons_for_media(&conn, media.id)?;
            let torrents = queries::get_torrents_for_media(&conn, media.id)?;
            let all_seasons = queries::get_seasons_for_media(&conn, media.id)?;

            for season in &seasons {
                // Skip if season already has a torrent
                let has_torrent = torrents
                    .iter()
                    .any(|t| t.season_number == Some(season.season_number));
                if has_torrent {
                    continue;
                }

                // Check if season has aired episodes
                let episodes = queries::get_episodes_for_season(&conn, season.id)?;
                let media_released = media
                    .year
                    .map(|y| y <= current_year as i64)
                    .unwrap_or(false);
                let has_aired = episodes.iter().any(|e| {
                    e.air_date
                        .as_deref()
                        .map(|d| d <= today.as_str())
                        .unwrap_or(media_released)
                });
                if !has_aired {
                    continue;
                }

                // Compute TV-only season number (same logic as search_season)
                let tv_num = all_seasons
                    .iter()
                    .filter(|s| matches!(s.format.as_deref(), Some("TV") | Some("TV_SHORT") | None))
                    .position(|s| s.id == season.id)
                    .map(|p| (p as i64) + 1)
                    .unwrap_or(1);

                results.push((media.clone(), season.clone(), tv_num));
            }
        }

        Ok::<_, anyhow::Error>(results)
    })
    .await??;

    if candidates.is_empty() {
        debug!("no seasons need torrent search");
        return Ok(());
    }

    info!(
        count = candidates.len(),
        "searching torrents for tracked seasons"
    );

    for (media, season, tv_num) in &candidates {
        if let Err(error) = search_single_season(state, media, season, *tv_num).await {
            error!(
                title = media.title,
                season = season.season_number,
                ?error,
                "failed to search torrents for season"
            );
        }
    }

    Ok(())
}

async fn search_single_season(
    state: &Arc<AppState>,
    media: &crate::db::models::Media,
    season: &crate::db::models::Season,
    tv_season_number: i64,
) -> Result<()> {
    let sq = search::build_queries(media, season, tv_season_number);

    // Only use primary queries (TV-N/ТВ-N), no fallback
    let queries = &sq.primary;

    info!(
        media_title = media.title,
        season = season.season_number,
        queries = ?queries,
        "auto-searching season torrents"
    );

    let futures: Vec<_> = queries.iter().map(|q| state.rutracker.search(q)).collect();
    let results = futures::future::join_all(futures).await;

    let mut total_count: i64 = 0;
    for result in results {
        match result {
            Ok(r) => total_count += r.len() as i64,
            Err(error) => {
                warn!(?error, "rutracker auto-search failed");
            }
        }
    }

    // Update search cache
    let pool = state.db.clone();
    let season_id = season.id;
    let media_id = media.id;
    let season_num = season.season_number;
    let media_title = media.title.clone();
    let was_found = total_count > 0;

    // Check if this is a new discovery (wasn't found before)
    let is_new_discovery = if was_found {
        let pool2 = state.db.clone();
        let sid = season_id;
        tokio::task::spawn_blocking(move || {
            let conn = pool2.get()?;
            let existing = queries::get_search_cache_for_season(&conn, sid)?;
            Ok::<_, anyhow::Error>(existing.map(|c| c.results_count == 0).unwrap_or(true))
        })
        .await??
    } else {
        false
    };

    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::upsert_search_cache(&conn, season_id, total_count)?;

        if is_new_discovery {
            queries::insert_notification(
                &conn,
                &Notification {
                    id: 0,
                    media_id: Some(media_id),
                    message: format!(
                        "Torrents found on RuTracker: {} - Season {}",
                        media_title, season_num
                    ),
                    notification_type: "torrent_available".to_string(),
                    read: false,
                    created_at: String::new(),
                },
            )?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;

    if is_new_discovery {
        let tg_msg = format!(
            "🔍 Torrents found: {} - Season {} ({} results)",
            media.title, season.season_number, total_count
        );
        if let Err(error) =
            tg_notify::send_notification(&state.telegram_bot, state.telegram_chat_id, &tg_msg).await
        {
            warn!(?error, "failed to send telegram notification");
        }

        info!(
            title = media.title,
            season = season.season_number,
            count = total_count,
            "new torrents discovered"
        );
    } else {
        debug!(
            title = media.title,
            season = season.season_number,
            count = total_count,
            "search cache updated"
        );
    }

    Ok(())
}
