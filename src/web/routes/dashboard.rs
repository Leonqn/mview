use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use serde::Serialize;

use chrono::{Datelike, Local};

use crate::db::queries;
use crate::error::AppError;
use crate::web::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/", get(dashboard))
}

#[derive(Debug, Serialize)]
struct SeasonDashboardInfo {
    season_number: i64,
    title: Option<String>,
    downloaded: usize,
    total: usize,
    downloading: bool,
    pending: bool,
}

#[derive(Debug, Serialize)]
struct MediaDashboardItem {
    #[serde(flatten)]
    media: crate::db::models::Media,
    tracking_seasons: Vec<SeasonDashboardInfo>,
    has_pending: bool,
}

async fn dashboard(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let items = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let media_list = queries::get_all_media(&conn)?;
        let mut items = Vec::new();
        for media in media_list {
            let today = Local::now().format("%Y-%m-%d").to_string();
            let seasons = queries::get_tracking_seasons_for_media(&conn, media.id)?;
            let torrents = queries::get_torrents_for_media(&conn, media.id)?;
            let season_ids: Vec<i64> = seasons.iter().map(|s| s.id).collect();
            let search_cache = queries::get_search_cache_for_seasons(&conn, &season_ids)?;
            let mut season_infos = Vec::new();
            let mut any_pending = false;
            let current_year = Local::now().year();
            for s in &seasons {
                let episodes = queries::get_episodes_for_season(&conn, s.id).unwrap_or_default();
                let media_released = media
                    .year
                    .map(|y| y <= current_year as i64)
                    .unwrap_or(false);
                let is_movie = media.media_type == "movie";
                let aired: Vec<_> = episodes
                    .iter()
                    .filter(|e| {
                        let date = e.air_date.as_deref().filter(|d| !d.is_empty());
                        match date {
                            Some(d) => d <= today.as_str(),
                            // For movies fall back to year-based release; for series require real air_date
                            None => is_movie && media_released,
                        }
                    })
                    .collect();
                let downloaded = aired.iter().filter(|e| e.downloaded).count();
                let total = aired.len();
                let has_torrent = torrents
                    .iter()
                    .any(|t| t.season_number == Some(s.season_number));
                let downloading = torrents.iter().any(|t| {
                    t.status == "active"
                        && t.season_number == Some(s.season_number)
                        && t.qbt_hash.is_some()
                });
                let search_found = search_cache.iter().any(|c| c.season_id == s.id);
                let pending = downloaded < total && search_found && !has_torrent;
                if pending {
                    any_pending = true;
                }
                season_infos.push(SeasonDashboardInfo {
                    season_number: s.season_number,
                    title: s.title.clone(),
                    downloaded,
                    total,
                    downloading,
                    pending,
                });
            }
            let (tracking_seasons, has_pending) = (season_infos, any_pending);
            items.push(MediaDashboardItem {
                media,
                tracking_seasons,
                has_pending,
            });
        }
        // Sort: pending items first, then by creation date desc
        items.sort_by_key(|b| std::cmp::Reverse(b.has_pending));
        Ok::<_, anyhow::Error>(items)
    })
    .await??;

    let tmpl = state.templates.get_template("dashboard.html")?;
    let html = tmpl.render(minijinja::context! { media => items })?;
    Ok(Html(html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db;
    use crate::rutracker::client::RutrackerClient;
    use crate::tmdb::client::TmdbClient;
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
        let rt_client = RutrackerClient::new(&config.rutracker.url, auth_handle.clone());
        let tmdb_client = TmdbClient::new(&config.tmdb.api_key).unwrap();
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
    async fn test_dashboard_returns_200() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dashboard_returns_html() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(content_type.contains("text/html"));
    }

    #[tokio::test]
    async fn test_dashboard_contains_title() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("mview"));
    }

    #[tokio::test]
    async fn test_static_route_exists() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/static/style.css")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // Should return 200 since static/style.css exists
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_dashboard_shows_tracked_media() {
        let state = build_test_state();

        // Insert a media item
        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().unwrap();
            queries::insert_media(
                &conn,
                &crate::db::models::Media {
                    id: 0,
                    media_type: "movie".to_string(),
                    title: "Test Movie".to_string(),
                    title_original: None,
                    year: Some(2024),
                    tmdb_id: Some(999),
                    imdb_id: None,
                    kinopoisk_url: None,
                    world_art_url: None,
                    poster_url: Some("https://image.tmdb.org/t/p/w300/test.jpg".to_string()),
                    overview: None,
                    anilist_id: None,
                    status: "tracking".to_string(),
                    created_at: String::new(),
                    updated_at: String::new(),
                },
            )
            .unwrap();
        })
        .await
        .unwrap();

        let app = web::build_router(state);
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Test Movie"));
        assert!(body_str.contains("2024"));
        assert!(body_str.contains("tracking"));
    }

    #[tokio::test]
    async fn test_dashboard_shows_tracking_seasons() {
        let state = build_test_state();

        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().unwrap();
            let media_id = queries::insert_media(
                &conn,
                &crate::db::models::Media {
                    id: 0,
                    media_type: "series".to_string(),
                    title: "Test Series".to_string(),
                    title_original: None,
                    year: Some(2024),
                    tmdb_id: Some(888),
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

            queries::insert_season(
                &conn,
                &crate::db::models::Season {
                    id: 0,
                    media_id,
                    season_number: 1,
                    title: None,
                    episode_count: None,
                    anilist_id: None,
                    format: None,
                    status: "tracking".to_string(),
                    created_at: String::new(),
                },
            )
            .unwrap();

            queries::insert_season(
                &conn,
                &crate::db::models::Season {
                    id: 0,
                    media_id,
                    season_number: 2,
                    title: None,
                    episode_count: None,
                    anilist_id: None,
                    format: None,
                    status: "ignored".to_string(),
                    created_at: String::new(),
                },
            )
            .unwrap();

            queries::insert_season(
                &conn,
                &crate::db::models::Season {
                    id: 0,
                    media_id,
                    season_number: 3,
                    title: None,
                    episode_count: None,
                    anilist_id: None,
                    format: None,
                    status: "tracking".to_string(),
                    created_at: String::new(),
                },
            )
            .unwrap();
        })
        .await
        .unwrap();

        let app = web::build_router(state);
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Test Series"));
        // Seasons 1 and 3 are tracking, season 2 is ignored
        assert!(body_str.contains("S1:"));
        assert!(body_str.contains("S3:"));
        assert!(!body_str.contains("S2:"));
    }
}
