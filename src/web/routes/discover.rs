use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::get;
use serde::Deserialize;

use crate::anilist::client::DiscoverCategory;
use crate::anilist::models::AniListSearchItem;
use crate::error::AppError;
use crate::tmdb::models::TmdbSearchItem;
use crate::web::AppState;
use crate::web::routes::search::{SearchItem, interleave_results};

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/discover", get(discover_page))
        .route("/discover/results", get(discover_results))
}

#[derive(Deserialize, Default)]
pub struct DiscoverParams {
    #[serde(default)]
    pub category: String,
}

fn parse_category(s: &str) -> DiscoverCategory {
    match s {
        "popular" => DiscoverCategory::Popular,
        "top_rated" => DiscoverCategory::TopRated,
        "airing" => DiscoverCategory::Airing,
        _ => DiscoverCategory::Trending,
    }
}

async fn discover_page(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError> {
    let tmpl = state.templates.get_template("discover.html")?;
    let html = tmpl.render(minijinja::context! {})?;
    Ok(Html(html))
}

async fn discover_results(
    State(state): State<Arc<AppState>>,
    Query(params): Query<DiscoverParams>,
) -> Result<Html<String>, AppError> {
    let category = parse_category(&params.category);
    let mut errors: Vec<String> = Vec::new();

    // For "Airing" category: movies don't "air", so skip TMDB movies.
    let skip_movies = matches!(category, DiscoverCategory::Airing);

    let (tmdb_movies_res, tmdb_tv_res, anilist_res) = tokio::join!(
        async {
            if skip_movies {
                Ok(Vec::new())
            } else {
                match category {
                    DiscoverCategory::Trending => state.tmdb.trending_movies().await,
                    DiscoverCategory::Popular => state.tmdb.popular_movies().await,
                    DiscoverCategory::TopRated => state.tmdb.top_rated_movies().await,
                    DiscoverCategory::Airing => Ok(Vec::new()),
                }
            }
        },
        async {
            match category {
                DiscoverCategory::Trending => state.tmdb.trending_tv().await,
                DiscoverCategory::Popular => state.tmdb.popular_tv().await,
                DiscoverCategory::TopRated => state.tmdb.top_rated_tv().await,
                DiscoverCategory::Airing => state.tmdb.on_air_tv().await,
            }
        },
        state.anilist.discover(category)
    );

    let tmdb_movies = tmdb_movies_res.unwrap_or_else(|error| {
        tracing::warn!(?error, "tmdb discover movies failed");
        errors.push(format!("TMDB movies failed: {error}"));
        Vec::new()
    });
    let tmdb_tv = tmdb_tv_res.unwrap_or_else(|error| {
        tracing::warn!(?error, "tmdb discover tv failed");
        errors.push(format!("TMDB tv failed: {error}"));
        Vec::new()
    });
    let anilist_raw = anilist_res.unwrap_or_else(|error| {
        tracing::warn!(?error, "anilist discover failed");
        errors.push(format!("AniList failed: {error}"));
        Vec::new()
    });

    tracing::info!(
        category = params.category,
        tmdb_movies = tmdb_movies.len(),
        tmdb_tv = tmdb_tv.len(),
        anilist = anilist_raw.len(),
        "discover results fetched"
    );

    let mut tmdb_items: Vec<SearchItem> = Vec::new();
    for m in tmdb_movies.iter() {
        tmdb_items.push(SearchItem::from_tmdb(&TmdbSearchItem::from_movie(m)));
    }
    for t in tmdb_tv.iter() {
        tmdb_items.push(SearchItem::from_tmdb(&TmdbSearchItem::from_tv(t)));
    }
    let anilist_items: Vec<SearchItem> = anilist_raw
        .iter()
        .map(AniListSearchItem::from_media)
        .map(|i| SearchItem::from_anilist(&i))
        .collect();

    let results = interleave_results(anilist_items, tmdb_items);

    let tmpl = state.templates.get_template("search_results.html")?;
    let html = tmpl.render(minijinja::context! {
        results => results,
        errors => errors,
        query => "",
    })?;
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

    #[test]
    fn test_parse_category() {
        assert!(matches!(
            parse_category("trending"),
            DiscoverCategory::Trending
        ));
        assert!(matches!(
            parse_category("popular"),
            DiscoverCategory::Popular
        ));
        assert!(matches!(
            parse_category("top_rated"),
            DiscoverCategory::TopRated
        ));
        assert!(matches!(parse_category("airing"), DiscoverCategory::Airing));
        // Unknown defaults to Trending
        assert!(matches!(parse_category(""), DiscoverCategory::Trending));
        assert!(matches!(parse_category("xyz"), DiscoverCategory::Trending));
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
    async fn test_discover_page_returns_200() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/discover")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_discover_page_contains_tabs() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/discover")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Trending"));
        assert!(body_str.contains("Popular"));
        assert!(body_str.contains("Top Rated"));
        assert!(body_str.contains("Airing"));
        assert!(body_str.contains("category=trending"));
    }
}
