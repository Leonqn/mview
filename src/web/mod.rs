pub mod routes;

use std::sync::Arc;

use axum::Router;
use minijinja::Environment;
use teloxide::Bot;
use tower_http::services::ServeDir;

use crate::anilist::client::AniListClient;
use crate::config::Config;
use crate::db::DbPool;
use crate::qbittorrent::client::QbtClient;
use crate::rutracker::auth::AuthHandle;
use crate::rutracker::client::RutrackerClient;
use crate::tmdb::client::TmdbClient;

pub struct AppState {
    pub db: DbPool,
    pub config: Config,
    pub templates: Environment<'static>,
    pub rutracker: RutrackerClient,
    pub tmdb: TmdbClient,
    pub anilist: AniListClient,
    pub qbittorrent: tokio::sync::Mutex<QbtClient>,
    pub auth_handle: AuthHandle,
    pub telegram_bot: Bot,
    pub telegram_chat_id: i64,
}

pub fn init_templates() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_loader(minijinja::path_loader("templates"));
    env.add_filter("truncate", truncate_filter);
    env.add_filter("extract_tags", extract_tags_filter);
    env
}

fn truncate_filter(value: String, length: usize) -> String {
    if value.chars().count() <= length {
        return value;
    }
    let truncated: String = value.chars().take(length).collect();
    format!("{truncated}...")
}

fn extract_tags_filter(value: String) -> Vec<String> {
    let mut tags = Vec::new();
    let mut rest = value.as_str();
    while let Some(start) = rest.find('[') {
        if let Some(end) = rest[start..].find(']') {
            let tag = &rest[start + 1..start + end];
            if !tag.is_empty() {
                tags.push(tag.to_string());
            }
            rest = &rest[start + end + 1..];
        } else {
            break;
        }
    }
    tags
}

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .merge(routes::dashboard::routes())
        .merge(routes::search::routes())
        .merge(routes::api::routes())
        .merge(routes::series::routes())
        .merge(routes::settings::routes())
        .merge(routes::captcha::routes())
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tags_filter_basic() {
        let tags = extract_tags_filter("Title [WEB-DL 1080p] [AniLibria]".to_string());
        assert_eq!(tags, vec!["WEB-DL 1080p", "AniLibria"]);
    }

    #[test]
    fn test_extract_tags_filter_no_tags() {
        let tags = extract_tags_filter("Title without tags".to_string());
        assert!(tags.is_empty());
    }

    #[test]
    fn test_extract_tags_filter_empty_brackets() {
        let tags = extract_tags_filter("Title [] [valid]".to_string());
        assert_eq!(tags, vec!["valid"]);
    }

    #[test]
    fn test_extract_tags_filter_nested() {
        let tags = extract_tags_filter("[HDRip] Some [Rus] title [1080p]".to_string());
        assert_eq!(tags, vec!["HDRip", "Rus", "1080p"]);
    }

    #[test]
    fn test_extract_tags_filter_unclosed() {
        let tags = extract_tags_filter("[good] title [unclosed".to_string());
        assert_eq!(tags, vec!["good"]);
    }
}
