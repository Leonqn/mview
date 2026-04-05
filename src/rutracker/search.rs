use anyhow::{Context, Result};
use scraper::{Html, Selector};
use serde::Serialize;
use tracing::info;

use super::client::RutrackerClient;

/// A single search result from RuTracker's search page.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub topic_id: String,
    pub title: String,
    pub size: String,
    pub size_bytes: i64,
    pub seeders: i32,
    pub leechers: i32,
    pub forum_name: String,
    pub url: String,
}

/// The search endpoint path on RuTracker.
const SEARCH_PATH: &str = "/forum/tracker.php";

impl RutrackerClient {
    /// Search RuTracker for the given query string.
    /// Parses the HTML results table and returns structured results.
    pub async fn search(&self, query: &str) -> Result<Vec<SearchResult>> {
        info!(query, "searching rutracker");
        // Rutracker treats " -" (space-dash) as an exclude operator, which breaks queries
        // like "Fate/strange Fake -Whispers of Dawn-". Replace dashes with spaces — rutracker
        // ignores punctuation anyway when matching, so result set is equivalent.
        let sanitized = sanitize_query(query);
        let url = format!("{}{}", self.base_url(), SEARCH_PATH);
        let full_url = reqwest::Url::parse_with_params(&url, &[("nm", sanitized.as_str())])
            .context("Failed to build search URL")?;
        let html = self
            .get(full_url.as_str())
            .await
            .context("Failed to fetch search results")?;
        let results = parse_search_results(&html, self.base_url())?;
        let total = results.len();
        let results: Vec<SearchResult> = results
            .into_iter()
            .filter(|r| !is_audio_only_forum(&r.forum_name))
            .collect();
        info!(
            query,
            count = results.len(),
            dropped_audio = total - results.len(),
            "rutracker search completed"
        );
        Ok(results)
    }
}

/// Check if a rutracker forum name indicates soundtrack/music content (to be excluded).
fn is_audio_only_forum(forum_name: &str) -> bool {
    let lower = forum_name.to_lowercase();
    lower.contains("саундтрек")
        || lower.contains("ost")
        || lower.contains("музык")
        || lower.contains("soundtrack")
        || lower.contains("soundtracks")
}

/// Replace characters in the query that rutracker treats as search operators.
/// `-` is treated as "exclude", `+` as "required", `"` as "phrase".
fn sanitize_query(query: &str) -> String {
    query
        .chars()
        .map(|c| match c {
            '-' | '+' | '"' => ' ',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse search results HTML from RuTracker's tracker.php page.
/// Each result row is a <tr> in the search results table with class "tCenter hl-tr".
pub fn parse_search_results(html: &str, base_url: &str) -> Result<Vec<SearchResult>> {
    let document = Html::parse_document(html);

    let row_selector = Selector::parse("tr.tCenter.hl-tr").unwrap();
    let topic_link_selector = Selector::parse("td.t-title-col a.tLink").unwrap();
    let size_selector = Selector::parse("td.tor-size a").unwrap();
    let seeders_selector = Selector::parse("td.seedmed b").unwrap();
    let leechers_selector = Selector::parse("td.leechmed b").unwrap();
    let forum_selector = Selector::parse("td.f-name-col a").unwrap();

    let mut results = Vec::new();

    for row in document.select(&row_selector) {
        let topic_link = match row.select(&topic_link_selector).next() {
            Some(el) => el,
            None => continue,
        };

        let title = topic_link.text().collect::<String>().trim().to_string();
        let href = topic_link.value().attr("href").unwrap_or_default();
        let topic_id = extract_topic_id(href);

        let (size, size_bytes) = match row.select(&size_selector).next() {
            Some(el) => {
                let bytes: i64 = el
                    .value()
                    .attr("data-ts_text")
                    .unwrap_or("0")
                    .parse()
                    .unwrap_or(0);
                let display = el.text().collect::<String>().trim().to_string();
                (display, bytes)
            }
            None => ("N/A".to_string(), 0),
        };

        let seeders = row
            .select(&seeders_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().parse().unwrap_or(0))
            .unwrap_or(0);

        let leechers = row
            .select(&leechers_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().parse().unwrap_or(0))
            .unwrap_or(0);

        let forum_name = row
            .select(&forum_selector)
            .next()
            .map(|el| el.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        let url = if topic_id.is_empty() {
            String::new()
        } else {
            format!("{}/forum/viewtopic.php?t={}", base_url, topic_id)
        };

        results.push(SearchResult {
            topic_id,
            title,
            size,
            size_bytes,
            seeders,
            leechers,
            forum_name,
            url,
        });
    }

    Ok(results)
}

/// Extract topic ID from an href like "viewtopic.php?t=12345"
fn extract_topic_id(href: &str) -> String {
    // Match "t=" only when it's a standalone query parameter (after '?' or '&')
    let candidates = [("?t=", 3), ("&t=", 3)];
    for (pattern, skip) in &candidates {
        if let Some(pos) = href.find(pattern) {
            let start = pos + skip;
            let end = href[start..]
                .find('&')
                .map(|i| start + i)
                .unwrap_or(href.len());
            return href[start..end].to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_search_html() -> &'static str {
        r##"
        <html><body>
        <table id="tor-tbl">
          <tr class="tCenter hl-tr">
            <td class="f-name-col"><a href="viewforum.php?f=123">Сериалы</a></td>
            <td class="t-title-col">
              <a class="tLink" href="viewtopic.php?t=6001234">Breaking Bad / Во все тяжкие (Сезон 1-5) [2008-2013, WEB-DL 1080p]</a>
            </td>
            <td class="tor-size">
              <a href="#" data-ts_text="53687091200">50 GB</a>
            </td>
            <td class="seedmed"><b>142</b></td>
            <td class="leechmed"><b>12</b></td>
          </tr>
          <tr class="tCenter hl-tr">
            <td class="f-name-col"><a href="viewforum.php?f=456">Фильмы</a></td>
            <td class="t-title-col">
              <a class="tLink" href="viewtopic.php?t=6005678">Breaking Bad Movie [2019, BDRip 1080p]</a>
            </td>
            <td class="tor-size">
              <a href="#" data-ts_text="4294967296">4 GB</a>
            </td>
            <td class="seedmed"><b>87</b></td>
            <td class="leechmed"><b>3</b></td>
          </tr>
        </table>
        </body></html>
        "##
    }

    #[test]
    fn test_parse_search_results() {
        let results = parse_search_results(mock_search_html(), "https://rutracker.org").unwrap();
        assert_eq!(results.len(), 2);

        let first = &results[0];
        assert_eq!(first.topic_id, "6001234");
        assert!(first.title.contains("Breaking Bad"));
        assert_eq!(first.size, "50 GB");
        assert_eq!(first.size_bytes, 53687091200);
        assert_eq!(first.seeders, 142);
        assert_eq!(first.leechers, 12);
        assert_eq!(first.forum_name, "Сериалы");
        assert_eq!(
            first.url,
            "https://rutracker.org/forum/viewtopic.php?t=6001234"
        );

        let second = &results[1];
        assert_eq!(second.topic_id, "6005678");
        assert_eq!(second.seeders, 87);
        assert_eq!(second.size_bytes, 4294967296);
    }

    #[test]
    fn test_parse_empty_results() {
        let html = "<html><body><table></table></body></html>";
        let results = parse_search_results(html, "https://rutracker.org").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_extract_topic_id() {
        assert_eq!(extract_topic_id("viewtopic.php?t=12345"), "12345");
        assert_eq!(extract_topic_id("viewtopic.php?t=999&start=0"), "999");
        assert_eq!(extract_topic_id("something_else"), "");
    }

    #[test]
    fn test_is_audio_only_forum() {
        assert!(is_audio_only_forum("Саундтреки, караоке и мьюзиклы"));
        assert!(is_audio_only_forum("Аниме OST"));
        assert!(is_audio_only_forum("Зарубежная рок-музыка"));
        assert!(is_audio_only_forum("Soundtracks"));
        assert!(!is_audio_only_forum("Аниме"));
        assert!(!is_audio_only_forum("Сериалы"));
        assert!(!is_audio_only_forum("Зарубежные фильмы"));
    }

    #[test]
    fn test_sanitize_query() {
        assert_eq!(
            sanitize_query("Fate/strange Fake -Whispers of Dawn-"),
            "Fate/strange Fake Whispers of Dawn"
        );
        assert_eq!(sanitize_query("Show TV-1 2020"), "Show TV 1 2020");
        assert_eq!(
            sanitize_query("\"quoted phrase\" +word"),
            "quoted phrase word"
        );
        assert_eq!(sanitize_query("normal title"), "normal title");
        assert_eq!(sanitize_query("  spaces   around  "), "spaces around");
    }

    #[test]
    fn test_parse_row_missing_optional_fields() {
        let html = r#"
        <html><body>
        <table>
          <tr class="tCenter hl-tr">
            <td class="t-title-col">
              <a class="tLink" href="viewtopic.php?t=111">Some Title</a>
            </td>
          </tr>
        </table>
        </body></html>
        "#;
        let results = parse_search_results(html, "https://rutracker.org").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].topic_id, "111");
        assert_eq!(results[0].title, "Some Title");
        assert_eq!(results[0].seeders, 0);
        assert_eq!(results[0].size, "N/A");
        assert_eq!(results[0].size_bytes, 0);
    }
}
