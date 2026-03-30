use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::response::Html;
use axum::routing::get;

use crate::error::AppError;
use crate::web::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/settings", get(get_settings))
}

async fn get_settings(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError> {
    let config = &state.config;

    let tmpl = state.templates.get_template("settings.html")?;
    let html = tmpl.render(minijinja::context! {
        rutracker_url => config.rutracker.url,
        rutracker_username => config.rutracker.username,
        qbittorrent_url => config.qbittorrent.url,
        qbittorrent_username => config.qbittorrent.username,
        tmdb_api_key => "***configured***",
        plex_url => config.plex.url,
        plex_token => "***configured***",
        telegram_bot_token => "***configured***",
        telegram_chat_id => config.telegram.chat_id,
        download_dir => config.paths.download_dir,
        movies_dir => config.paths.movies_dir,
        tv_dir => config.paths.tv_dir,
        server_host => config.server.host,
        server_port => config.server.port,
    })?;
    Ok(Html(html))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::db;
    use crate::web;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_config() -> Config {
        let toml_str = r#"
[rutracker]
url = "http://127.0.0.1:19999"
username = "testuser"
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
    async fn test_settings_returns_200() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_settings_contains_config_values() {
        let state = build_test_state();
        let app = web::build_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/settings")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("127.0.0.1"));
        assert!(body_str.contains("testuser"));
        assert!(body_str.contains("***configured***"));
        // Path values are rendered in input value attributes
        assert!(body_str.contains("movies"));
    }
}
