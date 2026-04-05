use std::collections::HashSet;
use std::sync::Arc;

use axum::Form;
use axum::Router;
use axum::extract::{Path, State};
use axum::response::Html;
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use tracing::{debug, info};

use crate::db::queries;
use crate::error::AppError;
use crate::rutracker::search::SearchResult;
use crate::web::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/media/{id}", get(media_detail))
        .route("/media/seasons/{id}/status", post(update_season_status))
        .route("/media/seasons/{id}/search", post(search_season))
        .route("/media/seasons/{id}/download", post(download_search_result))
}

#[derive(Debug, Serialize)]
struct SeasonWithEpisodes {
    season: crate::db::models::Season,
    episodes: Vec<crate::db::models::Episode>,
    next_episode: Option<crate::db::models::Episode>,
    torrents: Vec<crate::db::models::Torrent>,
    /// qbt_hash -> progress percentage (0-100)
    torrent_progress: std::collections::HashMap<String, f64>,
    downloading: bool,
    /// Download progress percentage (0-100) for the first active torrent
    download_progress: Option<f64>,
}

async fn media_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let (media, db_seasons, torrents, all_episodes) = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let media =
            queries::get_media(&conn, id)?.ok_or_else(|| anyhow::anyhow!("Media not found"))?;
        let media_id = media.id;
        let seasons = queries::get_seasons_for_media(&conn, media_id)?;
        let torrents = queries::get_torrents_for_media(&conn, media_id)?;
        let mut all_episodes = Vec::new();
        for s in &seasons {
            let eps = queries::get_episodes_for_season(&conn, s.id)?;
            all_episodes.push(eps);
        }
        Ok::<_, anyhow::Error>((media, seasons, torrents, all_episodes))
    })
    .await??;

    // Fetch download progress from qBittorrent for active torrents
    let qbt_progress: std::collections::HashMap<String, f64> = {
        let hashes: Vec<String> = torrents.iter().filter_map(|t| t.qbt_hash.clone()).collect();
        if hashes.is_empty() {
            std::collections::HashMap::new()
        } else {
            let mut qbt = state.qbittorrent.lock().await;
            match qbt.get_torrents(Some("mview")).await {
                Ok(qbt_torrents) => qbt_torrents
                    .into_iter()
                    .map(|t| (t.hash, t.progress))
                    .collect(),
                Err(_) => std::collections::HashMap::new(),
            }
        }
    };

    let mut seasons_with_episodes = Vec::new();
    for (season, episodes) in db_seasons.into_iter().zip(all_episodes) {
        let season_torrents: Vec<_> = torrents
            .iter()
            .filter(|t| t.season_number == Some(season.season_number))
            .cloned()
            .collect();
        let season_torrent = season_torrents.iter().find(|t| t.qbt_hash.is_some());
        let downloading = season_torrent.is_some();
        let download_progress = season_torrent
            .and_then(|t| t.qbt_hash.as_ref())
            .and_then(|h| qbt_progress.get(h))
            .map(|p| (p * 100.0 * 10.0).round() / 10.0);
        let torrent_progress: std::collections::HashMap<String, f64> = season_torrents
            .iter()
            .filter_map(|t| t.qbt_hash.as_ref())
            .filter_map(|h| {
                qbt_progress
                    .get(h)
                    .map(|p| (h.clone(), (p * 100.0 * 10.0).round() / 10.0))
            })
            .collect();
        let next_episode = episodes.iter().find(|e| !e.downloaded).cloned();
        seasons_with_episodes.push(SeasonWithEpisodes {
            season,
            episodes,
            next_episode,
            torrents: season_torrents,
            torrent_progress,
            downloading,
            download_progress,
        });
    }
    seasons_with_episodes.sort_by(|a, b| b.season.season_number.cmp(&a.season.season_number));

    debug!(
        media_id = media.id,
        title = media.title,
        media_type = media.media_type,
        "viewing media detail"
    );

    let plex_configured = !state.config.plex.url.is_empty() && !state.config.plex.token.is_empty();

    let tmpl = state.templates.get_template("series.html")?;
    let html = tmpl.render(minijinja::context! {
        media => media,
        seasons => seasons_with_episodes,
        plex_configured => plex_configured,
    })?;
    Ok(Html(html))
}

#[derive(Debug, Deserialize)]
struct UpdateSeasonStatusForm {
    status: String,
}

async fn build_season_entry(
    state: &Arc<AppState>,
    season_id: i64,
) -> Result<SeasonWithEpisodes, AppError> {
    let pool = state.db.clone();
    let (season, episodes, season_torrents) = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let season = queries::get_season(&conn, season_id)?
            .ok_or_else(|| anyhow::anyhow!("season not found"))?;
        let episodes = queries::get_episodes_for_season(&conn, season_id)?;
        let all_torrents = queries::get_torrents_for_media(&conn, season.media_id)?;
        let season_torrents: Vec<_> = all_torrents
            .into_iter()
            .filter(|t| t.season_number == Some(season.season_number))
            .collect();
        Ok::<_, anyhow::Error>((season, episodes, season_torrents))
    })
    .await??;

    let qbt_hashes: Vec<String> = season_torrents
        .iter()
        .filter_map(|t| t.qbt_hash.clone())
        .collect();
    let qbt_progress: std::collections::HashMap<String, f64> = if qbt_hashes.is_empty() {
        std::collections::HashMap::new()
    } else {
        let mut qbt = state.qbittorrent.lock().await;
        match qbt.get_torrents(Some("mview")).await {
            Ok(qbt_torrents) => qbt_torrents
                .into_iter()
                .filter(|t| qbt_hashes.contains(&t.hash))
                .map(|t| (t.hash, (t.progress * 100.0 * 10.0).round() / 10.0))
                .collect(),
            Err(_) => std::collections::HashMap::new(),
        }
    };

    let season_torrent = season_torrents.iter().find(|t| t.qbt_hash.is_some());
    let downloading = season_torrent.is_some();
    let download_progress = season_torrent
        .and_then(|t| t.qbt_hash.as_ref())
        .and_then(|h| qbt_progress.get(h))
        .copied();
    let next_episode = episodes.iter().find(|e| !e.downloaded).cloned();
    Ok(SeasonWithEpisodes {
        season,
        episodes,
        next_episode,
        torrents: season_torrents,
        torrent_progress: qbt_progress,
        downloading,
        download_progress,
    })
}

pub async fn render_season_partial(
    state: &Arc<AppState>,
    season_id: i64,
) -> Result<Html<String>, AppError> {
    let entry = build_season_entry(state, season_id).await?;
    let tmpl = state.templates.get_template("partials/season.html")?;
    let html = tmpl.render(minijinja::context! { entry => entry })?;
    Ok(Html(html))
}

async fn update_season_status(
    State(state): State<Arc<AppState>>,
    Path(season_id): Path<i64>,
    Form(form): Form<UpdateSeasonStatusForm>,
) -> Result<Html<String>, AppError> {
    let status = form.status.clone();
    if !["tracking", "ignored", "completed"].contains(&status.as_str()) {
        return Err(anyhow::anyhow!("Invalid status: {}", status).into());
    }

    let pool = state.db.clone();
    tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::update_season_status(&conn, season_id, &status)
    })
    .await??;

    render_season_partial(&state, season_id).await
}

#[derive(Debug, Deserialize)]
struct DownloadSearchForm {
    topic_id: String,
    media_id: i64,
    #[serde(default)]
    size_bytes: Option<i64>,
}

async fn download_search_result(
    State(state): State<Arc<AppState>>,
    Path(season_id): Path<i64>,
    Form(form): Form<DownloadSearchForm>,
) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let season = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::get_season(&conn, season_id)
    })
    .await??
    .ok_or_else(|| anyhow::anyhow!("season not found"))?;

    // Check if torrent already exists in DB for this media
    let pool = state.db.clone();
    let existing_topic_id = form.topic_id.clone();
    let existing_media_id = form.media_id;
    let existing_torrent = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let torrents = queries::get_torrents_for_media(&conn, existing_media_id)?;
        Ok::<_, anyhow::Error>(
            torrents
                .into_iter()
                .find(|t| t.rutracker_topic_id == existing_topic_id),
        )
    })
    .await??;

    // Parse topic info from RuTracker
    let topic_info = state.rutracker.parse_topic(&form.topic_id).await?;

    // Download .torrent and send to qBittorrent before touching DB
    let torrent_bytes = state.rutracker.download_torrent(&form.topic_id).await?;
    let filename = format!("{}.torrent", form.topic_id);
    let save_path = state.config.paths.download_dir.clone();

    {
        let mut qbt = state.qbittorrent.lock().await;
        qbt.ensure_logged_in().await?;
        qbt.add_torrent(&torrent_bytes, &filename, &save_path, "mview")
            .await?;
    }

    // Use torrent_hash from rutracker magnet link as qbt_hash
    let qbt_hash = topic_info.torrent_hash.clone();

    let torrent_id = if let Some(existing) = existing_torrent {
        // Torrent already in DB — just update qbt_hash
        let tid = existing.id;
        if let Some(ref hash) = qbt_hash {
            let hash = hash.clone();
            let pool = state.db.clone();
            tokio::task::spawn_blocking(move || {
                let conn = pool.get()?;
                queries::update_torrent_qbt_hash(&conn, tid, &hash)
            })
            .await??;
        }
        tid
    } else {
        // Insert new torrent
        let torrent = crate::db::models::Torrent {
            id: 0,
            media_id: form.media_id,
            rutracker_topic_id: form.topic_id.clone(),
            title: topic_info.title,
            quality: topic_info.quality,
            size_bytes: form
                .size_bytes
                .filter(|&s| s > 0)
                .or(Some(topic_info.size_bytes)),
            seeders: Some(topic_info.seeders as i64),
            season_number: Some(season.season_number),
            episode_info: None,
            registered_at: topic_info.registered_at,
            last_checked_at: None,
            torrent_hash: topic_info.torrent_hash,
            qbt_hash,
            status: "active".to_string(),
            auto_update: true,
            created_at: String::new(),
            updated_at: String::new(),
        };

        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_torrent(&conn, &torrent)
        })
        .await??
    };

    // Auto-set season to tracking when downloading a torrent
    if season.status != "tracking" {
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::update_season_status(&conn, season_id, "tracking")
        })
        .await??;
    }

    info!(
        topic_id = form.topic_id,
        torrent_id, "downloaded torrent from search results"
    );

    render_season_partial(&state, season_id).await
}

#[cfg(test)]
fn dedup_search_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut seen = HashSet::new();
    results
        .into_iter()
        .filter(|r| seen.insert(r.topic_id.clone()))
        .collect()
}

async fn search_season(
    State(state): State<Arc<AppState>>,
    Path(season_id): Path<i64>,
) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let (season, media, tv_season_number) = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let season = queries::get_season(&conn, season_id)?
            .ok_or_else(|| anyhow::anyhow!("season not found"))?;
        let media = queries::get_media(&conn, season.media_id)?
            .ok_or_else(|| anyhow::anyhow!("media not found"))?;
        // Count TV-only season number for search (skip movies/OVA)
        let all_seasons = queries::get_seasons_for_media(&conn, media.id)?;
        let tv_num = all_seasons
            .iter()
            .filter(|s| matches!(s.format.as_deref(), Some("TV") | Some("TV_SHORT") | None))
            .position(|s| s.id == season.id)
            .map(|p| (p as i64) + 1)
            .unwrap_or(1);
        Ok::<_, anyhow::Error>((season, media, tv_num))
    })
    .await??;

    let sq = crate::search::build_queries(&media, &season, tv_season_number);

    info!(
        season_id,
        media_type = media.media_type,
        primary = ?sq.primary,
        fallback = ?sq.fallback,
        broad_fallback = ?sq.broad_fallback,
        "searching season torrents"
    );

    let mut all_results: Vec<(usize, SearchResult)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut base_idx: usize = 0;

    // Try each tier in order, stopping when one returns results.
    for (tier_name, queries) in [
        ("primary", &sq.primary),
        ("fallback", &sq.fallback),
        ("broad_fallback", &sq.broad_fallback),
    ] {
        if queries.is_empty() {
            continue;
        }
        if !all_results.is_empty() {
            break;
        }
        if base_idx > 0 {
            info!(tier = tier_name, queries = ?queries, "previous tier empty, trying next");
        }
        let futures: Vec<_> = queries.iter().map(|q| state.rutracker.search(q)).collect();
        let results = futures::future::join_all(futures).await;
        for (query_idx, result) in results.into_iter().enumerate() {
            let query_str = &queries[query_idx];
            match result {
                Ok(r) => {
                    // Rutracker ignores `-` in search terms (treats it as word separator
                    // or "exclude" operator). If our query had a "TV-N" / "ТВ-N" marker,
                    // post-filter results to drop those with a conflicting marker.
                    let filtered = filter_conflicting_season_markers(r, query_str);
                    all_results.extend(filtered.into_iter().map(|sr| (base_idx + query_idx, sr)));
                }
                Err(error) => {
                    tracing::warn!(tier = tier_name, ?error, "rutracker search failed");
                    errors.push(format!("search failed: {error}"));
                }
            }
        }
        base_idx += queries.len();
    }

    // Deduplicate by topic_id, keeping the first (lowest query index) occurrence
    let mut seen = HashSet::new();
    all_results.retain(|(_, sr)| seen.insert(sr.topic_id.clone()));

    // Sort by query index first (title-only results first, TV-N last), then by seeders
    all_results.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.seeders.cmp(&a.1.seeders)));

    let results: Vec<SearchResult> = all_results.into_iter().map(|(_, sr)| sr).collect();

    let tmpl = state
        .templates
        .get_template("partials/season_search_results.html")?;
    let html = tmpl.render(minijinja::context! {
        results => results,
        errors => errors,
        season_id => season_id,
        media_id => media.id,
    })?;
    Ok(Html(html))
}

/// Extract a "TV-N" or "ТВ-N" marker from a string, returning the number.
/// Case-insensitive, supports optional space/dash between "TV" and the number.
fn extract_season_marker(s: &str) -> Option<i64> {
    use std::sync::LazyLock;
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:TV|ТВ)[-\s]?(\d+)").unwrap());
    RE.captures(s)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<i64>().ok())
}

/// Drop rutracker results that mention a different TV-N / ТВ-N marker than the query.
/// If the query has no season marker, all results pass through.
fn filter_conflicting_season_markers(results: Vec<SearchResult>, query: &str) -> Vec<SearchResult> {
    let expected = match extract_season_marker(query) {
        Some(n) => n,
        None => return results,
    };
    results
        .into_iter()
        .filter(|r| match extract_season_marker(&r.title) {
            Some(n) => n == expected,
            None => true, // no marker in title → keep
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db;
    use crate::db::models::{Episode, Media, Season, Torrent};
    use crate::web;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn test_extract_season_marker() {
        assert_eq!(extract_season_marker("Fate UBW TV-1 [BDRip]"), Some(1));
        assert_eq!(extract_season_marker("Fate UBW ТВ-2 [BDRip]"), Some(2));
        assert_eq!(extract_season_marker("Show tv-3 rip"), Some(3));
        assert_eq!(extract_season_marker("Show TV 3 rip"), Some(3));
        assert_eq!(extract_season_marker("Something Else"), None);
        assert_eq!(extract_season_marker("Show [1080p]"), None);
    }

    #[test]
    fn test_filter_conflicting_season_markers() {
        let make = |title: &str| SearchResult {
            topic_id: "1".to_string(),
            title: title.to_string(),
            size: "1 GB".to_string(),
            size_bytes: 1_000_000_000,
            seeders: 10,
            leechers: 1,
            forum_name: "Anime".to_string(),
            url: "http://example.com".to_string(),
        };

        let results = vec![
            make("Fate UBW ТВ-1 [BDRip]"),
            make("Fate UBW ТВ-2 [BDRip]"),
            make("Fate UBW TV-1 [BDRip]"),
            make("Fate UBW [BDRip]"), // no marker
        ];

        let filtered = filter_conflicting_season_markers(results, "Fate UBW TV-1");
        assert_eq!(filtered.len(), 3);
        // ТВ-2 is dropped
        assert!(!filtered.iter().any(|r| r.title.contains("ТВ-2")));

        // No marker in query → all results pass
        let all = vec![make("Any title"), make("Another TV-5")];
        let filtered = filter_conflicting_season_markers(all, "Fate UBW");
        assert_eq!(filtered.len(), 2);
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

    async fn insert_test_series(state: &AppState) -> (i64, i64, i64) {
        let media = Media {
            id: 0,
            media_type: "series".to_string(),
            title: "Breaking Bad".to_string(),
            title_original: Some("Breaking Bad".to_string()),
            year: Some(2008),
            tmdb_id: Some(1396),
            imdb_id: Some("tt0903747".to_string()),
            kinopoisk_url: Some("https://kinopoisk.ru/film/404900".to_string()),
            world_art_url: Some("https://world-art.ru/cinema/12345".to_string()),
            poster_url: Some("https://image.tmdb.org/t/p/w300/poster.jpg".to_string()),
            overview: Some("A chemistry teacher turned meth kingpin".to_string()),
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
            episode_count: Some(7),
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
            air_date: Some("2008-01-20".to_string()),
            downloaded: false,
            file_path: None,
        };

        let pool = state.db.clone();
        let episode_id = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_episode(&conn, &episode)
        })
        .await
        .unwrap()
        .unwrap();

        (media_id, season_id, episode_id)
    }

    #[tokio::test]
    async fn test_series_detail_returns_200() {
        let state = build_test_state();
        let (media_id, _, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_series_detail_contains_media_info() {
        let state = build_test_state();
        let (media_id, _, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Breaking Bad"));
        assert!(body_str.contains("2008"));
        assert!(body_str.contains("chemistry teacher"));
    }

    #[tokio::test]
    async fn test_series_detail_contains_seasons_and_episodes() {
        let state = build_test_state();
        let (media_id, _, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Season 1"));
        assert!(body_str.contains("Pilot"));
    }

    #[tokio::test]
    async fn test_series_detail_contains_external_links() {
        let state = build_test_state();
        let (media_id, _, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("tmdb.org"));
        assert!(body_str.contains("imdb.com"));
        assert!(body_str.contains("kinopoisk.ru"));
        assert!(body_str.contains("world-art.ru"));
    }

    #[tokio::test]
    async fn test_series_detail_shows_torrents() {
        let state = build_test_state();
        let (media_id, _, _) = insert_test_series(&state).await;

        // Insert a torrent
        let torrent = Torrent {
            id: 0,
            media_id,
            rutracker_topic_id: "999".to_string(),
            title: "Breaking.Bad.S01.1080p".to_string(),
            quality: Some("1080p".to_string()),
            size_bytes: Some(15_000_000_000),
            seeders: Some(100),
            season_number: Some(1),
            episode_info: Some("1-7".to_string()),
            registered_at: None,
            last_checked_at: None,
            torrent_hash: None,
            qbt_hash: None,
            status: "active".to_string(),
            auto_update: true,
            created_at: String::new(),
            updated_at: String::new(),
        };

        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::insert_torrent(&conn, &torrent)
        })
        .await
        .unwrap()
        .unwrap();

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Breaking.Bad.S01.1080p"));
        assert!(body_str.contains("1080p"));
    }

    #[tokio::test]
    async fn test_series_not_found() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/media/99999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_season_status_toggle() {
        let state = build_test_state();
        let (_, season_id, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/media/seasons/{}/status", season_id))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("status=ignored"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_season_status_toggle_updates_db() {
        let state = build_test_state();
        let (media_id, season_id, _) = insert_test_series(&state).await;

        let app = web::build_router(state.clone());
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/media/seasons/{}/status", season_id))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("status=ignored"))
                .unwrap(),
        )
        .await
        .unwrap();

        // Verify DB was updated
        let pool = state.db.clone();
        let seasons = tokio::task::spawn_blocking(move || {
            let conn = pool.get()?;
            queries::get_seasons_for_media(&conn, media_id)
        })
        .await
        .unwrap()
        .unwrap();

        assert_eq!(seasons[0].status, "ignored");
    }

    #[tokio::test]
    async fn test_season_status_invalid() {
        let state = build_test_state();
        let (_, season_id, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/media/seasons/{}/status", season_id))
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("status=invalid"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn test_dedup_search_results() {
        let results = vec![
            SearchResult {
                topic_id: "1".to_string(),
                title: "First".to_string(),
                size: "1 GB".to_string(),
                size_bytes: 1_000_000_000,
                seeders: 10,
                leechers: 1,
                forum_name: "Test".to_string(),
                url: "http://example.com/1".to_string(),
            },
            SearchResult {
                topic_id: "1".to_string(),
                title: "Duplicate".to_string(),
                size: "1 GB".to_string(),
                size_bytes: 1_000_000_000,
                seeders: 5,
                leechers: 1,
                forum_name: "Test".to_string(),
                url: "http://example.com/1".to_string(),
            },
            SearchResult {
                topic_id: "2".to_string(),
                title: "Second".to_string(),
                size: "2 GB".to_string(),
                size_bytes: 2_000_000_000,
                seeders: 20,
                leechers: 2,
                forum_name: "Test".to_string(),
                url: "http://example.com/2".to_string(),
            },
        ];

        let deduped = dedup_search_results(results);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].title, "First");
        assert_eq!(deduped[1].title, "Second");
    }

    #[tokio::test]
    async fn test_series_detail_contains_search_button() {
        let state = build_test_state();
        let (media_id, _, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains(">Search</button>"));
        assert!(body_str.contains("/media/seasons/"));
        assert!(body_str.contains("/search"));
    }

    #[tokio::test]
    async fn test_download_search_result_season_not_found() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/media/seasons/99999/download")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("topic_id=123456&media_id=1"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn test_series_detail_contains_episodes_details() {
        let state = build_test_state();
        let (media_id, _, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("<details"));
        assert!(body_str.contains("<summary>"));
    }

    #[tokio::test]
    async fn test_series_detail_contains_download_route() {
        let state = build_test_state();
        let (media_id, _, _) = insert_test_series(&state).await;

        let app = web::build_router(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!("/media/{}", media_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("/media/seasons/"));
        assert!(body_str.contains("Tracking"));
    }
}
