use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse, Redirect};
use axum::routing::{get, post};
use axum::{Form, Router};
use chrono::Local;
use serde::Deserialize;
use tracing::{error, info};

use crate::db::models::{Episode, Media, Notification, Season};
use crate::db::queries;
use crate::error::AppError;
use crate::rutracker::monitor;
use crate::web::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/media/track", post(track_media))
        .route("/api/media/{id}/delete", post(delete_media))
        .route("/api/media/{id}/add-torrent", post(add_torrent_to_media))
        .route("/api/torrents/{id}/download", post(download_torrent))
        .route("/api/torrents/{id}/progress", get(torrent_progress))
        .route(
            "/api/seasons/{id}/progress-badge",
            get(season_progress_badge),
        )
        .route("/api/torrents/{id}/update", post(check_torrent_update))
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
        let collection = state.tmdb.get_collection(collection_ref.id).await?;
        info!(
            collection = collection.name,
            parts = collection.parts.len(),
            "movie belongs to collection"
        );
        return track_movie_collection(state, &details, &collection).await;
    }

    // Standalone movie — single season stub
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
                title: None,
                episode_count: Some(1),
                anilist_id: None,
                format: None,
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
        title: collection.name.clone(),
        title_original: None,
        year: None,
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

#[derive(Debug, Deserialize)]
pub struct AddTorrentForm {
    pub topic_id: String,
    pub auto_update: Option<String>,
}

/// Add a RuTracker torrent to an existing tracked media item.
async fn add_torrent_to_media(
    State(state): State<Arc<AppState>>,
    Path(media_id): Path<i64>,
    Form(form): Form<AddTorrentForm>,
) -> Result<Redirect, AppError> {
    // Validate topic_id is numeric
    if form.topic_id.is_empty() || !form.topic_id.chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow::anyhow!("Invalid topic_id: must be a numeric value").into());
    }

    // Verify media exists
    let pool = state.db.clone();
    let mid = media_id;
    let media = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_media(&conn, mid)
    })
    .await??
    .ok_or_else(|| anyhow::anyhow!("Media {} not found", media_id))?;

    // Fetch topic info from RuTracker
    let topic_info = state.rutracker.parse_topic(&form.topic_id).await?;

    let auto_update = form.auto_update.as_deref() == Some("on");

    // Extract season number from topic info for series
    let season_number = if media.media_type == "series" {
        extract_season_number(&topic_info.title)
    } else {
        None
    };

    let torrent = crate::db::models::Torrent {
        id: 0,
        media_id,
        rutracker_topic_id: form.topic_id.clone(),
        title: topic_info.title,
        quality: topic_info.quality,
        size_bytes: Some(topic_info.size_bytes),
        seeders: Some(topic_info.seeders as i64),
        season_number,
        episode_info: None,
        registered_at: topic_info.registered_at,
        last_checked_at: None,
        torrent_hash: topic_info.torrent_hash,
        qbt_hash: None,
        status: "active".to_string(),
        auto_update,
        created_at: String::new(),
        updated_at: String::new(),
    };

    let topic_id_for_log = form.topic_id.clone();
    let pool = state.db.clone();
    let insert_result = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::insert_torrent(&conn, &torrent)
    })
    .await?;

    match insert_result {
        Ok(_) => {
            info!(
                topic_id = topic_id_for_log,
                media_id,
                title = media.title,
                "added torrent to media"
            );
        }
        Err(e) if e.to_string().contains("UNIQUE constraint failed") => {
            info!(
                topic_id = topic_id_for_log,
                media_id, "torrent already exists for media, redirecting"
            );
        }
        Err(e) => return Err(e.into()),
    }

    let redirect_path = format!("/media/{}", media_id);
    Ok(Redirect::to(&redirect_path))
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

            let episodes: Vec<Episode> = (1..=episode_count.unwrap_or(0))
                .map(|ep_num| Episode {
                    id: 0,
                    season_id: 0,
                    episode_number: ep_num,
                    title: None,
                    air_date: entry.episode_air_date(ep_num),
                    downloaded: false,
                    file_path: None,
                })
                .collect();

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

/// Extract season number from a torrent title (e.g., "S01" or "Season 1").
fn extract_season_number(title: &str) -> Option<i64> {
    // Try S01 pattern - search case-insensitively by scanning all positions
    let upper = title.to_uppercase();
    let bytes = upper.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'S'
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_digit()
            && (i == 0 || !bytes[i - 1].is_ascii_alphanumeric())
        {
            let after = &upper[i + 1..];
            if let Some(num_str) = after.split(|c: char| !c.is_ascii_digit()).next()
                && let Ok(n) = num_str.parse::<i64>()
                && n > 0
                && n < 100
            {
                return Some(n);
            }
        }
    }
    // Try "Сезон N" (Russian) and "Season N"
    for pattern in &["Сезон ", "Season "] {
        if let Some(pos) = title.find(pattern) {
            let after = &title[pos + pattern.len()..];
            if let Some(num_str) = after.split(|c: char| !c.is_ascii_digit()).next()
                && let Ok(n) = num_str.parse::<i64>()
                && n > 0
                && n < 100
            {
                return Some(n);
            }
        }
    }
    None
}

async fn download_torrent(
    State(state): State<Arc<AppState>>,
    Path(torrent_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    // Get torrent record from DB
    let pool = state.db.clone();
    let torrent = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_torrent(&conn, torrent_id)
    })
    .await??
    .ok_or_else(|| anyhow::anyhow!("Torrent {} not found", torrent_id))?;

    let topic_id = torrent.rutracker_topic_id.clone();

    // Download .torrent file from RuTracker
    let torrent_bytes = state.rutracker.download_torrent(&topic_id).await?;

    let filename = format!("{}.torrent", topic_id);
    let save_path = state.config.paths.download_dir.clone();

    // Send to qBittorrent and get hash
    let qbt_hash = {
        let mut qbt = state.qbittorrent.lock().await;
        qbt.ensure_logged_in().await?;
        qbt.add_torrent(&torrent_bytes, &filename, &save_path, "mview")
            .await?
    };

    info!(
        torrent_id,
        topic_id = topic_id,
        qbt_hash = ?qbt_hash,
        "torrent sent to qbittorrent"
    );

    // Update torrent status and qbt_hash in DB
    let pool = state.db.clone();
    let tid = torrent.id;
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::update_torrent_checked(&conn, tid)?;
        if let Some(ref hash) = qbt_hash {
            queries::update_torrent_qbt_hash(&conn, tid, hash)?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await??;

    // Return updated torrent row partial
    let tmpl = state.templates.get_template("partials/torrent_row.html")?;
    let html = tmpl.render(minijinja::context! {
        torrent => {
            let pool = state.db.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                queries::get_torrent(&conn, torrent_id)
            })
            .await??.unwrap()
        }
    })?;
    Ok(Html(html))
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

    let html = match progress {
        Some(p) if p >= 100.0 => "<span style=\"color:green;\">Completed</span>".to_string(),
        Some(p) => {
            format!("<span style=\"color:orange;\">Downloading {p}%</span>")
        }
        None => "<span style=\"color:orange;\">Downloading</span>".to_string(),
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

/// Manual update check: fetch topic info from RuTracker and compare registered_at.
/// If updated, re-download and re-add to qBittorrent.
async fn check_torrent_update(
    State(state): State<Arc<AppState>>,
    Path(torrent_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    // Get torrent from DB
    let pool = state.db.clone();
    let torrent = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_torrent(&conn, torrent_id)
    })
    .await??
    .ok_or_else(|| anyhow::anyhow!("Torrent {} not found", torrent_id))?;

    let topic_id = torrent.rutracker_topic_id.clone();

    // Fetch current topic info from RuTracker
    let topic_info = state.rutracker.parse_topic(&topic_id).await?;

    // Compare torrent hash / registered_at
    let result = monitor::check_update(
        &torrent,
        topic_info.registered_at.as_deref(),
        topic_info.torrent_hash.as_deref(),
    );

    // Update last_checked_at
    let pool = state.db.clone();
    let tid = torrent.id;
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::update_torrent_checked(&conn, tid)
    })
    .await??;

    if !result.has_update {
        // No download needed — safe to persist new metadata now
        if let Some(ref new_date) = result.new_registered_at {
            let tid = torrent.id;
            let new_date = new_date.clone();
            let pool = state.db.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                queries::update_torrent_registered_at(&conn, tid, &new_date)
            })
            .await??;
        }
    }

    if result.has_update {
        info!(
            title = torrent.title,
            "manual update check: torrent has update, re-downloading"
        );

        // Download updated .torrent and re-add to qBittorrent
        let torrent_bytes = state.rutracker.download_torrent(&topic_id).await?;

        let filename = format!("{}.torrent", topic_id);
        let save_path = state.config.paths.download_dir.clone();

        let new_hash = {
            let mut qbt = state.qbittorrent.lock().await;
            qbt.ensure_logged_in().await?;
            qbt.add_torrent(&torrent_bytes, &filename, &save_path, "mview")
                .await?
        };

        // Download succeeded — persist new metadata and qbt hash together
        let tid = torrent.id;
        let new_registered_at = result.new_registered_at.clone();
        let new_hash_clone = new_hash.clone();
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            if let Some(ref date) = new_registered_at {
                queries::update_torrent_registered_at(&conn, tid, date)?;
            }
            if let Some(ref hash) = new_hash_clone {
                queries::update_torrent_qbt_hash(&conn, tid, hash)?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;

        // Create notification
        let media_id = torrent.media_id;
        let notification_msg = format!("Torrent updated (manual): {}", torrent.title);
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
    }

    // Return updated torrent row partial
    let tmpl = state.templates.get_template("partials/torrent_row.html")?;
    let html = tmpl.render(minijinja::context! {
        torrent => {
            let pool = state.db.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                queries::get_torrent(&conn, torrent_id)
            })
            .await??.unwrap()
        }
    })?;
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
    use crate::db::models::{Media, Torrent};
    use crate::web;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

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

        assert_eq!(media.title, "Harry Potter Collection");
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
    async fn test_download_torrent_endpoint_exists() {
        let state = build_test_state();

        // Insert a media and torrent to get a valid torrent ID
        let pool = state.db.clone();
        let torrent_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            let media_id = queries::insert_media(
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
            )?;
            queries::insert_torrent(
                &conn,
                &Torrent {
                    id: 0,
                    media_id,
                    rutracker_topic_id: "123456".to_string(),
                    title: "Test Torrent".to_string(),
                    quality: Some("1080p".to_string()),
                    size_bytes: Some(1000000),
                    seeders: Some(10),
                    season_number: None,
                    episode_info: None,
                    registered_at: None,
                    last_checked_at: None,
                    torrent_hash: None,
                    qbt_hash: None,
                    status: "active".to_string(),
                    auto_update: false,
                    created_at: String::new(),
                    updated_at: String::new(),
                },
            )
        })
        .await
        .unwrap()
        .unwrap();

        let app = web::build_router(state);

        // Will fail with 500 (can't reach RuTracker/qBittorrent) but route exists (not 404)
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/torrents/{}/download", torrent_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_download_torrent_not_found() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/torrents/99999/download")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return 500 (torrent not found error), not 404 (route not found)
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
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
    async fn test_update_torrent_endpoint_exists() {
        let state = build_test_state();

        // Insert a media and torrent
        let pool = state.db.clone();
        let torrent_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
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
            )?;
            queries::insert_torrent(
                &conn,
                &Torrent {
                    id: 0,
                    media_id,
                    rutracker_topic_id: "123456".to_string(),
                    title: "Test Torrent".to_string(),
                    quality: Some("1080p".to_string()),
                    size_bytes: Some(1000000),
                    seeders: Some(10),
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
        })
        .await
        .unwrap()
        .unwrap();

        let app = web::build_router(state);

        // Will fail with 500 (can't reach RuTracker) but route exists (not 404)
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/torrents/{}/update", torrent_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_ne!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_update_torrent_not_found() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/torrents/99999/update")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
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

    #[test]
    fn test_extract_season_number_s01_pattern() {
        assert_eq!(extract_season_number("Show.S01.1080p"), Some(1));
        assert_eq!(extract_season_number("Show.S12E03.720p"), Some(12));
    }

    #[test]
    fn test_extract_season_number_lowercase() {
        assert_eq!(extract_season_number("show.s02e01.1080p"), Some(2));
    }

    #[test]
    fn test_extract_season_number_skips_non_season_s() {
        // "Stranger" has an 'S' but it's not followed by a digit
        assert_eq!(extract_season_number("Stranger.Things.S03.1080p"), Some(3));
        // "Super" has an 'S' not followed by digit, should find S2
        assert_eq!(extract_season_number("Super S2 720p"), Some(2));
    }

    #[test]
    fn test_extract_season_number_season_keyword() {
        assert_eq!(extract_season_number("Show Season 5"), Some(5));
    }

    #[test]
    fn test_extract_season_number_russian() {
        assert_eq!(extract_season_number("Шоу Сезон 3"), Some(3));
    }

    #[test]
    fn test_extract_season_number_none() {
        assert_eq!(extract_season_number("Movie.2024.1080p"), None);
        assert_eq!(extract_season_number("No season here"), None);
    }

    #[test]
    fn test_extract_season_number_ignores_non_season_prefixes() {
        // "DTS5.1" should not match as season 5
        assert_eq!(extract_season_number("Movie.DTS5.1.1080p"), None);
        // "PS4" should not match as season 4
        assert_eq!(extract_season_number("Game.PS4.Edition"), None);
        // "MS3" should not match as season 3
        assert_eq!(extract_season_number("Release.MS3.Final"), None);
        // But ".S3." should still match (preceded by non-alphanumeric)
        assert_eq!(extract_season_number("Show.S3.720p"), Some(3));
        // At start of string should also match
        assert_eq!(extract_season_number("S1 Episode 5"), Some(1));
    }
}
