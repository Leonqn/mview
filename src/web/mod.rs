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
    let mut raw_tags = Vec::new();
    let mut rest = value.as_str();
    let mut found_bracket = false;
    while let Some(start) = rest.find('[') {
        if let Some(end) = rest[start..].find(']') {
            let tag = &rest[start + 1..start + end];
            if !tag.is_empty() {
                raw_tags.push(tag.to_string());
            }
            rest = &rest[start + end + 1..];
            found_bracket = true;
        } else {
            break;
        }
    }
    // Parse audio/subtitle info after the last ']'
    if found_bracket {
        let trailing = rest.trim();
        if !trailing.is_empty() && !trailing.contains('[') {
            for part in trailing.split('+') {
                let part = part.trim().trim_matches(',').trim();
                if !part.is_empty() {
                    raw_tags.push(part.to_string());
                }
            }
        }
    }
    // Split long comma-separated tags into individual tags,
    // but keep short ones intact (e.g. "RUS(ext), JAP+Sub")
    let mut tags = Vec::new();
    for tag in raw_tags {
        if tag.len() > 40 && tag.contains(", ") {
            for part in tag.split(", ") {
                let part = part.trim();
                if !part.is_empty() {
                    tags.push(part.to_string());
                }
            }
        } else {
            tags.push(tag);
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

    #[test]
    fn test_extract_tags_filter_trailing_audio() {
        let tags =
            extract_tags_filter("Title [2025, драма] [1080p] Dub + MVO + Sub Rus, Eng".to_string());
        assert_eq!(
            tags,
            vec!["2025, драма", "1080p", "Dub", "MVO", "Sub Rus, Eng"]
        );
    }

    #[test]
    fn test_extract_tags_filter_anime_audio() {
        let tags = extract_tags_filter(
            "Title [TV] [12 из 12] [RUS(ext), JAP+Sub] [2026, WEB-DL] [1080p]".to_string(),
        );
        assert_eq!(
            tags,
            vec![
                "TV",
                "12 из 12",
                "RUS(ext), JAP+Sub",
                "2026, WEB-DL",
                "1080p"
            ]
        );
    }

    #[test]
    fn test_extract_tags_filter_splits_long_tags() {
        let tags = extract_tags_filter(
            "Title [2025, США, Южная Корея, Канада, Ирландия, драма, комедия, фантастика, UHD BDRemux 2160p, HDR10] [HYBRID]".to_string(),
        );
        assert!(tags.contains(&"2025".to_string()));
        assert!(tags.contains(&"HDR10".to_string()));
        assert!(tags.contains(&"HYBRID".to_string()));
    }
}
