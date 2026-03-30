use anyhow::{Context, Result};
use scraper::{Html, Selector};
use serde::Serialize;

use super::client::RutrackerClient;

/// Detailed information parsed from a RuTracker topic page.
#[derive(Debug, Clone, Serialize, Default)]
pub struct TopicInfo {
    pub topic_id: String,
    pub title: String,
    pub quality: Option<String>,
    pub episode_info: Option<String>,
    pub registered_at: Option<String>,
    pub size_bytes: i64,
    pub seeders: i32,
    pub torrent_hash: Option<String>,
    pub kinopoisk_url: Option<String>,
    pub imdb_url: Option<String>,
    pub world_art_url: Option<String>,
}

impl RutrackerClient {
    /// Fetch and parse a topic page by its ID.
    pub async fn parse_topic(&self, topic_id: &str) -> Result<TopicInfo> {
        let url = format!("{}/forum/viewtopic.php?t={}", self.base_url(), topic_id);
        let html = self.get(&url).await.context("Failed to fetch topic page")?;
        parse_topic_html(&html, topic_id)
    }
}

/// Parse a RuTracker topic page HTML to extract detailed torrent info.
pub fn parse_topic_html(html: &str, topic_id: &str) -> Result<TopicInfo> {
    let document = Html::parse_document(html);

    let title = extract_title(&document);
    let quality = extract_quality(&title);
    let episode_info = extract_episode_info(&title);
    let registered_at = extract_registered_at(&document);
    let (size_bytes, seeders) = extract_torrent_stats(&document);
    let torrent_hash = extract_torrent_hash(&document);
    let kinopoisk_url = extract_link(&document, "kinopoisk");
    let imdb_url = extract_link(&document, "imdb.com");
    let world_art_url = extract_link(&document, "world-art.ru");

    Ok(TopicInfo {
        topic_id: topic_id.to_string(),
        title,
        quality,
        episode_info,
        registered_at,
        size_bytes,
        seeders,
        torrent_hash,
        kinopoisk_url,
        imdb_url,
        world_art_url,
    })
}

/// Extract the topic title from the page title element.
fn extract_title(document: &Html) -> String {
    let selector = Selector::parse("#topic-title").unwrap();
    document
        .select(&selector)
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .unwrap_or_default()
}

/// Extract video quality from the title string.
/// Looks for patterns like "BDRip 1080p", "WEB-DL 720p", "HDRip", etc.
/// Typical format: "Title [Year, Quality]" or "Title [Year, Source Resolution]"
fn extract_quality(title: &str) -> Option<String> {
    let source_patterns = [
        "BDRemux",
        "BDRip",
        "WEB-DL",
        "WEB-DLRip",
        "WEBRip",
        "HDRip",
        "HDTVRip",
        "DVDRip",
        "DVD5",
        "DVD9",
    ];
    let resolution_patterns = ["2160p", "1080p", "1080i", "720p", "480p", "UHD"];

    // Strategy: find bracketed segments like [2008-2013, WEB-DL 1080p] and extract quality parts
    for segment in title.split('[') {
        if let Some(end) = segment.find(']') {
            let inner = &segment[..end];
            // Split by comma and check each part for quality keywords
            for part in inner.split(',') {
                let part = part.trim();
                let has_source = source_patterns.iter().any(|p| part.contains(p));
                let has_resolution = resolution_patterns.iter().any(|p| part.contains(p));
                if has_source || has_resolution {
                    return Some(part.to_string());
                }
            }
        }
    }

    // Fallback: search anywhere in the title for source+resolution combos
    for source in &source_patterns {
        if title.contains(source) {
            // Check if a resolution follows nearby
            if let Some(pos) = title.find(source) {
                let after = &title[pos..];
                for res in &resolution_patterns {
                    if let Some(res_pos) = after.find(res)
                        && res_pos < source.len() + 10
                    {
                        let end = pos + res_pos + res.len();
                        return Some(title[pos..end].to_string());
                    }
                }
                return Some(source.to_string());
            }
        }
    }

    // Fallback: just a bare resolution
    for res in &resolution_patterns {
        if title.contains(res) {
            return Some(res.to_string());
        }
    }

    None
}

/// Extract episode info from the title.
/// Looks for patterns like "(1-12)", "серии 1-24", "(Сезон 1-5)", etc.
fn extract_episode_info(title: &str) -> Option<String> {
    // Pattern: "серии X-Y из Z" or "серии X-Y"
    let lower = title.to_lowercase();
    if let Some(pos) = lower.find("сери") {
        let after = &title[pos..];
        if let Some(info) = extract_range_from(after) {
            return Some(info);
        }
    }

    // Pattern: "(X-Y из Z)" or "(X-Y)"
    for segment in title.split('[') {
        if let Some(end) = segment.find(']') {
            let inner = &segment[..end];
            if let Some(info) = extract_range_from(inner) {
                return Some(info);
            }
        }
    }
    for segment in title.split('(') {
        if let Some(end) = segment.find(')') {
            let inner = &segment[..end];
            if let Some(info) = extract_range_from(inner) {
                return Some(info);
            }
        }
    }

    None
}

/// Try to extract a numeric range like "1-12", "1-24 из 24", or "13 из 13" from a string segment.
fn extract_range_from(text: &str) -> Option<String> {
    let mut nums = String::new();
    let mut found_dash = false;

    for ch in text.chars() {
        if ch.is_ascii_digit() {
            nums.push(ch);
        } else if ch == '-' && !nums.is_empty() {
            nums.push('-');
            found_dash = true;
        } else if ch == '+' && !nums.is_empty() {
            nums.push('+');
            found_dash = true;
        } else if found_dash && !nums.is_empty() && nums.ends_with(|c: char| c.is_ascii_digit()) {
            break;
        } else if !nums.is_empty() && !found_dash {
            // Check for "X из Y" pattern before clearing
            let lower = text.to_lowercase();
            if lower.contains(" из ") {
                let parts: Vec<&str> = lower.split(" из ").collect();
                if parts.len() == 2 {
                    if let (Some(start), Some(total)) = (
                        parts[0].split_whitespace().last().and_then(|s| s.parse::<i32>().ok()),
                        parts[1].split_whitespace().next().and_then(|s| s.parse::<i32>().ok()),
                    ) {
                        return Some(format!("1-{start} из {total}"));
                    }
                }
            }
            nums.clear();
        }
    }

    if found_dash && nums.contains(|c: char| c == '-' || c == '+') {
        let parts: Vec<&str> = nums.split(|c: char| c == '-' || c == '+').collect();
        if parts.len() == 2 && parts[0].parse::<i32>().is_ok() && parts[1].parse::<i32>().is_ok() {
            return Some(nums);
        }
    }

    None
}

/// Extract the registration date from the topic page.
/// RuTracker shows this in a specific element on the topic page.
fn extract_registered_at(document: &Html) -> Option<String> {
    // Look for the "Зарегистрирован" (registered) line in the torrent body
    let selector = Selector::parse("li.seed-distribution-date").ok()?;
    if let Some(el) = document.select(&selector).next() {
        let text = el.text().collect::<String>();
        return extract_date_from_text(&text);
    }

    // Fallback: scan all text nodes for a date near "Зарегистрирован"
    let body_selector = Selector::parse("body").ok()?;
    if let Some(body) = document.select(&body_selector).next() {
        let full_text = body.text().collect::<String>();
        let lower = full_text.to_lowercase();
        if let Some(pos) = lower.find("зарегистрирован") {
            let after = &full_text[pos..];
            if let Some(date) = extract_date_from_text(after) {
                return Some(date);
            }
        }
    }

    None
}

/// Try to extract a date string from text.
/// Looks for patterns like "2024-01-15" or "15-Янв-24".
fn extract_date_from_text(text: &str) -> Option<String> {
    // ISO date pattern: YYYY-MM-DD
    for word in text.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| !c.is_ascii_digit() && c != '-');
        if trimmed.len() == 10
            && trimmed.chars().filter(|&c| c == '-').count() == 2
            && trimmed[..4].parse::<i32>().is_ok()
        {
            return Some(trimmed.to_string());
        }
    }

    // RuTracker date pattern: "DD-Mon-YY" like "15-Янв-24"
    let months = [
        "янв", "фев", "мар", "апр", "май", "июн", "июл", "авг", "сен", "окт", "ноя", "дек",
    ];
    let lower = text.to_lowercase();
    for month in &months {
        if lower.contains(month) {
            // Found a month reference, try to extract surrounding date
            if let Some(pos) = lower.find(month) {
                let start = lower[..pos]
                    .rfind(char::is_whitespace)
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let end = lower[pos..]
                    .find(char::is_whitespace)
                    .map(|i| pos + i)
                    .unwrap_or(lower.len());
                let date_part = text[start..end].trim().to_string();
                if !date_part.is_empty() {
                    return Some(date_part);
                }
            }
        }
    }

    None
}

/// Extract torrent size and seeders from the topic stats section.
fn extract_torrent_stats(document: &Html) -> (i64, i32) {
    let size_bytes = Selector::parse("#tor-size-humn")
        .ok()
        .and_then(|sel| document.select(&sel).next())
        .and_then(|el| {
            el.value()
                .attr("data-ts_text")
                .or_else(|| el.value().attr("title"))
        })
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let seeders = Selector::parse(".seed b")
        .ok()
        .and_then(|sel| document.select(&sel).next())
        .map(|el| el.text().collect::<String>().trim().parse().unwrap_or(0))
        .unwrap_or(0);

    (size_bytes, seeders)
}

/// Extract torrent info hash from the magnet link on the page.
fn extract_torrent_hash(document: &Html) -> Option<String> {
    let selector = Selector::parse("a.magnet-link").ok()?;
    if let Some(el) = document.select(&selector).next() {
        // Try title attribute first (contains raw hash)
        if let Some(title) = el.value().attr("title") {
            let trimmed = title.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_lowercase());
            }
        }
        // Fallback: extract from href magnet URI (btih:HASH)
        if let Some(href) = el.value().attr("href") {
            if let Some(start) = href.find("btih:") {
                let hash_start = start + 5;
                let hash_end = href[hash_start..]
                    .find('&')
                    .map(|i| hash_start + i)
                    .unwrap_or(href.len());
                let hash = &href[hash_start..hash_end];
                if !hash.is_empty() {
                    return Some(hash.to_lowercase());
                }
            }
        }
    }
    None
}

/// Extract external links (kinopoisk, imdb, world-art) from the post body.
fn extract_link(document: &Html, domain: &str) -> Option<String> {
    let selector = Selector::parse(".post_body a").ok()?;
    for el in document.select(&selector) {
        if let Some(href) = el.value().attr("href")
            && href.contains(domain)
        {
            return Some(href.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_topic_html() -> String {
        r#"
        <html><body>
        <a id="topic-title">Breaking Bad / Во все тяжкие (Сезон 1-5) [2008-2013, WEB-DL 1080p]</a>
        <div id="tor-size-humn" data-ts_text="53687091200">50 GB</div>
        <span class="seed"><b>142</b></span>
        <li class="seed-distribution-date">Зарегистрирован: 2024-01-15 10:30</li>
        <div class="post_body">
          <a href="https://www.kinopoisk.ru/film/404900/">Кинопоиск</a>
          <a href="https://www.imdb.com/title/tt0903747/">IMDB</a>
          <a href="http://www.world-art.ru/cinema/cinema.php?id=12345">World Art</a>
          Серии 1-62 из 62
        </div>
        </body></html>
        "#
        .to_string()
    }

    #[test]
    fn test_parse_topic_html() {
        let info = parse_topic_html(&mock_topic_html(), "6001234").unwrap();

        assert_eq!(info.topic_id, "6001234");
        assert!(info.title.contains("Breaking Bad"));
        assert_eq!(info.quality.as_deref(), Some("WEB-DL 1080p"));
        assert_eq!(info.registered_at.as_deref(), Some("2024-01-15"));
        assert_eq!(info.size_bytes, 53687091200);
        assert_eq!(info.seeders, 142);
        assert!(info.kinopoisk_url.as_deref().unwrap().contains("kinopoisk"));
        assert!(info.imdb_url.as_deref().unwrap().contains("imdb.com"));
        assert!(
            info.world_art_url
                .as_deref()
                .unwrap()
                .contains("world-art.ru")
        );
    }

    #[test]
    fn test_extract_quality() {
        assert_eq!(
            extract_quality("Title [2024, BDRip 1080p]"),
            Some("BDRip 1080p".to_string())
        );
        assert_eq!(
            extract_quality("Title [WEB-DL 720p]"),
            Some("WEB-DL 720p".to_string())
        );
        assert_eq!(extract_quality("Title [HDRip]"), Some("HDRip".to_string()));
        assert_eq!(extract_quality("Title without quality"), None);
    }

    #[test]
    fn test_extract_episode_info() {
        assert_eq!(
            extract_episode_info("Title (Серии 1-12 из 12)"),
            Some("1-12".to_string())
        );
        assert_eq!(
            extract_episode_info("Title [серии 1-24]"),
            Some("1-24".to_string())
        );
        assert_eq!(
            extract_episode_info("Title (Сезон 1-5)"),
            Some("1-5".to_string())
        );
        assert_eq!(extract_episode_info("Title"), None);
    }

    #[test]
    fn test_extract_episode_info_from_brackets() {
        assert_eq!(
            extract_episode_info("Title [01-16 из 16]"),
            Some("01-16".to_string())
        );
    }

    #[test]
    fn test_extract_link() {
        let html = r#"
        <html><body>
        <div class="post_body">
          <a href="https://www.kinopoisk.ru/film/404900/">KP</a>
          <a href="https://example.com">Other</a>
        </div>
        </body></html>
        "#;
        let doc = Html::parse_document(html);
        assert_eq!(
            extract_link(&doc, "kinopoisk"),
            Some("https://www.kinopoisk.ru/film/404900/".to_string())
        );
        assert_eq!(extract_link(&doc, "imdb.com"), None);
    }

    #[test]
    fn test_parse_topic_minimal_html() {
        let html = "<html><body><a id=\"topic-title\">Simple Title</a></body></html>";
        let info = parse_topic_html(html, "999").unwrap();
        assert_eq!(info.topic_id, "999");
        assert_eq!(info.title, "Simple Title");
        assert_eq!(info.quality, None);
        assert_eq!(info.episode_info, None);
        assert_eq!(info.kinopoisk_url, None);
    }

    #[test]
    fn test_extract_date_from_text() {
        assert_eq!(
            extract_date_from_text("Зарегистрирован: 2024-01-15 10:30"),
            Some("2024-01-15".to_string())
        );
        assert_eq!(extract_date_from_text("no date here"), None);
    }

    #[test]
    fn test_parse_real_rutracker_html() {
        // Minimal HTML matching real RuTracker page structure (title attr for size, no seed-distribution-date)
        let html = r#"
        <html><body>
        <a id="topic-title" class="topic-title-6780201">Добро пожаловать в класс для особо одарённых (ТВ-3) / Youkoso Jitsuryoku Shijou Shugi no Kyoushitsu e 3rd Season / Classroom of the Elite III / Добро пожаловать в класс превосходства 3 [TV] [13 из 13] [RUS(int), JAP+Sub] [2024, драма, повседневность, BDRip] [1080p]</a>
        <span id="tor-size-humn" title="9366413336">8.72&nbsp;GB</span>
        <span class="seed">Сиды:&nbsp; <b>9</b></span>
        <a href="magnet:?xt=urn:btih:2972EE6568A16E674FF5AEC4B06D26B53E7CE140&tr=http%3A%2F%2Fbt.t-ru.org%2Fann"
           class="magnet-link" title="2972EE6568A16E674FF5AEC4B06D26B53E7CE140">magnet</a>
        <div class="post_body">
            <a href="https://www.kinopoisk.ru/film/1234/">KP</a>
            <a href="https://www.world-art.ru/animation/animation.php?id=5678">WA</a>
        </div>
        </body></html>
        "#;
        let info = parse_topic_html(html, "6780201").unwrap();

        assert_eq!(info.topic_id, "6780201");
        assert!(info.title.contains("Classroom of the Elite III"));
        assert_eq!(info.size_bytes, 9366413336);
        assert_eq!(info.seeders, 9);
        assert_eq!(info.quality.as_deref(), Some("BDRip"));
        assert_eq!(
            info.torrent_hash.as_deref(),
            Some("2972ee6568a16e674ff5aec4b06d26b53e7ce140")
        );
        assert!(info.kinopoisk_url.is_some());
        assert!(info.world_art_url.is_some());
    }
}
