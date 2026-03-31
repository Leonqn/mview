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

async fn render_season_partial(
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

    // Both external operations succeeded — now persist to DB
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
    let torrent_id = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        queries::insert_torrent(&conn, &torrent)
    })
    .await??;

    info!(
        topic_id = form.topic_id,
        torrent_id, "downloaded torrent from search results"
    );

    render_season_partial(&state, season_id).await
}

/// Returns true if the title contains only ASCII characters.
fn is_ascii_title(title: &str) -> bool {
    title.is_ascii()
}

#[cfg(test)]
fn dedup_search_results(results: Vec<SearchResult>) -> Vec<SearchResult> {
    let mut seen = HashSet::new();
    results
        .into_iter()
        .filter(|r| seen.insert(r.topic_id.clone()))
        .collect()
}

/// Returns true if the name is a generic season label like "Season 1", "Сезон 2", etc.
fn is_generic_season_name(name: &str) -> bool {
    use regex::Regex;
    use std::sync::LazyLock;

    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(season|сезон|specials?)\s*\d*$").unwrap());
    RE.is_match(name.trim())
}

/// Parse an anime season title to extract the base title and season number.
///
/// AniList titles for sequels often follow patterns like:
/// - "Title 2nd Season" → ("Title", 2)
/// - "Title Season 2" → ("Title", 2)
/// - "Title Part 2" → ("Title", 2)
/// - "Title" (no suffix) → ("Title", 1)
fn parse_anime_season_title(title: &str) -> (&str, i64) {
    use regex::Regex;
    use std::sync::LazyLock;

    static ORDINAL_SEASON: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\s+((\d+)(?:st|nd|rd|th)\s+season)$").unwrap());
    static SEASON_NUM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\s+(season\s+(\d+))$").unwrap());
    static PART_NUM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\s+(part\s+(\d+))$").unwrap());
    static COUR_NUM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\s+(cour\s+(\d+))$").unwrap());

    for re in [&*ORDINAL_SEASON, &*SEASON_NUM, &*PART_NUM, &*COUR_NUM] {
        if let Some(caps) = re.captures(title) {
            let num: i64 = caps[2].parse().unwrap_or(1);
            let base = &title[..caps.get(0).unwrap().start()];
            let base = base.trim_end_matches(&[' ', ':'][..]).trim();
            return (base, num);
        }
    }

    (title, 1)
}

/// Build search queries for a season of a series.
/// Returns 4 queries: "Title Season N", "Title Сезон N", "Title TV-N", "Title ТВ-N".
fn build_season_queries(title: &str, season_number: i64) -> Vec<String> {
    vec![
        format!("{} Season {}", title, season_number),
        format!("{} Сезон {}", title, season_number),
        format!("{} TV-{}", title, season_number),
        format!("{} ТВ-{}", title, season_number),
    ]
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

    // Determine search name: use original title if ASCII, otherwise use localized title
    let search_name = media
        .title_original
        .as_deref()
        .filter(|t| is_ascii_title(t))
        .unwrap_or(&media.title);

    let season_name = season.title.as_deref().unwrap_or(search_name);
    let fmt = season.format.as_deref().unwrap_or("");

    let search_queries =
        if media.media_type == "movie" || fmt == "MOVIE" || fmt == "OVA" || fmt == "SPECIAL" {
            // Movies/OVA/specials — search by title only
            vec![season_name.to_string()]
        } else if media.media_type == "anime" && season.anilist_id.is_some() {
            // AniList anime — parse title to extract base name and season number,
            // then build TV-N queries from the season's own title (not media title)
            let (base_title, season_num) = parse_anime_season_title(season_name);
            let mut queries = vec![
                format!("{} TV-{}", base_title, season_num),
                format!("{} ТВ-{}", base_title, season_num),
            ];
            // AniList season title (e.g. "Fate/Zero 2nd Season") before base title
            if base_title != season_name {
                queries.push(season_name.to_string());
            }
            // Base title last as the broadest query
            queries.push(base_title.to_string());
            queries
        } else if media.media_type == "anime" {
            // TMDB anime — use media title with TV-N (season numbering from TMDB is correct)
            let mut queries = vec![
                format!("{} TV-{}", search_name, tv_season_number),
                format!("{} ТВ-{}", search_name, tv_season_number),
            ];
            if !is_generic_season_name(season_name) && season_name != search_name {
                queries.push(season_name.to_string());
            }
            queries.push(search_name.to_string());
            queries
        } else {
            let mut queries = build_season_queries(search_name, season.season_number);
            // Add season-specific title if it's meaningful (not just "Season N")
            if season_name != search_name && !is_generic_season_name(season_name) {
                queries.push(season_name.to_string());
            }
            queries.push(search_name.to_string());
            queries
        };

    info!(
        season_id,
        media_type = media.media_type,
        queries = ?search_queries,
        "searching season torrents"
    );

    // Execute all queries concurrently, tracking which query each result came from
    let mut all_results: Vec<(usize, SearchResult)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    let futures: Vec<_> = search_queries
        .iter()
        .map(|q| state.rutracker.search(q))
        .collect();

    let results = futures::future::join_all(futures).await;
    for (query_idx, result) in results.into_iter().enumerate() {
        match result {
            Ok(r) => all_results.extend(r.into_iter().map(|sr| (query_idx, sr))),
            Err(error) => {
                tracing::warn!(?error, "rutracker season search failed");
                errors.push(format!("search failed: {error}"));
            }
        }
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
    fn test_is_ascii_title() {
        assert!(is_ascii_title("Breaking Bad"));
        assert!(is_ascii_title("Attack on Titan"));
        assert!(is_ascii_title(""));
        assert!(!is_ascii_title("進撃の巨人"));
        assert!(!is_ascii_title("Наруто"));
        assert!(!is_ascii_title("Attack on Titan / 進撃の巨人"));
    }

    #[test]
    fn test_build_season_queries() {
        let queries = build_season_queries("Breaking Bad", 3);
        assert_eq!(queries.len(), 4);
        assert_eq!(queries[0], "Breaking Bad Season 3");
        assert_eq!(queries[1], "Breaking Bad Сезон 3");
        assert_eq!(queries[2], "Breaking Bad TV-3");
        assert_eq!(queries[3], "Breaking Bad ТВ-3");
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
