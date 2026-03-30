use std::sync::Arc;

use axum::Router;
use axum::extract::{Form, State};
use axum::http::header;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use crate::error::AppError;
use crate::web::AppState;

pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/captcha", get(captcha_page))
        .route("/api/captcha", get(captcha_partial).post(submit_captcha))
        .route("/api/captcha/image", get(captcha_image))
        .route("/api/captcha/status", get(captcha_status))
}

async fn captcha_page(State(state): State<Arc<AppState>>) -> Result<Html<String>, AppError> {
    let tmpl = state.templates.get_template("captcha.html")?;
    let html = tmpl.render(minijinja::context! {})?;
    Ok(Html(html))
}

async fn captcha_partial(State(state): State<Arc<AppState>>) -> Html<String> {
    let captcha = state.auth_handle.captcha_state.read().await;

    if captcha.is_some() {
        Html(CAPTCHA_FORM.to_string())
    } else {
        Html("<p>No captcha required at this time.</p>".to_string())
    }
}

const CAPTCHA_FORM: &str = r##"<div class="captcha-form">
    <img src="/api/captcha/image" alt="captcha" style="max-width: 300px; margin-bottom: 1rem;" />
    <form hx-post="/api/captcha" hx-target="#captcha-content" hx-swap="innerHTML">
        <fieldset role="group">
            <input type="text" name="code" placeholder="Enter captcha code" autofocus required />
            <button type="submit">Submit</button>
        </fieldset>
    </form>
</div>"##;

/// Serve the captcha image as PNG.
async fn captcha_image(State(state): State<Arc<AppState>>) -> Response {
    let captcha = state.auth_handle.captcha_state.read().await;
    if let Some(ref captcha) = *captcha {
        (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            captcha.image_data.clone(),
        )
            .into_response()
    } else {
        axum::http::StatusCode::NOT_FOUND.into_response()
    }
}

#[derive(Deserialize)]
struct CaptchaForm {
    code: String,
}

async fn submit_captcha(
    State(state): State<Arc<AppState>>,
    Form(form): Form<CaptchaForm>,
) -> Html<String> {
    let code = form.code.trim().to_string();
    if code.is_empty() {
        return Html(r#"<p class="error">Please enter the captcha code.</p>"#.to_string());
    }

    match state.auth_handle.submit_captcha(code).await {
        Ok(()) => Html("<p>Captcha solved successfully. Authentication complete.</p>".to_string()),
        Err(e) => {
            let escaped = e
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            // After failed captcha, a new one may be available — re-render the form
            let captcha = state.auth_handle.captcha_state.read().await;
            if captcha.is_some() {
                Html(format!("<p class=\"error\">{escaped}</p>\n{CAPTCHA_FORM}"))
            } else {
                Html(format!("<p class=\"error\">{escaped}</p>"))
            }
        }
    }
}

/// Lightweight status endpoint polled by the base template banner.
async fn captcha_status(State(state): State<Arc<AppState>>) -> Html<String> {
    let captcha = state.auth_handle.captcha_state.read().await;
    if captcha.is_some() {
        Html(
            r#"<article><a href="/captcha">Captcha required — click here to solve</a></article>"#
                .to_string(),
        )
    } else {
        Html(String::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn build_test_state() -> Arc<AppState> {
        use crate::config::Config;
        use crate::db;

        let toml_str = r##"
[rutracker]
url = "http://127.0.0.1:19999"
username = "user"
password = "pass"

[qbittorrent]
url = "http://127.0.0.1:19998"
username = "admin"
password = "admin"

[tmdb]
api_key = "fake"

[paths]
download_dir = "/tmp"
movies_dir = "/tmp/movies"
tv_dir = "/tmp/tv"
anime_dir = "/tmp/anime"
"##;
        let config: Config = toml::from_str(toml_str).unwrap();
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
            templates: web::init_templates(),
        })
    }

    #[tokio::test]
    async fn test_captcha_page_returns_200() {
        let state = build_test_state();
        let app = web::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/captcha")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_captcha_status_empty_when_no_captcha() {
        let state = build_test_state();
        let app = web::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/captcha/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.is_empty(), "should be empty when no captcha pending");
    }

    #[tokio::test]
    async fn test_captcha_partial_no_captcha() {
        let state = build_test_state();
        let app = web::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/captcha")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("No captcha required"));
    }

    #[tokio::test]
    async fn test_captcha_image_404_when_no_captcha() {
        let state = build_test_state();
        let app = web::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/captcha/image")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_submit_captcha_empty_code() {
        let state = build_test_state();
        let app = web::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/captcha")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from("code="))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("enter the captcha code"));
    }

    #[tokio::test]
    async fn test_captcha_status_shows_banner_when_pending() {
        let state = build_test_state();
        // Manually set captcha state
        {
            let mut captcha = state.auth_handle.captcha_state.write().await;
            *captcha = Some(crate::rutracker::auth::CaptchaForWeb {
                image_data: vec![1, 2, 3],
            });
        }
        let app = web::build_router(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/captcha/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        assert!(body_str.contains("Captcha required"));
        assert!(body_str.contains("/captcha"));
    }
}
