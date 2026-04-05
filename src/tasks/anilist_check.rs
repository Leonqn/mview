use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::db::models::{Episode, Notification, Season};
use crate::db::queries;
use crate::telegram::notifications as tg_notify;
use crate::web::AppState;

const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

pub async fn run(state: Arc<AppState>) {
    info!(
        interval_hours = CHECK_INTERVAL.as_secs() / 3600,
        "anilist check started"
    );

    let mut interval = tokio::time::interval(CHECK_INTERVAL);
    interval.tick().await;

    loop {
        interval.tick().await;

        if let Err(error) = check_anime_updates(&state).await {
            error!(?error, "anilist check error");
        }
    }
}

pub async fn check_anime_updates(state: &Arc<AppState>) -> Result<()> {
    let media_list = {
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_all_media(&conn)
        })
        .await??
    };

    let anime: Vec<_> = media_list
        .into_iter()
        .filter(|m| m.media_type == "anime" && m.status == "tracking" && m.anilist_id.is_some())
        .collect();

    if anime.is_empty() {
        debug!("no tracked anime to check on anilist");
        return Ok(());
    }

    info!(count = anime.len(), "checking tracked anime on anilist");

    for media in &anime {
        if let Err(error) = check_single_anime(state, media).await {
            error!(
                title = media.title,
                anilist_id = media.anilist_id.unwrap_or(0),
                ?error,
                "failed to check anime"
            );
        }
    }

    Ok(())
}

async fn check_single_anime(state: &Arc<AppState>, media: &crate::db::models::Media) -> Result<()> {
    let anilist_id = media.anilist_id.unwrap();

    let media_id = media.id;
    let pool = state.db.clone();
    let db_seasons = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_seasons_for_media(&conn, media_id)
    })
    .await??;

    let db_anilist_ids: HashSet<i64> = db_seasons.iter().filter_map(|s| s.anilist_id).collect();

    let chain = state.anilist.get_sequel_chain(anilist_id).await?;

    let next_season_number = db_seasons
        .iter()
        .map(|s| s.season_number)
        .max()
        .unwrap_or(0)
        + 1;

    let mut season_num = next_season_number;

    for (idx, entry) in chain.iter().enumerate() {
        if db_anilist_ids.contains(&entry.id) {
            // Season already in DB — check if episode_count / episodes need updating
            // (e.g. the season was added while NOT_YET_RELEASED and now has data).
            if let Err(error) = update_existing_season(state, &db_seasons, &chain, idx).await {
                warn!(
                    title = media.title,
                    anilist_id = entry.id,
                    ?error,
                    "failed to update existing season"
                );
            }
            continue;
        }

        let season_title = entry
            .title
            .english
            .clone()
            .or_else(|| entry.title.romaji.clone());

        let season = Season {
            id: 0,
            media_id: media.id,
            season_number: season_num,
            title: season_title,
            episode_count: entry.episodes,
            anilist_id: Some(entry.id),
            format: None,
            status: "tracking".to_string(),
            created_at: String::new(),
        };

        let streaming = crate::web::routes::api::streaming_episodes_for_season_public(&chain, idx);
        let episodes: Vec<Episode> = if !streaming.is_empty() {
            streaming
                .iter()
                .map(|(ep_num, ep_title)| Episode {
                    id: 0,
                    season_id: 0,
                    episode_number: *ep_num,
                    title: if ep_title.is_empty() {
                        None
                    } else {
                        Some(ep_title.clone())
                    },
                    air_date: entry.episode_air_date(*ep_num),
                    downloaded: false,
                    file_path: None,
                })
                .collect()
        } else {
            (1..=entry.episodes.unwrap_or(0))
                .map(|ep_num| Episode {
                    id: 0,
                    season_id: 0,
                    episode_number: ep_num,
                    title: None,
                    air_date: entry.episode_air_date(ep_num),
                    downloaded: false,
                    file_path: None,
                })
                .collect()
        };

        let pool = state.db.clone();
        let mid = media.id;
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let season_id = queries::insert_season(&conn, &season)?;
            for mut ep in episodes {
                ep.season_id = season_id;
                queries::insert_episode(&conn, &ep)?;
            }
            queries::insert_notification(
                &conn,
                &Notification {
                    id: 0,
                    media_id: Some(mid),
                    message: format!(
                        "New anime season detected: {} - Season {}",
                        season.title.as_deref().unwrap_or("Unknown"),
                        season.season_number
                    ),
                    notification_type: "new_season".to_string(),
                    read: false,
                    created_at: String::new(),
                },
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;

        let tg_msg = tg_notify::format_new_season(&media.title, season_num);
        if let Err(error) =
            tg_notify::send_notification(&state.telegram_bot, state.telegram_chat_id, &tg_msg).await
        {
            warn!(?error, "failed to send telegram notification");
        }

        info!(
            title = media.title,
            season = season_num,
            "new anime season added from anilist"
        );

        season_num += 1;
    }

    Ok(())
}

/// Update an existing season with latest data from AniList.
/// Handles the case where a season was added while NOT_YET_RELEASED (no episode_count,
/// no episodes) and has now been populated with data.
async fn update_existing_season(
    state: &Arc<AppState>,
    db_seasons: &[Season],
    chain: &[crate::anilist::models::AniListMedia],
    idx: usize,
) -> Result<()> {
    let entry = &chain[idx];
    let Some(db_season) = db_seasons.iter().find(|s| s.anilist_id == Some(entry.id)) else {
        return Ok(());
    };

    // Nothing to update if AniList still has no episode count
    let Some(new_ep_count) = entry.episodes else {
        return Ok(());
    };

    // Count existing episodes in DB
    let season_id = db_season.id;
    let pool = state.db.clone();
    let existing_eps = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_episodes_for_season(&conn, season_id)
    })
    .await??;

    let existing_count = existing_eps.len() as i64;
    let old_ep_count = db_season.episode_count;

    // Skip if nothing changed: same count, same episodes already present
    if old_ep_count == Some(new_ep_count) && existing_count >= new_ep_count {
        return Ok(());
    }

    info!(
        title = db_season.title,
        anilist_id = entry.id,
        old_count = ?old_ep_count,
        new_count = new_ep_count,
        existing = existing_count,
        "updating existing season from anilist"
    );

    // Build new episodes from AniList data
    let streaming = crate::web::routes::api::streaming_episodes_for_season_public(chain, idx);
    let new_episodes: Vec<Episode> = if !streaming.is_empty() {
        streaming
            .iter()
            .map(|(ep_num, ep_title)| Episode {
                id: 0,
                season_id,
                episode_number: *ep_num,
                title: if ep_title.is_empty() {
                    None
                } else {
                    Some(ep_title.clone())
                },
                air_date: entry.episode_air_date(*ep_num),
                downloaded: false,
                file_path: None,
            })
            .collect()
    } else {
        (1..=new_ep_count)
            .map(|ep_num| Episode {
                id: 0,
                season_id,
                episode_number: ep_num,
                title: None,
                air_date: entry.episode_air_date(ep_num),
                downloaded: false,
                file_path: None,
            })
            .collect()
    };

    // Insert only episodes that don't already exist (by episode_number)
    let existing_nums: HashSet<i64> = existing_eps.iter().map(|e| e.episode_number).collect();
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::update_season_episode_count(&conn, season_id, Some(new_ep_count))?;
        for ep in new_episodes {
            if !existing_nums.contains(&ep.episode_number) {
                queries::insert_episode(&conn, &ep)?;
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;

    Ok(())
}
