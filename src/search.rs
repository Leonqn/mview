use regex::Regex;
use std::sync::LazyLock;

use crate::db::models::{Episode, Media, Season};

/// Search queries in tiers of precision.
/// - `primary`: most precise (narrow, with year if available)
/// - `fallback`: narrow without year
/// - `broad_fallback`: title-only, broad (for manual search)
pub struct SearchQueries {
    pub primary: Vec<String>,
    pub fallback: Vec<String>,
    pub broad_fallback: Vec<String>,
}

/// Year the season first aired, derived from its earliest episode air date.
pub fn season_air_year(episodes: &[Episode]) -> Option<i64> {
    episodes
        .iter()
        .filter_map(|e| e.air_date.as_deref())
        .filter(|d| d.len() >= 4)
        .min()
        .and_then(|d| d[..4].parse().ok())
}

/// Build search queries for a season based on media type.
///
/// `tv_season_number` is the TV-only season index (skipping movies/OVA formats).
/// `season_year` is the year the season aired (from episode air dates); later
/// seasons air years after `media.year`, so primary queries carry both years.
pub fn build_queries(
    media: &Media,
    season: &Season,
    tv_season_number: i64,
    season_year: Option<i64>,
) -> SearchQueries {
    let mut years: Vec<i64> = season_year.into_iter().chain(media.year).collect();
    years.dedup();

    let search_name = media
        .title_original
        .as_deref()
        .filter(|t| t.is_ascii())
        .unwrap_or(&media.title);

    let season_name = season.title.as_deref().unwrap_or(search_name);
    let fmt = season.format.as_deref().unwrap_or("");

    // If any year is known: primary = with each year (season year first),
    // fallback = without year. If no year: primary = without year, fallback = empty.
    let layer = |narrow: Vec<String>| -> (Vec<String>, Vec<String>) {
        if years.is_empty() {
            (narrow, vec![])
        } else {
            let with_year: Vec<String> = years
                .iter()
                .flat_map(|y| narrow.iter().map(move |q| format!("{} {}", q, y)))
                .collect();
            (with_year, narrow)
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

    fn test_media(year: Option<i64>) -> Media {
        Media {
            id: 1,
            media_type: "series".to_string(),
            title: "Гримм".to_string(),
            title_original: Some("Grimm".to_string()),
            year,
            tmdb_id: None,
            imdb_id: None,
            kinopoisk_url: None,
            world_art_url: None,
            poster_url: None,
            overview: None,
            anilist_id: None,
            status: "tracking".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn test_season(number: i64) -> Season {
        Season {
            id: 1,
            media_id: 1,
            season_number: number,
            title: None,
            episode_count: None,
            anilist_id: None,
            format: None,
            status: "tracking".to_string(),
            created_at: String::new(),
        }
    }

    #[test]
    fn test_series_queries_use_season_year_and_media_year() {
        let media = test_media(Some(2011));
        let season = test_season(2);

        let sq = build_queries(&media, &season, 2, Some(2012));

        // Season year variants come first, then series start year.
        assert_eq!(sq.primary[0], "Grimm Season 2 2012");
        assert!(sq.primary.contains(&"Grimm Сезон 2 2012".to_string()));
        assert!(sq.primary.contains(&"Grimm Season 2 2011".to_string()));
        assert!(sq.fallback.contains(&"Grimm Сезон 2".to_string()));
    }

    #[test]
    fn test_series_queries_dedup_equal_years() {
        let media = test_media(Some(2011));
        let season = test_season(1);

        let sq = build_queries(&media, &season, 1, Some(2011));

        assert_eq!(
            sq.primary
                .iter()
                .filter(|q| *q == "Grimm Season 1 2011")
                .count(),
            1
        );
    }

    #[test]
    fn test_series_queries_no_year() {
        let media = test_media(None);
        let season = test_season(1);

        let sq = build_queries(&media, &season, 1, None);

        assert_eq!(sq.primary[0], "Grimm Season 1");
        assert!(sq.fallback.is_empty());
    }

    #[test]
    fn test_season_air_year() {
        let ep = |air: Option<&str>| Episode {
            id: 1,
            season_id: 1,
            episode_number: 1,
            title: None,
            air_date: air.map(String::from),
            downloaded: false,
            file_path: None,
        };
        assert_eq!(
            season_air_year(&[ep(Some("2012-08-13")), ep(Some("2012-08-20"))]),
            Some(2012)
        );
        assert_eq!(season_air_year(&[ep(None), ep(Some(""))]), None);
        assert_eq!(season_air_year(&[]), None);
    }

    #[test]
    fn test_is_generic_season_name() {
        assert!(is_generic_season_name("Season 1"));
        assert!(is_generic_season_name("Сезон 2"));
        assert!(is_generic_season_name("Specials"));
        assert!(!is_generic_season_name("Unlimited Blade Works"));
    }
}
