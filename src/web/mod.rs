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

/// Extract a season marker from a torrent title (e.g. "Сезон 1-4", "Season 2", "S01-S05").
/// Looks anywhere in the title, inside or outside brackets/parens.
fn extract_season_tag(value: &str) -> Option<String> {
    use regex::Regex;
    use std::sync::LazyLock;

    static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
        vec![
            // "Сезон 1" / "Сезоны 1-5" / "Сезон: 1"
            Regex::new(r"(?i)сезон[ы]?\s*:?\s*(\d+(?:\s*[-–—]\s*\d+)?)").unwrap(),
            // "1 сезон" / "1-5 сезонов" / "1-й сезон"
            Regex::new(r"(?i)(\d+(?:\s*[-–—]\s*\d+)?)(?:-?[йая])?\s*сезон").unwrap(),
            // "Season 1" / "Season 1-5"
            Regex::new(r"(?i)season\s*(\d+(?:\s*[-–—]\s*\d+)?)").unwrap(),
            // "S01" / "S01-S05" / "S1-S5"
            Regex::new(r"\bS(\d{1,2})(?:\s*[-–—]\s*S?(\d{1,2}))?\b").unwrap(),
            // "ТВ-1" / "TV-1" / "ТВ-2" (anime season marker)
            Regex::new(r"(?i)(?:ТВ|TV)[-\s]?(\d+)").unwrap(),
            // "сезон первый" / "первый сезон" / "сезон второй" — Russian ordinal words
            Regex::new(
                r"(?i)сезон\s+(перв|втор|трет|четв[её]рт|пят|шест|седьм|восьм|девят|десят)\w*",
            )
            .unwrap(),
            Regex::new(
                r"(?i)(перв|втор|трет|четв[её]рт|пят|шест|седьм|восьм|девят|десят)\w*\s+сезон",
            )
            .unwrap(),
        ]
    });

    fn ordinal_word_to_number(word: &str) -> Option<i64> {
        let w = word.to_lowercase();
        let w = w.replace('ё', "е");
        match w.as_str() {
            "перв" => Some(1),
            "втор" => Some(2),
            "трет" => Some(3),
            "четверт" => Some(4),
            "пят" => Some(5),
            "шест" => Some(6),
            "седьм" => Some(7),
            "восьм" => Some(8),
            "девят" => Some(9),
            "десят" => Some(10),
            _ => None,
        }
    }

    for (idx, re) in PATTERNS.iter().enumerate() {
        if let Some(caps) = re.captures(value) {
            // Ordinal word patterns (idx 5, 6)
            if idx >= 5 {
                if let Some(num) = ordinal_word_to_number(&caps[1]) {
                    return Some(format!("Сезон {num}"));
                }
                continue;
            }
            let nums = caps[1].to_string();
            // Normalize separators and spaces
            let nums = nums.replace(['–', '—'], "-");
            let nums: String = nums.chars().filter(|c| !c.is_whitespace()).collect();
            // For "S01" pattern (idx 3), format as S-prefixed
            if idx == 3 {
                let end = caps.get(2).map(|m| m.as_str());
                return Some(match end {
                    Some(e) => format!("S{nums}-S{e}"),
                    None => format!("S{nums}"),
                });
            }
            // For "TV-N" pattern (idx 4), format as TV-N
            if idx == 4 {
                return Some(format!("TV-{nums}"));
            }
            return Some(format!("Сезон {nums}"));
        }
    }
    None
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
    // Prepend a season tag if one is found in the title and not already a tag
    if let Some(season_tag) = extract_season_tag(&value)
        && !tags.iter().any(|t| t.eq_ignore_ascii_case(&season_tag))
    {
        tags.insert(0, season_tag);
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
    fn test_extract_season_tag_russian() {
        assert_eq!(
            extract_season_tag("Sherlock / Шерлок (Сезон 1-4)"),
            Some("Сезон 1-4".to_string())
        );
        assert_eq!(
            extract_season_tag("Стрела / Arrow [Сезон 1 из 8]"),
            Some("Сезон 1".to_string())
        );
        assert_eq!(
            extract_season_tag("Друзья / Friends (1-10 сезоны)"),
            Some("Сезон 1-10".to_string())
        );
        assert_eq!(
            extract_season_tag("Игра престолов / 2 сезон"),
            Some("Сезон 2".to_string())
        );
    }

    #[test]
    fn test_extract_season_tag_english() {
        assert_eq!(
            extract_season_tag("Breaking Bad Season 3"),
            Some("Сезон 3".to_string())
        );
        assert_eq!(
            extract_season_tag("Show S01-S05 complete"),
            Some("S01-S05".to_string())
        );
        assert_eq!(
            extract_season_tag("Show S02 [1080p]"),
            Some("S02".to_string())
        );
    }

    #[test]
    fn test_extract_season_tag_ordinal_words() {
        assert_eq!(
            extract_season_tag("Судьба: Ночь Схватки (сезон первый) / Fate-Stay Night"),
            Some("Сезон 1".to_string())
        );
        assert_eq!(
            extract_season_tag("Title (сезон второй)"),
            Some("Сезон 2".to_string())
        );
        assert_eq!(
            extract_season_tag("Title (четвёртый сезон)"),
            Some("Сезон 4".to_string())
        );
        assert_eq!(
            extract_season_tag("Title (третий сезон)"),
            Some("Сезон 3".to_string())
        );
    }

    #[test]
    fn test_extract_season_tag_anime() {
        assert_eq!(
            extract_season_tag("Судьба: Ночь схватки (ТВ-1) / Fate Stay Night [1080p]"),
            Some("TV-1".to_string())
        );
        assert_eq!(
            extract_season_tag("Title / Fate Unlimited Blade Works [TV-2]"),
            Some("TV-2".to_string())
        );
        // [TV+Special] should NOT match (no digit after TV)
        assert_eq!(extract_season_tag("Title [TV+Special] [1080p]"), None);
    }

    #[test]
    fn test_extract_season_tag_none() {
        assert_eq!(extract_season_tag("Title without season"), None);
        assert_eq!(extract_season_tag("Some movie (2023)"), None);
    }

    #[test]
    fn test_extract_tags_filter_prepends_season() {
        let tags = extract_tags_filter("Sherlock / Шерлок (Сезон 1-4) [1080p]".to_string());
        assert_eq!(tags[0], "Сезон 1-4");
        assert!(tags.contains(&"1080p".to_string()));
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
