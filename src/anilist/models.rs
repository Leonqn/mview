use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct GraphQLResponse<T> {
    pub data: T,
}

#[derive(Debug, Deserialize)]
pub struct SearchData {
    #[serde(rename = "Page")]
    pub page: PageData,
}

#[derive(Debug, Deserialize)]
pub struct PageData {
    pub media: Vec<AniListMedia>,
}

#[derive(Debug, Deserialize)]
pub struct MediaData {
    #[serde(rename = "Media")]
    pub media: AniListMedia,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListMedia {
    pub id: i64,
    pub title: AniListTitle,
    pub episodes: Option<i64>,
    #[serde(rename = "seasonYear")]
    pub season_year: Option<i64>,
    pub format: Option<String>,
    pub status: Option<String>,
    pub description: Option<String>,
    #[serde(rename = "coverImage")]
    pub cover_image: Option<AniListCoverImage>,
    #[serde(rename = "airingSchedule")]
    pub airing_schedule: Option<AniListAiringSchedule>,
    #[serde(rename = "streamingEpisodes", default)]
    pub streaming_episodes: Vec<AniListStreamingEpisode>,
    pub relations: Option<AniListRelations>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListStreamingEpisode {
    pub title: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListAiringSchedule {
    pub nodes: Vec<AniListAiringNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListAiringNode {
    pub episode: i64,
    #[serde(rename = "airingAt")]
    pub airing_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListTitle {
    pub romaji: Option<String>,
    pub english: Option<String>,
    pub native: Option<String>,
}

impl AniListMedia {
    /// Get air date for a specific episode as "YYYY-MM-DD" string.
    pub fn episode_air_date(&self, episode: i64) -> Option<String> {
        self.airing_schedule.as_ref().and_then(|schedule| {
            schedule
                .nodes
                .iter()
                .find(|n| n.episode == episode)
                .map(|n| unix_to_date(n.airing_at))
        })
    }

    /// Parse streaming_episodes titles like "Episode 0 - Prologue" into (number, title) pairs.
    /// Returns a raw sorted Vec of (episode_number, title) — NO renormalization applied.
    /// Callers that know the sequel chain offset should apply it themselves via
    /// `streaming_episodes_for_season`.
    pub fn parsed_streaming_episodes_raw(&self) -> Vec<(i64, String)> {
        let mut result = Vec::new();
        for ep in &self.streaming_episodes {
            let raw = match ep.title.as_deref() {
                Some(t) => t.trim(),
                None => continue,
            };
            // Expected format: "Episode N - Title" or "Episode N"
            let lower = raw.to_lowercase();
            if let Some(rest) = lower.strip_prefix("episode ") {
                let num_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if num_str.is_empty() {
                    continue;
                }
                let num: i64 = match num_str.parse() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                // Extract title after " - " (preserving original case)
                let title = raw
                    .split_once('-')
                    .map(|(_, t)| t.trim().to_string())
                    .unwrap_or_default();
                result.push((num, title));
            }
        }
        result.sort_by_key(|(n, _)| *n);
        result
    }
}

fn unix_to_date(ts: i64) -> String {
    // Simple unix timestamp to YYYY-MM-DD without external deps
    let secs_per_day: i64 = 86400;
    let mut days = ts / secs_per_day;
    // Days since 1970-01-01
    let mut year = 1970;
    loop {
        let days_in_year = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
            366
        } else {
            365
        };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    for (i, &md) in month_days.iter().enumerate() {
        if days < md {
            month = i + 1;
            break;
        }
        days -= md;
    }
    let day = days + 1;
    format!("{year:04}-{month:02}-{day:02}")
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListCoverImage {
    pub large: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListRelations {
    pub edges: Vec<AniListRelationEdge>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListRelationEdge {
    #[serde(rename = "relationType")]
    pub relation_type: String,
    pub node: AniListRelationNode,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AniListRelationNode {
    pub id: i64,
    #[serde(rename = "type")]
    pub media_type: Option<String>,
}

/// Unified search result for the UI, analogous to TmdbSearchItem.
#[derive(Debug, Clone, Serialize)]
pub struct AniListSearchItem {
    pub anilist_id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<String>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub episodes: Option<i64>,
    pub format: Option<String>,
    pub status: Option<String>,
}

impl AniListSearchItem {
    pub fn from_media(m: &AniListMedia) -> Self {
        let title = m
            .title
            .english
            .clone()
            .or_else(|| m.title.romaji.clone())
            .unwrap_or_default();

        let original_title = m.title.native.clone();
        let year = m.season_year.map(|y| y.to_string());
        let poster_url = m.cover_image.as_ref().and_then(|c| c.large.clone());

        // Strip HTML tags from description
        let overview = m.description.as_ref().map(|d| strip_html(d));

        Self {
            anilist_id: m.id,
            title,
            original_title,
            year,
            overview,
            poster_url,
            episodes: m.episodes,
            format: m.format.clone(),
            status: m.status.clone(),
        }
    }
}

fn strip_html(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_search_response() {
        let json = r#"{
            "data": {
                "Page": {
                    "media": [
                        {
                            "id": 101922,
                            "idMal": 40748,
                            "title": {
                                "romaji": "Jujutsu Kaisen",
                                "english": "JUJUTSU KAISEN",
                                "native": "呪術廻戦"
                            },
                            "episodes": 24,
                            "season": "FALL",
                            "seasonYear": 2020,
                            "format": "TV",
                            "status": "FINISHED",
                            "description": "A boy fights <b>curses</b>.",
                            "coverImage": {
                                "large": "https://example.com/cover.jpg",
                                "medium": null
                            },
                            "nextAiringEpisode": null,
                            "relations": null
                        }
                    ]
                }
            }
        }"#;

        let resp: GraphQLResponse<SearchData> = serde_json::from_str(json).unwrap();
        let media = &resp.data.page.media;
        assert_eq!(media.len(), 1);
        assert_eq!(media[0].id, 101922);
        assert_eq!(media[0].title.english.as_deref(), Some("JUJUTSU KAISEN"));
        assert_eq!(media[0].episodes, Some(24));
        assert_eq!(media[0].season_year, Some(2020));
    }

    #[test]
    fn test_deserialize_media_with_relations() {
        let json = r#"{
            "data": {
                "Media": {
                    "id": 101922,
                    "idMal": null,
                    "title": { "romaji": "Jujutsu Kaisen", "english": null, "native": null },
                    "episodes": 24,
                    "season": null,
                    "seasonYear": 2020,
                    "format": "TV",
                    "status": "FINISHED",
                    "description": null,
                    "coverImage": null,
                    "nextAiringEpisode": null,
                    "relations": {
                        "edges": [
                            {
                                "relationType": "SEQUEL",
                                "node": {
                                    "id": 145064,
                                    "type": "ANIME",
                                    "format": "TV",
                                    "title": { "romaji": "Jujutsu Kaisen 2nd Season", "english": null, "native": null },
                                    "episodes": 23,
                                    "status": "FINISHED",
                                    "coverImage": null,
                                    "nextAiringEpisode": null
                                }
                            }
                        ]
                    }
                }
            }
        }"#;

        let resp: GraphQLResponse<MediaData> = serde_json::from_str(json).unwrap();
        let media = &resp.data.media;
        assert_eq!(media.id, 101922);

        let relations = media.relations.as_ref().unwrap();
        assert_eq!(relations.edges.len(), 1);
        assert_eq!(relations.edges[0].relation_type, "SEQUEL");
        assert_eq!(relations.edges[0].node.id, 145064);
    }

    #[test]
    fn test_search_item_from_media() {
        let media = AniListMedia {
            id: 101922,
            title: AniListTitle {
                romaji: Some("Jujutsu Kaisen".into()),
                english: Some("JUJUTSU KAISEN".into()),
                native: Some("呪術廻戦".into()),
            },
            episodes: Some(24),
            season_year: Some(2020),
            format: Some("TV".into()),
            status: Some("FINISHED".into()),
            description: Some("A boy fights <b>curses</b>.".into()),
            cover_image: Some(AniListCoverImage {
                large: Some("https://example.com/cover.jpg".into()),
            }),
            airing_schedule: None,
            streaming_episodes: Vec::new(),
            relations: None,
        };

        let item = AniListSearchItem::from_media(&media);
        assert_eq!(item.anilist_id, 101922);
        assert_eq!(item.title, "JUJUTSU KAISEN");
        assert_eq!(item.original_title.as_deref(), Some("呪術廻戦"));
        assert_eq!(item.year.as_deref(), Some("2020"));
        assert_eq!(item.overview.as_deref(), Some("A boy fights curses."));
        assert_eq!(
            item.poster_url.as_deref(),
            Some("https://example.com/cover.jpg")
        );
    }

    #[test]
    fn test_search_item_fallback_to_romaji() {
        let media = AniListMedia {
            id: 1,
            title: AniListTitle {
                romaji: Some("Romaji Title".into()),
                english: None,
                native: None,
            },
            episodes: None,
            season_year: None,
            format: None,
            status: None,
            description: None,
            cover_image: None,
            airing_schedule: None,
            streaming_episodes: Vec::new(),
            relations: None,
        };

        let item = AniListSearchItem::from_media(&media);
        assert_eq!(item.title, "Romaji Title");
    }

    #[test]
    fn test_parsed_streaming_episodes() {
        let media = AniListMedia {
            id: 1,
            title: AniListTitle {
                romaji: None,
                english: None,
                native: None,
            },
            episodes: Some(13),
            season_year: None,
            format: None,
            status: None,
            description: None,
            cover_image: None,
            airing_schedule: None,
            streaming_episodes: vec![
                AniListStreamingEpisode {
                    title: Some("Episode 0 - Prologue".into()),
                },
                AniListStreamingEpisode {
                    title: Some("Episode 1 - Winter Days, A Fateful Night".into()),
                },
                AniListStreamingEpisode {
                    title: Some("Episode 2 - The Curtain Goes Up".into()),
                },
                AniListStreamingEpisode {
                    title: Some("Episode 13".into()),
                },
                AniListStreamingEpisode { title: None },
                AniListStreamingEpisode {
                    title: Some("something unrelated".into()),
                },
            ],
            relations: None,
        };

        let parsed = media.parsed_streaming_episodes_raw();
        assert_eq!(parsed.len(), 4);
        assert_eq!(parsed[0], (0, "Prologue".to_string()));
        assert_eq!(parsed[1], (1, "Winter Days, A Fateful Night".to_string()));
        assert_eq!(parsed[2], (2, "The Curtain Goes Up".to_string()));
        assert_eq!(parsed[3], (13, "".to_string()));
    }

    #[test]
    fn test_strip_html() {
        assert_eq!(strip_html("plain text"), "plain text");
        assert_eq!(strip_html("<b>bold</b>"), "bold");
        assert_eq!(strip_html("a<br>b<br/>c"), "abc");
        assert_eq!(strip_html("<i>nested <b>tags</b></i>"), "nested tags");
    }
}
