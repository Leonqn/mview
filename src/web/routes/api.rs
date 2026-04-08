use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Form, Router};
use chrono::Local;
use serde::Deserialize;
use tracing::{error, info};

use crate::db::models::{Episode, Media, Season};
use crate::db::queries;
use crate::error::AppError;
use crate::web::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/media/track", post(track_media))
        .route("/api/media/{id}/delete", post(delete_media))
        .route("/api/torrents/{id}/delete", post(delete_torrent_endpoint))
        .route("/api/torrents/{id}/progress", get(torrent_progress))
        .route(
            "/api/seasons/{id}/progress-badge",
            get(season_progress_badge),
        )
        .route("/api/notifications", get(get_notifications))
        .route("/api/notifications/{id}/read", post(mark_notification_read))
        .route("/api/media/{id}/plex-scan", post(plex_scan_media))
}

#[derive(Debug, Deserialize)]
pub struct TrackMediaForm {
    pub tmdb_id: Option<i64>,
    pub anilist_id: Option<i64>,
    pub media_type: String,
}

async fn track_media(
    State(state): State<Arc<AppState>>,
    Form(form): Form<TrackMediaForm>,
) -> axum::response::Response {
    let media_type = form.media_type.clone();
    info!(
        media_type = %media_type,
        tmdb_id = ?form.tmdb_id,
        anilist_id = ?form.anilist_id,
        "tracking media"
    );

    let result: anyhow::Result<(i64, String)> = async {
        match media_type.as_str() {
            "movie" | "series" => {
                let tmdb_id = form
                    .tmdb_id
                    .ok_or_else(|| anyhow::anyhow!("tmdb_id is required for {}", media_type))?;

                let pool = state.db.clone();
                let mt = media_type.clone();
                let existing = tokio::task::spawn_blocking(move || {
                    let conn = pool.get()?;
                    queries::get_media_by_tmdb_id(&conn, tmdb_id, &mt)
                })
                .await??;

                if let Some(m) = existing {
                    return Ok((m.id, media_type.clone()));
                }

                let id = if media_type == "movie" {
                    track_movie(&state, tmdb_id).await?
                } else {
                    track_series(&state, tmdb_id).await?
                };
                Ok((id, media_type.clone()))
            }
            "anime" => {
                if let Some(anilist_id) = form.anilist_id {
                    // Track via AniList
                    let pool = state.db.clone();
                    let existing = tokio::task::spawn_blocking(move || {
                        let conn = pool.get()?;
                        queries::get_media_by_anilist_id(&conn, anilist_id)
                    })
                    .await??;

                    if let Some(m) = existing {
                        return Ok((m.id, "anime".to_string()));
                    }

                    let id = track_anime(&state, anilist_id).await?;
                    Ok((id, "anime".to_string()))
                } else if let Some(tmdb_id) = form.tmdb_id {
                    // Track via TMDB but as anime type
                    let pool = state.db.clone();
                    let existing = tokio::task::spawn_blocking(move || {
                        let conn = pool.get()?;
                        queries::get_media_by_tmdb_id(&conn, tmdb_id, "anime")
                    })
                    .await??;

                    if let Some(m) = existing {
                        return Ok((m.id, "anime".to_string()));
                    }

                    let id = track_series_as_anime(&state, tmdb_id).await?;
                    Ok((id, "anime".to_string()))
                } else {
                    Err(anyhow::anyhow!("anilist_id or tmdb_id required for anime"))
                }
            }
            _ => Err(anyhow::anyhow!("invalid media_type: {}", media_type)),
        }
    }
    .await;

    match result {
        Ok((id, _)) => {
            let path = format!("/media/{id}");
            // HX-Redirect tells htmx to do a client-side redirect
            axum::response::Response::builder()
                .header("HX-Redirect", &path)
                .body(axum::body::Body::empty())
                .unwrap()
        }
        Err(e) => Html(format!(
            "<span style=\"color:var(--pico-del-color);\">{e}</span>"
        ))
        .into_response(),
    }
}

async fn track_movie(state: &AppState, tmdb_id: i64) -> anyhow::Result<i64> {
    let details = state.tmdb.get_movie(tmdb_id).await?;
    info!(title = details.title, tmdb_id, "tracking movie");

    // Check if movie belongs to a collection (franchise)
    if let Some(ref collection_ref) = details.belongs_to_collection {
        // Collection stores tmdb_id = collection.id, check for existing
        let pool = state.db.clone();
        let collection_id = collection_ref.id;
        let existing = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_media_by_tmdb_id(&conn, collection_id, "movie")
        })
        .await??;

        if let Some(m) = existing {
            return Ok(m.id);
        }

        let collection = state.tmdb.get_collection(collection_ref.id).await?;
        info!(
            collection = collection.name,
            parts = collection.parts.len(),
            "movie belongs to collection"
        );
        return track_movie_collection(state, &details, &collection).await;
    }

    // Standalone movie — single season stub
    let movie_title = details.title.clone();
    let year = details
        .release_date
        .as_ref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i64>().ok());

    let poster_url = details
        .poster_path
        .as_ref()
        .map(|p| format!("https://image.tmdb.org/t/p/w780{}", p));

    let media = Media {
        id: 0,
        media_type: "movie".to_string(),
        title: details.title,
        title_original: details.original_title,
        year,
        tmdb_id: Some(tmdb_id),
        imdb_id: details.imdb_id,
        kinopoisk_url: None,
        world_art_url: None,
        poster_url,
        overview: details.overview,
        anilist_id: None,
        status: "tracking".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };

    let pool = state.db.clone();
    let media_id = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let tx = conn.unchecked_transaction()?;
        let media_id = queries::insert_media(&tx, &media)?;
        let season_id = queries::insert_season(
            &tx,
            &Season {
                id: 0,
                media_id,
                season_number: 1,
                title: Some(movie_title),
                episode_count: Some(1),
                anilist_id: None,
                format: Some("MOVIE".to_string()),
                status: "tracking".to_string(),
                created_at: String::new(),
            },
        )?;
        queries::insert_episode(
            &tx,
            &Episode {
                id: 0,
                season_id,
                episode_number: 1,
                title: None,
                air_date: None,
                downloaded: false,
                file_path: None,
            },
        )?;
        tx.commit()?;
        Ok::<i64, anyhow::Error>(media_id)
    })
    .await??;

    Ok(media_id)
}

async fn track_movie_collection(
    state: &AppState,
    tracked_movie: &crate::tmdb::models::TmdbMovieDetails,
    collection: &crate::tmdb::models::TmdbCollectionDetails,
) -> anyhow::Result<i64> {
    let poster_url = collection
        .poster_path
        .as_ref()
        .map(|p| format!("https://image.tmdb.org/t/p/w780{}", p));

    let media = Media {
        id: 0,
        media_type: "movie".to_string(),
        title: tracked_movie.title.clone(),
        title_original: tracked_movie.original_title.clone(),
        year: tracked_movie
            .release_date
            .as_deref()
            .and_then(|d| d.split('-').next())
            .and_then(|y| y.parse::<i64>().ok()),
        tmdb_id: Some(collection.id),
        imdb_id: None,
        kinopoisk_url: None,
        world_art_url: None,
        poster_url,
        overview: collection.overview.clone(),
        anilist_id: None,
        status: "tracking".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };

    // Sort parts by release date
    let mut parts = collection.parts.clone();
    parts.sort_by(|a, b| a.release_date.cmp(&b.release_date));

    let tracked_tmdb_id = tracked_movie.id;

    let season_data: Vec<(Season, Episode)> = parts
        .iter()
        .enumerate()
        .map(|(idx, part)| {
            let status = if part.id == tracked_tmdb_id {
                "tracking"
            } else {
                "ignored"
            };

            let season = Season {
                id: 0,
                media_id: 0,
                season_number: (idx as i64) + 1,
                title: Some(part.title.clone()),
                episode_count: Some(1),
                anilist_id: None,
                format: Some("MOVIE".to_string()),
                status: status.to_string(),
                created_at: String::new(),
            };

            let air_date = part
                .release_date
                .as_ref()
                .and_then(|d| d.get(..10).map(|s| s.to_string()));

            let episode = Episode {
                id: 0,
                season_id: 0,
                episode_number: 1,
                title: Some(part.title.clone()),
                air_date,
                downloaded: false,
                file_path: None,
            };

            (season, episode)
        })
        .collect();

    let pool = state.db.clone();
    let media_id = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let tx = conn.unchecked_transaction()?;

        let media_id = queries::insert_media(&tx, &media)?;

        for (mut season, mut episode) in season_data {
            season.media_id = media_id;
            let season_id = queries::insert_season(&tx, &season)?;
            episode.season_id = season_id;
            queries::insert_episode(&tx, &episode)?;
        }

        tx.commit()?;
        Ok::<i64, anyhow::Error>(media_id)
    })
    .await??;

    info!(
        media_id,
        collection = collection.name,
        parts = collection.parts.len(),
        "collection tracked"
    );
    Ok(media_id)
}

async fn track_series_as_anime(state: &AppState, tmdb_id: i64) -> anyhow::Result<i64> {
    track_series_with_type(state, tmdb_id, "anime").await
}

async fn track_series(state: &AppState, tmdb_id: i64) -> anyhow::Result<i64> {
    track_series_with_type(state, tmdb_id, "series").await
}

async fn track_series_with_type(
    state: &AppState,
    tmdb_id: i64,
    media_type: &str,
) -> anyhow::Result<i64> {
    let details = state.tmdb.get_tv(tmdb_id).await?;
    info!(title = details.name, tmdb_id, media_type, "tracking series");

    let year = details
        .first_air_date
        .as_ref()
        .and_then(|d| d.get(..4))
        .and_then(|y| y.parse::<i64>().ok());

    let poster_url = details
        .poster_path
        .as_ref()
        .map(|p| format!("https://image.tmdb.org/t/p/w780{}", p));

    let imdb_id = details
        .external_ids
        .as_ref()
        .and_then(|ext| ext.imdb_id.clone());

    let media = Media {
        id: 0,
        media_type: media_type.to_string(),
        title: details.name,
        title_original: details.original_name,
        year,
        tmdb_id: Some(tmdb_id),
        imdb_id,
        kinopoisk_url: None,
        world_art_url: None,
        poster_url,
        overview: details.overview,
        anilist_id: None,
        status: "tracking".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };

    // Fetch all season/episode data from TMDB before touching the DB,
    // so we can insert everything in a single transaction.
    let mut season_data: Vec<(Season, Vec<Episode>)> = Vec::new();
    if let Some(ref seasons) = details.seasons {
        let real_seasons: Vec<_> = seasons.iter().filter(|s| s.season_number > 0).collect();
        let last_season_num = real_seasons
            .iter()
            .map(|s| s.season_number)
            .max()
            .unwrap_or(0);

        let today = Local::now().format("%Y-%m-%d").to_string();
        for s in &real_seasons {
            let episodes: Vec<Episode> = match state.tmdb.get_season(tmdb_id, s.season_number).await
            {
                Ok(season_details) => season_details
                    .episodes
                    .unwrap_or_default()
                    .into_iter()
                    .map(|ep| Episode {
                        id: 0,
                        season_id: 0,
                        episode_number: ep.episode_number,
                        title: ep.name,
                        air_date: ep.air_date,
                        downloaded: false,
                        file_path: None,
                    })
                    .collect(),
                Err(error) => {
                    error!(
                        season_number = s.season_number,
                        ?error,
                        "failed to fetch season details from tmdb"
                    );
                    Vec::new()
                }
            };

            let all_aired = !episodes.is_empty()
                && episodes.iter().all(|e| {
                    e.air_date
                        .as_deref()
                        .map(|d| d <= today.as_str())
                        .unwrap_or(false)
                });
            let status = if s.season_number == last_season_num && !all_aired {
                "tracking"
            } else {
                "ignored"
            };

            let season = Season {
                id: 0,
                media_id: 0,
                season_number: s.season_number,
                title: s.name.clone(),
                episode_count: s.episode_count,
                anilist_id: None,
                format: Some("TV".to_string()),
                status: status.to_string(),
                created_at: String::new(),
            };

            season_data.push((season, episodes));
        }
    }

    // Insert everything in a single transaction
    let pool = state.db.clone();
    let media_id = tokio::task::spawn_blocking(move || {
        let mut conn = pool.get()?;
        let tx = conn.transaction()?;

        let media_id = queries::insert_media(&tx, &media)?;

        for (mut season, episodes) in season_data {
            season.media_id = media_id;
            let season_id = queries::insert_season(&tx, &season)?;

            for mut episode in episodes {
                episode.season_id = season_id;
                queries::insert_episode(&tx, &episode)?;
            }
        }

        tx.commit()?;
        Ok::<i64, anyhow::Error>(media_id)
    })
    .await??;

    Ok(media_id)
}

/// Slice cumulative streamingEpisodes for a specific chain entry.
///
/// AniList often returns cumulative numbering in streamingEpisodes (e.g. UBW TV-2 entry
/// returns episodes 1..26 or 14..26 — both represent the whole franchise). We compute the
/// expected range `[offset, offset + episode_count]` and take only those, then renormalize
/// them back to start from 1 (keeping episode 0 if it falls in range).
pub fn streaming_episodes_for_season_public(
    chain: &[crate::anilist::models::AniListMedia],
    idx: usize,
) -> Vec<(i64, String)> {
    streaming_episodes_for_season(chain, idx)
}

fn streaming_episodes_for_season(
    chain: &[crate::anilist::models::AniListMedia],
    idx: usize,
) -> Vec<(i64, String)> {
    let entry = &chain[idx];
    let raw = entry.parsed_streaming_episodes_raw();
    if raw.is_empty() {
        return Vec::new();
    }

    let ep_count = match entry.episodes {
        Some(n) => n,
        None => return Vec::new(),
    };

    // Detect "combined" list: AniList bundles multiple seasons' episodes into one
    // streamingEpisodes list shared between sibling entries (e.g. UBW TV-1 and TV-2
    // both get 26 items for 2×13 eps). Heuristic: raw_count >= 2 * ep_count.
    let raw_count = raw.len() as i64;
    let is_combined = raw_count >= 2 * ep_count;

    if is_combined {
        // Find this entry's position among chain siblings sharing the same raw list length.
        let my_raw_count = raw.len();
        let position: i64 = chain[..idx]
            .iter()
            .filter(|m| m.parsed_streaming_episodes_raw().len() == my_raw_count)
            .count() as i64;

        // Take the ep_count-sized slice for this position.
        let start = position * ep_count;
        let end = start + ep_count;

        raw.into_iter()
            .filter(|(n, _)| *n >= start && *n < end)
            .map(|(n, title)| {
                // First segment: keep 0-indexed (preserves prologue as ep 0).
                // Later segments: shift to 1-indexed.
                let new_num = if position == 0 {
                    n - start
                } else {
                    n - start + 1
                };
                (new_num, title)
            })
            .collect()
    } else {
        // Per-season list: only trust titles if count matches this season's ep_count.
        // Otherwise AniList is reusing a sibling's titles (e.g. JJK S2 returns S1's
        // 24 titles for its own 23 episodes).
        if raw_count != ep_count {
            return Vec::new();
        }
        raw.into_iter()
            .filter(|(n, _)| *n >= 0 && *n <= ep_count)
            .collect()
    }
}

fn build_anime_season_data(
    chain: &[crate::anilist::models::AniListMedia],
) -> Vec<(Season, Vec<Episode>)> {
    let last_idx = chain.len().saturating_sub(1);
    chain
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            let season_number = (idx as i64) + 1;
            let season_title = entry
                .title
                .english
                .clone()
                .or_else(|| entry.title.romaji.clone());
            let episode_count = entry.episodes;
            let finished = entry.status.as_deref() == Some("FINISHED")
                || entry.status.as_deref() == Some("CANCELLED");
            let status = if idx == last_idx && !finished {
                "tracking"
            } else {
                "ignored"
            };

            let season = Season {
                id: 0,
                media_id: 0,
                season_number,
                title: season_title,
                episode_count,
                anilist_id: Some(entry.id),
                format: entry.format.clone(),
                status: status.to_string(),
                created_at: String::new(),
            };

            // Prefer AniList's streamingEpisodes (gives real titles + episode 0 when present).
            // Cumulative numbering across sequel seasons is handled by subtracting
            // the chain offset and keeping only this entry's episodes.
            let streaming = streaming_episodes_for_season(chain, idx);
            let raw = entry.parsed_streaming_episodes_raw();
            let min_n = raw.iter().map(|(n, _)| *n).min().unwrap_or(-1);
            let max_n = raw.iter().map(|(n, _)| *n).max().unwrap_or(-1);
            let offset: i64 = chain[..idx].iter().filter_map(|m| m.episodes).sum();
            tracing::info!(
                anilist_id = entry.id,
                chain_idx = idx,
                ep_count = entry.episodes,
                offset,
                raw_count = raw.len(),
                raw_min = min_n,
                raw_max = max_n,
                filtered_count = streaming.len(),
                "building episodes for anime season"
            );
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
                (1..=episode_count.unwrap_or(0))
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

            (season, episodes)
        })
        .collect()
}

async fn track_anime(state: &AppState, anilist_id: i64) -> anyhow::Result<i64> {
    let chain = state.anilist.get_sequel_chain(anilist_id).await?;
    if chain.is_empty() {
        return Err(anyhow::anyhow!(
            "no anime found for anilist_id {}",
            anilist_id
        ));
    }

    // Use the entry the user clicked for media info (not necessarily chain root)
    let clicked = chain
        .iter()
        .find(|m| m.id == anilist_id)
        .unwrap_or(&chain[0]);
    let title = clicked
        .title
        .english
        .clone()
        .or_else(|| clicked.title.romaji.clone())
        .unwrap_or_default();
    let title_original = clicked.title.native.clone();

    // Check for existing media with same title (cross-source dedup)
    let pool = state.db.clone();
    let t = title.clone();
    let t_orig = title_original.clone();
    let existing = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        if let Some(m) = queries::find_media_by_title(&conn, &t)? {
            return Ok(Some(m));
        }
        if let Some(ref orig) = t_orig {
            return queries::find_media_by_title(&conn, orig);
        }
        Ok(None)
    })
    .await??;

    if let Some(m) = existing {
        // Already tracked (e.g. via TMDB) — upgrade with AniList info and rebuild seasons
        // Preserve download status from old episodes before replacing
        let pool = state.db.clone();
        let mid = m.id;
        let downloaded_episodes: std::collections::HashMap<(i64, i64), String> = {
            let pool2 = pool.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool2.get()?;
                let seasons = queries::get_seasons_for_media(&conn, mid)?;
                let mut downloaded = std::collections::HashMap::new();
                for s in &seasons {
                    let eps = queries::get_episodes_for_season(&conn, s.id)?;
                    for ep in eps {
                        if ep.downloaded {
                            downloaded.insert(
                                (s.season_number, ep.episode_number),
                                ep.file_path.unwrap_or_default(),
                            );
                        }
                    }
                }
                Ok::<_, anyhow::Error>(downloaded)
            })
            .await??
        };

        let season_data = build_anime_season_data(&chain);
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let tx = conn.unchecked_transaction()?;
            queries::update_media_anilist(&tx, mid, anilist_id, "anime")?;
            queries::delete_seasons_for_media(&tx, mid)?;
            for (mut season, episodes) in season_data {
                season.media_id = mid;
                let sn = season.season_number;
                let season_id = queries::insert_season(&tx, &season)?;
                for mut ep in episodes {
                    ep.season_id = season_id;
                    if let Some(path) = downloaded_episodes.get(&(sn, ep.episode_number)) {
                        ep.downloaded = true;
                        if !path.is_empty() {
                            ep.file_path = Some(path.clone());
                        }
                    }
                    queries::insert_episode(&tx, &ep)?;
                }
            }
            tx.commit()?;
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        info!(
            media_id = m.id,
            title = m.title,
            seasons = chain.len(),
            "upgraded existing media with anilist info"
        );
        return Ok(m.id);
    }

    let year = clicked.season_year;
    let poster_url = clicked.cover_image.as_ref().and_then(|c| c.large.clone());
    let overview = clicked.description.as_ref().map(|d| {
        // strip HTML
        let mut result = String::new();
        let mut in_tag = false;
        for c in d.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => result.push(c),
                _ => {}
            }
        }
        result
    });

    info!(
        title = title,
        anilist_id,
        seasons = chain.len(),
        "tracking anime"
    );

    let media = Media {
        id: 0,
        media_type: "anime".to_string(),
        title,
        title_original,
        year,
        tmdb_id: None,
        imdb_id: None,
        kinopoisk_url: None,
        world_art_url: None,
        poster_url,
        overview,
        anilist_id: Some(anilist_id),
        status: "tracking".to_string(),
        created_at: String::new(),
        updated_at: String::new(),
    };

    let season_data = build_anime_season_data(&chain);

    let pool = state.db.clone();
    let media_id = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let tx = conn.unchecked_transaction()?;

        let media_id = queries::insert_media(&tx, &media)?;

        for (mut season, episodes) in season_data {
            season.media_id = media_id;
            let season_id = queries::insert_season(&tx, &season)?;

            for mut episode in episodes {
                episode.season_id = season_id;
                queries::insert_episode(&tx, &episode)?;
            }
        }

        tx.commit()?;
        Ok::<i64, anyhow::Error>(media_id)
    })
    .await??;

    info!(media_id, "anime tracked successfully");
    Ok(media_id)
}

#[derive(Debug, Deserialize, Default)]
pub struct DeleteTorrentForm {
    #[serde(default)]
    pub delete_files: bool,
}

async fn delete_torrent_endpoint(
    State(state): State<Arc<AppState>>,
    Path(torrent_id): Path<i64>,
    Form(form): Form<DeleteTorrentForm>,
) -> Result<Html<String>, AppError> {
    let delete_files = form.delete_files;

    let pool = state.db.clone();
    let torrent = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_torrent(&conn, torrent_id)
    })
    .await??
    .ok_or_else(|| anyhow::anyhow!("torrent not found"))?;

    // Only touch qBittorrent when `delete_files` is set: remove the torrent
    // AND its source files. Without the flag, the torrent keeps seeding.
    if delete_files && let Some(ref hash) = torrent.qbt_hash {
        let mut qbt = state.qbittorrent.lock().await;
        if let Err(e) = qbt.delete_torrent(hash, true).await {
            error!(torrent_id, error = %e, "failed to delete torrent from qbittorrent");
        }
    }

    // Collect Plex file paths for completed episodes in this season, then reset downloaded status.
    let plex_paths: Vec<String> = if let Some(season_number) = torrent.season_number {
        let media_id = torrent.media_id;
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let seasons = queries::get_seasons_for_media(&conn, media_id)?;
            let mut paths = Vec::new();
            if let Some(season) = seasons.iter().find(|s| s.season_number == season_number) {
                let episodes = queries::get_episodes_for_season(&conn, season.id)?;
                for ep in &episodes {
                    if ep.downloaded {
                        if let Some(ref path) = ep.file_path {
                            paths.push(path.clone());
                        }
                        queries::update_episode_downloaded(&conn, ep.id, false, None)?;
                    }
                }
                // Reset season status back to tracking if it was completed
                if season.status == "completed" {
                    queries::update_season_status(&conn, season.id, "tracking")?;
                }
            }
            Ok::<_, anyhow::Error>(paths)
        })
        .await??
    } else {
        Vec::new()
    };

    // Remove hardlinks/files from the Plex library and their companion files.
    let plex_scan_dirs = remove_plex_files(&plex_paths).await;

    // Delete torrent row from DB
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::delete_torrent(&conn, torrent_id)
    })
    .await??;

    // Trigger Plex scan on affected directories so the library updates.
    if !plex_scan_dirs.is_empty() {
        let dirs: Vec<String> = plex_scan_dirs.into_iter().collect();
        crate::plex::client::scan(&state, &dirs).await;
    }

    info!(
        torrent_id,
        title = torrent.title,
        delete_files,
        "torrent deleted"
    );

    // Re-render the season partial so the UI reflects the updated torrent list
    // and "Downloaded" badge state.
    if let Some(season_number) = torrent.season_number {
        let media_id = torrent.media_id;
        let pool = state.db.clone();
        let season_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let seasons = queries::get_seasons_for_media(&conn, media_id)?;
            Ok::<_, anyhow::Error>(
                seasons
                    .iter()
                    .find(|s| s.season_number == season_number)
                    .map(|s| s.id),
            )
        })
        .await??;
        if let Some(sid) = season_id {
            return crate::web::routes::series::render_season_partial(&state, sid).await;
        }
    }

    Ok(Html(String::new()))
}

/// Remove video files and their companion files (same stem, different extension)
/// from the Plex library. Returns the set of parent directories for Plex scan.
async fn remove_plex_files(video_paths: &[String]) -> std::collections::HashSet<String> {
    use std::path::Path;
    let mut scan_dirs = std::collections::HashSet::new();

    for video_path in video_paths {
        let path = Path::new(video_path);
        let parent = match path.parent() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        // Remove the video file itself
        if path.exists()
            && let Err(e) = std::fs::remove_file(path)
        {
            error!(path = %path.display(), error = %e, "failed to remove plex file");
        }

        // Remove companion files with the same stem (e.g. subtitles, audio tracks)
        if let Ok(entries) = std::fs::read_dir(&parent) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = match name.to_str() {
                    Some(n) => n,
                    None => continue,
                };
                if name_str.starts_with(&format!("{stem}."))
                    && entry.path() != path
                    && let Err(e) = std::fs::remove_file(entry.path())
                {
                    error!(path = %entry.path().display(), error = %e, "failed to remove companion file");
                }
            }
        }

        scan_dirs.insert(parent.to_string_lossy().into_owned());
    }

    scan_dirs
}

async fn torrent_progress(
    State(state): State<Arc<AppState>>,
    Path(torrent_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let torrent = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_torrent(&conn, torrent_id)
    })
    .await??
    .ok_or_else(|| anyhow::anyhow!("torrent not found"))?;

    let qbt_hash = match &torrent.qbt_hash {
        Some(h) => h.clone(),
        None => return Ok(Html(String::new())),
    };

    let mut qbt = state.qbittorrent.lock().await;
    let progress = match qbt.get_torrents(Some("mview")).await {
        Ok(torrents) => torrents
            .iter()
            .find(|t| t.hash == qbt_hash)
            .map(|t| (t.progress * 100.0 * 10.0).round() / 10.0),
        Err(_) => None,
    };

    let delete_btn = format!(
        " <button hx-post=\"/api/torrents/{tid}/delete\" hx-target=\"#torrent-{tid}\" hx-swap=\"outerHTML\" hx-confirm=\"Delete torrent and all downloaded files?\" class=\"outline secondary\" style=\"margin:0;padding:1px 8px;font-size:0.8em;\">Delete</button>",
        tid = torrent_id
    );

    let html = match progress {
        Some(p) if p >= 100.0 => {
            format!("<span style=\"color:green;\">Completed</span>{delete_btn}")
        }
        Some(p) => {
            format!("<span style=\"color:orange;\">Downloading {p}%</span>{delete_btn}")
        }
        None => format!("<span style=\"color:orange;\">Downloading</span>{delete_btn}"),
    };

    Ok(Html(html))
}

async fn season_progress_badge(
    State(state): State<Arc<AppState>>,
    Path(season_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let (season_number, media_id) = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let season = queries::get_season(&conn, season_id)?
            .ok_or_else(|| anyhow::anyhow!("season not found"))?;
        Ok::<_, anyhow::Error>((season.season_number, season.media_id))
    })
    .await??;

    let pool = state.db.clone();
    let qbt_hash = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let torrents = queries::get_torrents_for_media(&conn, media_id)?;
        let hash = torrents
            .into_iter()
            .find(|t| t.season_number == Some(season_number) && t.qbt_hash.is_some())
            .and_then(|t| t.qbt_hash);
        Ok::<_, anyhow::Error>(hash)
    })
    .await??;

    let Some(qbt_hash) = qbt_hash else {
        return Ok(Html(String::new()));
    };

    let mut qbt = state.qbittorrent.lock().await;
    let torrent_info = match qbt.get_torrents(Some("mview")).await {
        Ok(torrents) => torrents.into_iter().find(|t| t.hash == qbt_hash),
        Err(_) => None,
    };

    let html = match torrent_info {
        Some(ref t) if t.progress >= 1.0 => {
            // Completed — return static span without polling
            format!(
                "<span id=\"season-progress-{season_id}\" style=\"font-size:0.8em;opacity:0.6;white-space:nowrap;\">Downloaded</span>"
            )
        }
        Some(t) => {
            let pct = (t.progress * 100.0 * 10.0).round() / 10.0;
            let eta_str = if t.eta > 0 && t.eta < 8_640_000 {
                format!(" · {}", format_eta(t.eta))
            } else {
                String::new()
            };
            format!(
                "<span id=\"season-progress-{season_id}\" hx-get=\"/api/seasons/{season_id}/progress-badge\" hx-trigger=\"every 10s\" hx-swap=\"outerHTML\" style=\"font-size:0.8em;color:orange;white-space:nowrap;\">Downloading {pct}%{eta_str}</span>"
            )
        }
        None => format!(
            "<span id=\"season-progress-{season_id}\" hx-get=\"/api/seasons/{season_id}/progress-badge\" hx-trigger=\"every 10s\" hx-swap=\"outerHTML\" style=\"font-size:0.8em;color:orange;white-space:nowrap;\">Downloading</span>"
        ),
    };

    Ok(Html(html))
}

async fn delete_media(
    State(state): State<Arc<AppState>>,
    Path(media_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::delete_media(&conn, media_id)
    })
    .await??;

    info!(media_id, "media deleted");
    Ok(Html(String::new()))
}

async fn get_notifications(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let notifications = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_unread_notifications(&conn)
    })
    .await??;

    let tmpl = state.templates.get_template("partials/notification.html")?;
    let html = tmpl.render(minijinja::context! {
        notifications => notifications,
    })?;
    Ok(Html(html))
}

async fn mark_notification_read(
    State(state): State<Arc<AppState>>,
    Path(notification_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::mark_notification_read(&conn, notification_id)
    })
    .await??;

    // Return empty string to remove the notification from the DOM
    Ok(Html(String::new()))
}

async fn plex_scan_media(
    State(state): State<Arc<AppState>>,
    Path(media_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let media = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_media(&conn, media_id)
    })
    .await??
    .ok_or_else(|| anyhow::anyhow!("media not found"))?;

    let dir = match media.media_type.as_str() {
        "anime" if !state.config.paths.anime_dir.is_empty() => state.config.paths.anime_dir.clone(),
        "movie" => state.config.paths.movies_dir.clone(),
        _ => state.config.paths.tv_dir.clone(),
    };

    let path = crate::plex::organizer::series_dir_path(&dir, &media.title)
        .to_string_lossy()
        .into_owned();

    info!(media_id, title = media.title, path, "triggering plex scan");
    crate::plex::client::scan(&state, &[path]).await;

    Ok(Html(String::new()))
}

fn format_eta(secs: i64) -> String {
    if secs >= 3600 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h {m}m")
    } else if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m {s}s")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db;
    use crate::db::models::Media;
    use crate::web;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn test_streaming_episodes_for_season_combined() {
        use crate::anilist::models::{AniListMedia, AniListStreamingEpisode, AniListTitle};

        // UBW case: AniList returns 26 items (0..=25) for 2 seasons × 13 eps,
        // shared between both sibling entries.
        let make_entry = |id: i64| AniListMedia {
            id,
            title: AniListTitle {
                romaji: None,
                english: None,
                native: None,
            },
            episodes: Some(13),
            season_year: None,
            format: None,
            status: None,
            description: None,
            cover_image: None,
            airing_schedule: None,
            streaming_episodes: (0..=25)
                .map(|n| AniListStreamingEpisode {
                    title: Some(format!("Episode {n} - Title {n}")),
                })
                .collect(),
            relations: None,
        };

        let chain = vec![make_entry(1), make_entry(2)];

        // S1 (position=0): items 0..=12 → 13 items, 0-indexed (preserves prologue as ep 0)
        let s1 = streaming_episodes_for_season(&chain, 0);
        assert_eq!(s1.len(), 13);
        assert_eq!(s1[0], (0, "Title 0".to_string()));
        assert_eq!(s1[12], (12, "Title 12".to_string()));

        // S2 (position=1): items 13..=25 → 13 items, renormalized to 1-indexed
        let s2 = streaming_episodes_for_season(&chain, 1);
        assert_eq!(s2.len(), 13);
        assert_eq!(s2[0], (1, "Title 13".to_string()));
        assert_eq!(s2[12], (13, "Title 25".to_string()));
    }

    #[test]
    fn test_streaming_episodes_jjk_scenario() {
        use crate::anilist::models::{AniListMedia, AniListStreamingEpisode, AniListTitle};

        // JJK reality: ALL entries return same 24 streaming episodes (S1's).
        // Movie has 1 ep, S1 has 24, S2 has 23.
        let make_entry = |id: i64, ep_count: i64, raw_count: i64| AniListMedia {
            id,
            title: AniListTitle {
                romaji: None,
                english: None,
                native: None,
            },
            episodes: Some(ep_count),
            season_year: None,
            format: None,
            status: None,
            description: None,
            cover_image: None,
            airing_schedule: None,
            streaming_episodes: (1..=raw_count)
                .map(|n| AniListStreamingEpisode {
                    title: Some(format!("Episode {n} - Title {n}")),
                })
                .collect(),
            relations: None,
        };

        let chain = vec![
            make_entry(131573, 1, 24),  // JJK 0 movie
            make_entry(113415, 24, 24), // JJK S1
            make_entry(145064, 23, 24), // JJK S2
        ];

        let s0 = streaming_episodes_for_season(&chain, 0);
        let s1 = streaming_episodes_for_season(&chain, 1);
        let s2 = streaming_episodes_for_season(&chain, 2);

        eprintln!("JJK 0 movie: {} items", s0.len());
        eprintln!("JJK S1: {} items, first: {:?}", s1.len(), s1.first());
        eprintln!("JJK S2: {} items, first: {:?}", s2.len(), s2.first());

        assert_eq!(s1.len(), 24, "S1 should have 24 episodes");
        assert!(s1[0].1.starts_with("Title"), "S1 should have titles");
    }

    #[test]
    fn test_streaming_episodes_for_season_per_season() {
        use crate::anilist::models::{AniListMedia, AniListStreamingEpisode, AniListTitle};

        // Fate/Zero case: each entry has its own per-season list 1..=N.
        let make_entry = |id: i64, max: i64| AniListMedia {
            id,
            title: AniListTitle {
                romaji: None,
                english: None,
                native: None,
            },
            episodes: Some(max),
            season_year: None,
            format: None,
            status: None,
            description: None,
            cover_image: None,
            airing_schedule: None,
            streaming_episodes: (1..=max)
                .map(|n| AniListStreamingEpisode {
                    title: Some(format!("Episode {n} - S{id}E{n}")),
                })
                .collect(),
            relations: None,
        };

        let chain = vec![make_entry(1, 13), make_entry(2, 12)];

        // S1: 13 items for 13 eps, not combined (13 < 2*13), use as-is
        let s1 = streaming_episodes_for_season(&chain, 0);
        assert_eq!(s1.len(), 13);
        assert_eq!(s1[0].1, "S1E1");
        assert_eq!(s1[12].1, "S1E13");

        // S2: 12 items for 12 eps, not combined, use as-is (titles from this entry)
        let s2 = streaming_episodes_for_season(&chain, 1);
        assert_eq!(s2.len(), 12);
        assert_eq!(s2[0].1, "S2E1");
        assert_eq!(s2[11].1, "S2E12");
    }

    fn test_config() -> Config {
        let toml_str = r#"
[rutracker]
url = "http://127.0.0.1:19999"
username = "user"
password = "pass"

[qbittorrent]
url = "http://localhost:8080"
username = "admin"
password = "adminpass"

[tmdb]
api_key = "abc123"

[paths]
download_dir = "/tmp"
movies_dir = "/tmp/movies"
tv_dir = "/tmp/tv"
anime_dir = "/tmp/anime"
"#;
        toml::from_str(toml_str).unwrap()
    }

    fn build_test_state() -> Arc<AppState> {
        let config = test_config();
        let pool = db::init_pool(":memory:").unwrap();
        let rt_config = Arc::new(config.rutracker.clone());
        let auth_handle = crate::rutracker::auth::spawn_auth_task(rt_config);
        let rt_client = crate::rutracker::client::RutrackerClient::new(
            &config.rutracker.url,
            auth_handle.clone(),
        );
        let tmdb_client = crate::tmdb::client::TmdbClient::new(&config.tmdb.api_key).unwrap();
        let qbt_config = Arc::new(config.qbittorrent.clone());
        let qbt_client = crate::qbittorrent::client::QbtClient::new(qbt_config).unwrap();
        let templates = web::init_templates();
        Arc::new(AppState {
            db: pool,
            rutracker: rt_client,
            tmdb: tmdb_client,
            anilist: crate::anilist::client::AniListClient::new().unwrap(),
            qbittorrent: tokio::sync::Mutex::new(qbt_client),
            auth_handle,
            telegram_bot: teloxide::Bot::new("fake:token"),
            telegram_chat_id: 0,
            config,
            templates,
        })
    }

    #[tokio::test]
    async fn test_track_endpoint_exists() {
        let state = build_test_state();
        let app = web::build_router(state);

        // POST without valid form data should return an error, not 404
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/media/track")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tmdb_id=999&media_type=movie"))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Will fail with 500 since TMDB API key is fake, but route exists (not 404)
        assert_ne!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_track_invalid_media_type() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/media/track")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("tmdb_id=999&media_type=invalid"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("invalid media_type"));
    }

    #[tokio::test]
    async fn test_track_movie_db_integration() {
        // Test that track_movie correctly saves to DB when given valid data
        let state = build_test_state();

        // Directly test DB insertion logic (bypassing TMDB API)
        let media = Media {
            id: 0,
            media_type: "movie".to_string(),
            title: "Test Movie".to_string(),
            title_original: Some("Test Movie Original".to_string()),
            year: Some(2024),
            tmdb_id: Some(12345),
            imdb_id: Some("tt1234567".to_string()),
            kinopoisk_url: None,
            world_art_url: None,
            poster_url: Some("https://image.tmdb.org/t/p/w300/poster.jpg".to_string()),
            overview: Some("A test movie".to_string()),
            anilist_id: None,
            status: "tracking".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        };

        let pool = state.db.clone();
        let media_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_media(&conn, &media)
        })
        .await
        .unwrap()
        .unwrap();

        assert!(media_id > 0);

        let pool = state.db.clone();
        let fetched = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_media(&conn, media_id)
        })
        .await
        .unwrap()
        .unwrap()
        .unwrap();

        assert_eq!(fetched.title, "Test Movie");
        assert_eq!(fetched.tmdb_id, Some(12345));
        assert_eq!(fetched.status, "tracking");
    }

    #[tokio::test]
    async fn test_track_movie_collection_creates_multiple_seasons() {
        use crate::tmdb::models::{TmdbCollectionDetails, TmdbCollectionPart, TmdbMovieDetails};

        let state = build_test_state();

        let movie_details = TmdbMovieDetails {
            id: 671,
            title: "Harry Potter 1".to_string(),
            original_title: None,
            overview: None,
            poster_path: None,
            release_date: Some("2001-11-16".to_string()),
            imdb_id: None,
            vote_average: None,
            runtime: None,
            belongs_to_collection: None,
        };

        let collection = TmdbCollectionDetails {
            id: 1241,
            name: "Harry Potter Collection".to_string(),
            overview: Some("All movies".to_string()),
            poster_path: Some("/collection.jpg".to_string()),
            parts: vec![
                TmdbCollectionPart {
                    id: 671,
                    title: "Harry Potter 1".to_string(),
                    original_title: None,
                    release_date: Some("2001-11-16".to_string()),
                    poster_path: None,
                    overview: None,
                },
                TmdbCollectionPart {
                    id: 672,
                    title: "Harry Potter 2".to_string(),
                    original_title: None,
                    release_date: Some("2002-11-15".to_string()),
                    poster_path: None,
                    overview: None,
                },
                TmdbCollectionPart {
                    id: 673,
                    title: "Harry Potter 3".to_string(),
                    original_title: None,
                    release_date: Some("2004-06-04".to_string()),
                    poster_path: None,
                    overview: None,
                },
            ],
        };

        let media_id = track_movie_collection(&state, &movie_details, &collection)
            .await
            .unwrap();

        // Verify media was created
        let pool = state.db.clone();
        let mid = media_id;
        let media = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_media(&conn, mid)
        })
        .await
        .unwrap()
        .unwrap()
        .unwrap();

        assert_eq!(media.title, "Harry Potter 1");
        assert_eq!(media.media_type, "movie");

        // Verify 3 seasons were created (one per collection part)
        let pool = state.db.clone();
        let seasons = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_seasons_for_media(&conn, mid)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(seasons.len(), 3);

        // The tracked movie (id=671) should be "tracking", others "ignored"
        let tracking: Vec<_> = seasons.iter().filter(|s| s.status == "tracking").collect();
        let ignored: Vec<_> = seasons.iter().filter(|s| s.status == "ignored").collect();
        assert_eq!(tracking.len(), 1);
        assert_eq!(ignored.len(), 2);
        assert_eq!(tracking[0].title, Some("Harry Potter 1".to_string()));
    }

    #[tokio::test]
    async fn test_track_series_db_integration() {
        let state = build_test_state();

        // Test series + season + episode insertion
        let media = Media {
            id: 0,
            media_type: "series".to_string(),
            title: "Test Series".to_string(),
            title_original: None,
            year: Some(2020),
            tmdb_id: Some(67890),
            imdb_id: None,
            kinopoisk_url: None,
            world_art_url: None,
            poster_url: None,
            overview: None,
            anilist_id: None,
            status: "tracking".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        };

        let pool = state.db.clone();
        let media_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_media(&conn, &media)
        })
        .await
        .unwrap()
        .unwrap();

        let season = Season {
            id: 0,
            media_id,
            season_number: 1,
            title: Some("Season 1".to_string()),
            episode_count: Some(10),
            anilist_id: None,
            format: None,
            status: "tracking".to_string(),
            created_at: String::new(),
        };

        let pool = state.db.clone();
        let season_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_season(&conn, &season)
        })
        .await
        .unwrap()
        .unwrap();

        let episode = Episode {
            id: 0,
            season_id,
            episode_number: 1,
            title: Some("Pilot".to_string()),
            air_date: Some("2020-01-15".to_string()),
            downloaded: false,
            file_path: None,
        };

        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_episode(&conn, &episode)
        })
        .await
        .unwrap()
        .unwrap();

        // Verify everything is in DB
        let pool = state.db.clone();
        let seasons = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_seasons_for_media(&conn, media_id)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(seasons.len(), 1);
        assert_eq!(seasons[0].season_number, 1);

        let sid = seasons[0].id;
        let pool = state.db.clone();
        let episodes = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_episodes_for_season(&conn, sid)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].title, Some("Pilot".to_string()));
    }

    #[tokio::test]
    async fn test_delete_media_endpoint() {
        let state = build_test_state();

        // Insert a media to delete
        let pool = state.db.clone();
        let media_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_media(
                &conn,
                &Media {
                    id: 0,
                    media_type: "movie".to_string(),
                    title: "To Delete".to_string(),
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
        })
        .await
        .unwrap()
        .unwrap();

        let app = web::build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/media/{}/delete", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Verify it's deleted from DB
        let pool = state.db.clone();
        let media = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_media(&conn, media_id)
        })
        .await
        .unwrap()
        .unwrap();
        assert!(media.is_none());
    }

    #[tokio::test]
    async fn test_get_notifications_empty() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/notifications")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_get_notifications_with_data() {
        let state = build_test_state();

        // Insert a media first (for foreign key)
        let pool = state.db.clone();
        let media_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_media(
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
        })
        .await
        .unwrap()
        .unwrap();

        // Insert a notification
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_notification(
                &conn,
                &crate::db::models::Notification {
                    id: 0,
                    media_id: Some(media_id),
                    message: "New episode available!".to_string(),
                    notification_type: "new_episode".to_string(),
                    read: false,
                    created_at: String::new(),
                },
            )
        })
        .await
        .unwrap()
        .unwrap();

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/notifications")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("New episode available!"));
    }

    #[tokio::test]
    async fn test_mark_notification_read() {
        let state = build_test_state();

        // Insert media + notification
        let pool = state.db.clone();
        let media_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_media(
                &conn,
                &Media {
                    id: 0,
                    media_type: "movie".to_string(),
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
        })
        .await
        .unwrap()
        .unwrap();

        let pool = state.db.clone();
        let notif_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_notification(
                &conn,
                &crate::db::models::Notification {
                    id: 0,
                    media_id: Some(media_id),
                    message: "Test notification".to_string(),
                    notification_type: "torrent_update".to_string(),
                    read: false,
                    created_at: String::new(),
                },
            )
        })
        .await
        .unwrap()
        .unwrap();

        let app = web::build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/notifications/{}/read", notif_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Verify notification is marked as read
        let pool = state.db.clone();
        let unread = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_unread_notifications(&conn)
        })
        .await
        .unwrap()
        .unwrap();
        assert!(unread.is_empty());
    }
}
