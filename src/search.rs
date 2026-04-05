use regex::Regex;
use std::sync::LazyLock;

use crate::db::models::{Media, Season};

/// Search queries in tiers of precision.
/// - `primary`: most precise (narrow, with year if available)
/// - `fallback`: narrow without year
/// - `broad_fallback`: title-only, broad (for manual search)
pub struct SearchQueries {
    pub primary: Vec<String>,
    pub fallback: Vec<String>,
    pub broad_fallback: Vec<String>,
}

/// Build search queries for a season based on media type.
///
/// `tv_season_number` is the TV-only season index (skipping movies/OVA formats).
pub fn build_queries(media: &Media, season: &Season, tv_season_number: i64) -> SearchQueries {
    let year = media.year;

    let search_name = media
        .title_original
        .as_deref()
        .filter(|t| t.is_ascii())
        .unwrap_or(&media.title);

    let season_name = season.title.as_deref().unwrap_or(search_name);
    let fmt = season.format.as_deref().unwrap_or("");

    // If year is known: primary = with year, fallback = without year.
    // If no year: primary = without year, fallback = empty.
    let layer = |narrow: Vec<String>| -> (Vec<String>, Vec<String>) {
        if let Some(y) = year {
            let with_year: Vec<String> = narrow.iter().map(|q| format!("{} {}", q, y)).collect();
            (with_year, narrow)
        } else {
            (narrow, vec![])
        }
    };

    if media.media_type == "movie" || fmt == "MOVIE" || fmt == "OVA" || fmt == "SPECIAL" {
        let (primary, fallback) = layer(vec![season_name.to_string()]);
        SearchQueries {
            primary,
            fallback,
            broad_fallback: vec![],
        }
    } else if media.media_type == "anime" && season.anilist_id.is_some() {
        let (base_title, season_num) = parse_anime_season_title(season_name);
        let narrow = vec![
            format!("{} TV-{}", base_title, season_num),
            format!("{} ТВ-{}", base_title, season_num),
        ];
        let (primary, fallback) = layer(narrow);
        let mut broad = Vec::new();
        if base_title != season_name {
            broad.push(season_name.to_string());
        }
        broad.push(base_title.to_string());
        SearchQueries {
            primary,
            fallback,
            broad_fallback: broad,
        }
    } else if media.media_type == "anime" {
        let narrow = vec![
            format!("{} TV-{}", search_name, tv_season_number),
            format!("{} ТВ-{}", search_name, tv_season_number),
        ];
        let (primary, fallback) = layer(narrow);
        let mut broad = Vec::new();
        if !is_generic_season_name(season_name) && season_name != search_name {
            broad.push(season_name.to_string());
        }
        broad.push(search_name.to_string());
        SearchQueries {
            primary,
            fallback,
            broad_fallback: broad,
        }
    } else {
        let mut narrow = vec![
            format!("{} Season {}", search_name, season.season_number),
            format!("{} Сезон {}", search_name, season.season_number),
            format!("{} TV-{}", search_name, season.season_number),
            format!("{} ТВ-{}", search_name, season.season_number),
        ];
        if season_name != search_name && !is_generic_season_name(season_name) {
            narrow.push(season_name.to_string());
        }
        let (primary, fallback) = layer(narrow);
        SearchQueries {
            primary,
            fallback,
            broad_fallback: vec![search_name.to_string()],
        }
    }
}

fn is_generic_season_name(name: &str) -> bool {
    static RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(season|сезон|specials?)\s*\d*$").unwrap());
    RE.is_match(name.trim())
}

/// Parse an anime season title to extract the base title and season number.
///
/// Examples:
/// - "Title 2nd Season" → ("Title", 2)
/// - "Title Season 2" → ("Title", 2)
/// - "Title Part 2" → ("Title", 2)
/// - "Title" (no suffix) → ("Title", 1)
pub fn parse_anime_season_title(title: &str) -> (&str, i64) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_anime_season_title() {
        assert_eq!(
            parse_anime_season_title("Fate/Zero 2nd Season"),
            ("Fate/Zero", 2)
        );
        assert_eq!(parse_anime_season_title("Title Season 3"), ("Title", 3));
        assert_eq!(parse_anime_season_title("Title Part 2"), ("Title", 2));
        assert_eq!(parse_anime_season_title("Title"), ("Title", 1));
    }

    #[test]
    fn test_is_generic_season_name() {
        assert!(is_generic_season_name("Season 1"));
        assert!(is_generic_season_name("Сезон 2"));
        assert!(is_generic_season_name("Specials"));
        assert!(!is_generic_season_name("Unlimited Blade Works"));
    }
}
