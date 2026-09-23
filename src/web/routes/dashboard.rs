use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;
use serde::Serialize;

use chrono::{Datelike, Local, NaiveDate};

use crate::db::models::Media;
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
    /// Earliest future episode air_date for this season (YYYY-MM-DD), if known.
    next_air_date: Option<String>,
    /// Season is fully downloaded (every aired ep is on disk).
    complete: bool,
    /// Some episodes aired, more are still to come (dated or TBA).
    in_progress: bool,
    /// Nothing has aired yet: an announced future season / unreleased film.
    announced: bool,
    /// Aired episodes are missing and no torrent covers the season.
    missing: bool,
}

#[derive(Debug, Serialize)]
struct MediaDashboardItem {
    #[serde(flatten)]
    media: Media,
    /// Seasons rendered as chips: tracking + completed (ignored seasons are hidden).
    visible_seasons: Vec<SeasonDashboardInfo>,
    has_pending: bool,
    /// Dashboard section key, see [`GROUPS`].
    group: &'static str,
    /// One-line human summary of why the item sits in its group.
    status_line: String,
    /// Earliest future air date across visible seasons.
    next_date: Option<String>,
    /// `next_date` is within the next 30 days.
    soon: bool,
}

#[derive(Debug, Serialize)]
struct DashboardGroup {
    key: &'static str,
    label: &'static str,
    hint: &'static str,
    items: Vec<MediaDashboardItem>,
}

/// Dashboard sections in display order: (key, label, hint).
const GROUPS: [(&str, &str, &str); 7] = [
    (
        "downloading",
        "Downloading",
        "qBittorrent is pulling these right now",
    ),
    (
        "found",
        "Found on rutracker",
        "a torrent turned up — pick one to download",
    ),
    (
        "airing",
        "Airing now",
        "waiting for new episodes of the current season",
    ),
    (
        "missing",
        "Not found yet",
        "aired episodes are missing, nothing on rutracker so far",
    ),
    ("upcoming", "Upcoming", "a new season or film is announced"),
    (
        "waiting",
        "Waiting for a new season",
        "everything downloaded, the show is still running",
    ),
    (
        "complete",
        "Complete",
        "everything downloaded, nothing more expected",
    ),
];

const SOON_DAYS: i64 = 30;

/// "in 3 days" / "in 2 months" style relative label for a future date.
fn relative_days(days: i64) -> String {
    match days {
        d if d <= 1 => "tomorrow".to_string(),
        d if d < 45 => format!("in {d} days"),
        d if d < 365 => format!("in {} months", (d + 15) / 30),
        d => format!("in {} years", (d + 182) / 365),
    }
}

async fn dashboard(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError> {
    let pool = state.db.clone();
    let groups = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let media_list = queries::get_all_media(&conn)?;
        let now = Local::now();
        let today_date = now.date_naive();
        let today = today_date.format("%Y-%m-%d").to_string();
        let current_year = now.year();
        let mut items = Vec::new();
        for media in media_list {
            // Render chips for seasons the user is following or has finished.
            // Ignored seasons (older parts of a finished show, opt-out) stay hidden.
            let seasons: Vec<_> = queries::get_seasons_for_media(&conn, media.id)?
                .into_iter()
                .filter(|s| s.status != "ignored")
                .collect();
            let torrents = queries::get_torrents_for_media(&conn, media.id)?;
            let season_ids: Vec<i64> = seasons.iter().map(|s| s.id).collect();
            let search_cache = queries::get_search_cache_for_seasons(&conn, &season_ids)?;
            let is_movie = media.media_type == "movie";
            let media_released = media
                .year
                .map(|y| y <= current_year as i64)
                .unwrap_or(false);
            let mut season_infos = Vec::new();
            for s in &seasons {
                let episodes = queries::get_episodes_for_season(&conn, s.id).unwrap_or_default();
                let aired: Vec<_> = episodes
                    .iter()
                    .filter(|e| {
                        let date = e.air_date.as_deref().filter(|d| !d.is_empty());
                        match date {
                            Some(d) => d <= today.as_str(),
                            // Empty air_date means different things by source:
                            //  - movies: per-episode air_date is just a placeholder,
                            //    use the media year.
                            //  - AniList-tracked: AniList stops exposing airingSchedule
                            //    for long-finished anime (e.g. Fate/Zero), so dateless
                            //    episodes really did air — trust media_released.
                            //  - TMDB-tracked series/anime: TMDB always populates
                            //    air_date once aired, so dateless = upcoming/TBA.
                            None => is_movie || (media.anilist_id.is_some() && media_released),
                        }
                    })
                    .collect();
                let downloaded = aired.iter().filter(|e| e.downloaded).count();
                let total = aired.len();
                // Episodes the source knows about, aired or not.
                let known_total = episodes
                    .len()
                    .max(s.episode_count.unwrap_or(0).max(0) as usize);
                let has_torrent = torrents
                    .iter()
                    .any(|t| t.season_number == Some(s.season_number));
                let downloading = torrents.iter().any(|t| {
                    t.status == "active"
                        && t.season_number == Some(s.season_number)
                        && t.qbt_hash.is_some()
                });
                let search_found = search_cache.iter().any(|c| c.season_id == s.id);
                let missing = downloaded < total && !has_torrent;
                let pending = missing && search_found;
                let next_air_date = episodes
                    .iter()
                    .filter_map(|e| e.air_date.as_deref())
                    .filter(|d| !d.is_empty() && *d > today.as_str())
                    .min()
                    .map(str::to_string);
                // Derive "complete" from actual episode download flags rather than
                // trusting season.status — for movies whose air_date filter excludes
                // them from `aired` (future release), the status can stay "completed"
                // after the user toggles tracking off/on, leading to phantom ticks.
                let complete = if is_movie {
                    !episodes.is_empty() && episodes.iter().all(|e| e.downloaded)
                } else {
                    total > 0 && downloaded == total
                };
                let in_progress = !is_movie && total > 0 && total < known_total;
                let announced = total == 0;
                season_infos.push(SeasonDashboardInfo {
                    season_number: s.season_number,
                    title: s.title.clone(),
                    downloaded,
                    total,
                    downloading,
                    pending,
                    next_air_date,
                    complete,
                    in_progress,
                    announced,
                    missing,
                });
            }

            let next_date = season_infos
                .iter()
                .filter_map(|s| s.next_air_date.clone())
                .min();
            let days_until = next_date
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .map(|d| (d - today_date).num_days());
            let soon = days_until.map(|d| d <= SOON_DAYS).unwrap_or(false);
            let ended = is_movie || media.source_status.as_deref() == Some("ended");
            let has_pending = season_infos.iter().any(|s| s.pending);
            let missing_eps: usize = season_infos
                .iter()
                .filter(|s| s.missing)
                .map(|s| s.total - s.downloaded)
                .sum();
            let when = |d: Option<&str>| match (d, days_until) {
                (Some(date), Some(days)) => format!("{date} ({})", relative_days(days)),
                (Some(date), None) => date.to_string(),
                (None, _) => "date TBA".to_string(),
            };

            let (group, status_line) = if season_infos.iter().any(|s| s.downloading) {
                ("downloading", "downloading".to_string())
            } else if has_pending {
                ("found", "torrents found on rutracker".to_string())
            } else if season_infos.iter().any(|s| s.in_progress) {
                let line = match next_date.as_deref() {
                    Some(_) => format!("airing · next episode {}", when(next_date.as_deref())),
                    None => "airing · next episode date TBA".to_string(),
                };
                ("airing", line)
            } else if missing_eps > 0 {
                let line = if is_movie {
                    "released, nothing on rutracker yet".to_string()
                } else {
                    format!("{missing_eps} episodes missing, nothing on rutracker yet")
                };
                ("missing", line)
            } else if next_date.is_some() || season_infos.iter().any(|s| s.announced) {
                let announced = season_infos
                    .iter()
                    .find(|s| s.announced)
                    .or_else(|| season_infos.iter().find(|s| s.next_air_date.is_some()));
                let what = match (is_movie, announced) {
                    (true, _) => "releases".to_string(),
                    (false, Some(s)) => format!("Season {} airs", s.season_number),
                    (false, None) => "next episode".to_string(),
                };
                ("upcoming", format!("{what} {}", when(next_date.as_deref())))
            } else if ended {
                let line = if is_movie {
                    "downloaded".to_string()
                } else {
                    "ended · all downloaded".to_string()
                };
                ("complete", line)
            } else {
                (
                    "waiting",
                    "all downloaded · waiting for a new season".to_string(),
                )
            };

            items.push(MediaDashboardItem {
                media,
                visible_seasons: season_infos,
                has_pending,
                group,
                status_line,
                next_date,
                soon,
            });
        }

        // Inside a group: things happening sooner first, then by title.
        items.sort_by(|a, b| {
            let da = a.next_date.as_deref().unwrap_or("9999");
            let db = b.next_date.as_deref().unwrap_or("9999");
            da.cmp(db).then_with(|| {
                a.media
                    .title
                    .to_lowercase()
                    .cmp(&b.media.title.to_lowercase())
            })
        });

        let mut groups: Vec<DashboardGroup> = GROUPS
            .iter()
            .map(|(key, label, hint)| DashboardGroup {
                key,
                label,
                hint,
                items: Vec::new(),
            })
            .collect();
        for item in items {
            if let Some(g) = groups.iter_mut().find(|g| g.key == item.group) {
                g.items.push(item);
            }
        }
        groups.retain(|g| !g.items.is_empty());
        Ok::<_, anyhow::Error>(groups)
    })
    .await??;

    let tmpl = state.templates.get_template("dashboard.html")?;
    let html = tmpl.render(minijinja::context! { groups => groups })?;
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
        let templates = web::init_templates("https://rutracker.org");
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
                    rating: None,
                    source_status: None,
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
                    rating: None,
                    source_status: None,
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
        // Seasons 1 and 3 are tracking, season 2 is ignored.
        // Card layout renders season chips like "S1" / "S3".
        assert!(body_str.contains("S1"));
        assert!(body_str.contains("S3"));
        assert!(!body_str.contains("S2"));
    }

    #[tokio::test]
    async fn test_dashboard_groups_by_show_status() {
        let state = build_test_state();

        let pool = state.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().unwrap();
            let mk = |title: &str, source_status: &str| crate::db::models::Media {
                id: 0,
                media_type: "series".to_string(),
                title: title.to_string(),
                title_original: None,
                year: Some(2020),
                tmdb_id: None,
                imdb_id: None,
                kinopoisk_url: None,
                world_art_url: None,
                poster_url: None,
                overview: None,
                anilist_id: None,
                status: "tracking".to_string(),
                rating: Some(8.4),
                source_status: Some(source_status.to_string()),
                created_at: String::new(),
                updated_at: String::new(),
            };
            for (title, status) in [("Ended Show", "ended"), ("Ongoing Show", "returning")] {
                let media_id = queries::insert_media(&conn, &mk(title, status)).unwrap();
                let season_id = queries::insert_season(
                    &conn,
                    &crate::db::models::Season {
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
                )
                .unwrap();
                queries::insert_episode(
                    &conn,
                    &crate::db::models::Episode {
                        id: 0,
                        season_id,
                        episode_number: 1,
                        title: None,
                        air_date: Some("2020-01-01".to_string()),
                        downloaded: true,
                        file_path: None,
                    },
                )
                .unwrap();
            }
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

        let complete_idx = body_str.find("ended · all downloaded").unwrap();
        let waiting_idx = body_str.find("waiting for a new season").unwrap();
        assert!(body_str.contains("Waiting for a new season"));
        assert!(body_str.contains(">Complete"));
        // Ended show sits in the later "Complete" section, ongoing one before it.
        assert!(waiting_idx < complete_idx);
        assert!(body_str.contains("★ 8.4"));
    }
}
