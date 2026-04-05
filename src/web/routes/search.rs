use std::sync::Arc;

use axum::Router;
use axum::extract::{Form, State};
use axum::response::Html;
use axum::routing::get;
use serde::{Deserialize, Serialize};

use crate::anilist::models::{AniListMedia, AniListSearchItem};
use crate::error::AppError;
use crate::tmdb::models::TmdbSearchItem;
use crate::web::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/search", get(search_page).post(search_results))
}

#[derive(Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    30
}

/// Unified search result for display, merging TMDB and AniList sources.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SearchItem {
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<String>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub source: String,
    pub media_type: String,
    pub tmdb_id: Option<i64>,
    pub anilist_id: Option<i64>,
    pub episodes: Option<i64>,
    pub format: Option<String>,
}

impl SearchItem {
    pub(crate) fn from_tmdb(item: &TmdbSearchItem) -> Self {
        Self {
            title: item.title.clone(),
            original_title: item.original_title.clone(),
            year: item.year.clone(),
            overview: item.overview.clone(),
            poster_url: item.poster_url.clone(),
            source: "tmdb".to_string(),
            media_type: item.media_type.clone(),
            tmdb_id: Some(item.tmdb_id),
            anilist_id: None,
            episodes: None,
            format: None,
        }
    }

    pub(crate) fn from_anilist(item: &AniListSearchItem) -> Self {
        Self {
            title: item.title.clone(),
            original_title: item.original_title.clone(),
            year: item.year.clone(),
            overview: item.overview.clone(),
            poster_url: item.poster_url.clone(),
            source: "anilist".to_string(),
            media_type: "anime".to_string(),
            tmdb_id: None,
            anilist_id: Some(item.anilist_id),
            episodes: item.episodes,
            format: item.format.clone(),
        }
    }
}

/// Interleave AniList and TMDB items (one-from-each alternating).
pub(crate) fn interleave_results(
    anilist_items: Vec<SearchItem>,
    tmdb_items: Vec<SearchItem>,
) -> Vec<SearchItem> {
    let mut results = Vec::new();
    let mut ai = anilist_items.into_iter();
    let mut ti = tmdb_items.into_iter();
    loop {
        let a = ai.next();
        let t = ti.next();
        if a.is_none() && t.is_none() {
            break;
        }
        if let Some(item) = a {
            results.push(item);
        }
        if let Some(item) = t {
            results.push(item);
        }
    }
    results
}

async fn search_page(
    State(state): State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<SearchQuery>,
) -> Result<Html<String>, AppError> {
    let query = params.q.trim().to_string();
    let tmpl = state.templates.get_template("search.html")?;
    let html = tmpl.render(minijinja::context! { query => query })?;
    Ok(Html(html))
}

async fn search_results(
    State(state): State<Arc<AppState>>,
    Form(params): Form<SearchQuery>,
) -> Result<Html<String>, AppError> {
    let query = params.q.trim().to_string();
    let limit = params.limit;
    if query.is_empty() {
        let tmpl = state.templates.get_template("search_results.html")?;
        let html = tmpl.render(minijinja::context! {
            results => Vec::<SearchItem>::new(),
            query => ""
        })?;
        return Ok(Html(html));
    }

    let mut errors: Vec<String> = Vec::new();

    let tmdb_movies = match state.tmdb.search_movie(&query).await {
        Ok(results) => results,
        Err(error) => {
            tracing::warn!(?error, "tmdb movie search failed");
            errors.push(format!("TMDB search failed: {error}"));
            Vec::new()
        }
    };
    let tmdb_tv = match state.tmdb.search_tv(&query).await {
        Ok(results) => results,
        Err(error) => {
            if !errors.iter().any(|err| err.starts_with("TMDB")) {
                tracing::warn!(?error, "tmdb tv search failed");
                errors.push(format!("TMDB search failed: {error}"));
            }
            Vec::new()
        }
    };
    let anilist_raw: Vec<AniListMedia> = match state.anilist.search(&query).await {
        Ok(results) => results.into_iter().take(limit).collect(),
        Err(error) => {
            tracing::warn!(?error, "anilist search failed");
            errors.push(format!("AniList search failed: {error}"));
            Vec::new()
        }
    };

    tracing::info!(
        query,
        tmdb_movies = tmdb_movies.len(),
        tmdb_tv = tmdb_tv.len(),
        anilist = anilist_raw.len(),
        "search results fetched"
    );

    let anilist_results: Vec<AniListSearchItem> = anilist_raw
        .iter()
        .map(AniListSearchItem::from_media)
        .collect();

    // Interleave AniList and TMDB results (1 from each source, alternating)
    let mut tmdb_items: Vec<SearchItem> = Vec::new();
    for m in tmdb_movies.iter().take(limit) {
        tmdb_items.push(SearchItem::from_tmdb(&TmdbSearchItem::from_movie(m)));
    }
    for t in tmdb_tv.iter().take(limit) {
        tmdb_items.push(SearchItem::from_tmdb(&TmdbSearchItem::from_tv(t)));
    }
    let anilist_items: Vec<SearchItem> = anilist_results
        .iter()
        .map(SearchItem::from_anilist)
        .collect();

    let results = interleave_results(anilist_items, tmdb_items);

    let tmpl = state.templates.get_template("search_results.html")?;
    let html = tmpl.render(minijinja::context! {
        results => results,
        errors => errors,
        query => query
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
    async fn test_search_page_returns_200() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_search_page_contains_form() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/search")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("hx-post"));
        assert!(body_str.contains("search"));
    }

    #[tokio::test]
    async fn test_search_with_empty_query_returns_200() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/search")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("q="))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_search_results_partial_contains_sections() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/search")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("q="))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("search-results"));
    }

    #[tokio::test]
    async fn test_search_results_no_rutracker_section() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/search")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("q="))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(!body_str.contains("RuTracker Results"));
    }
}
