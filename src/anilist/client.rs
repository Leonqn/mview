use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;
use tracing::{debug, info};

use super::models::{AniListMedia, AniListRelationNode, GraphQLResponse, MediaData, SearchData};

const ANILIST_URL: &str = "https://graphql.anilist.co";

const SEARCH_QUERY: &str = r#"
query ($search: String!) {
    Page(perPage: 10) {
        media(search: $search, type: ANIME, sort: SEARCH_MATCH) {
            id
            title { romaji english native }
            episodes
            seasonYear
            format
            status
            description
            coverImage { large }
        }
    }
}
"#;

const MEDIA_QUERY: &str = r#"
query ($id: Int!) {
    Media(id: $id, type: ANIME) {
        id
        title { romaji english native }
        episodes
        seasonYear
        format
        status
        description
        coverImage { large }
        airingSchedule(perPage: 50) {
            nodes { episode airingAt }
        }
        relations {
            edges {
                relationType
                node {
                    id
                    type
                }
            }
        }
    }
}
"#;

pub struct AniListClient {
    client: Client,
}

impl AniListClient {
    pub fn new() -> Result<Self> {
        let client = Client::builder()
            .build()
            .context("failed to create anilist http client")?;
        Ok(Self { client })
    }

    pub async fn search(&self, query: &str) -> Result<Vec<AniListMedia>> {
        let search_query = query.to_lowercase();
        debug!(query = search_query, "searching anilist");
        let body = json!({
            "query": SEARCH_QUERY,
            "variables": { "search": search_query }
        });

        let response = self
            .client
            .post(ANILIST_URL)
            .json(&body)
            .send()
            .await
            .context("failed to send anilist search request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("anilist search failed: {status} — {text}"));
        }

        let resp: GraphQLResponse<SearchData> = response
            .json()
            .await
            .context("failed to parse anilist search response")?;

        let results = resp.data.page.media;
        info!(query, count = results.len(), "searched anilist");
        Ok(results)
    }

    pub async fn get_media(&self, id: i64) -> Result<AniListMedia> {
        debug!(id, "fetching anilist media");
        let body = json!({
            "query": MEDIA_QUERY,
            "variables": { "id": id }
        });

        let response = self
            .client
            .post(ANILIST_URL)
            .json(&body)
            .send()
            .await
            .context("failed to send anilist media request")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "anilist media query failed for id {id}: {status} — {text}"
            ));
        }

        let resp: GraphQLResponse<MediaData> = response
            .json()
            .await
            .context("failed to parse anilist media response")?;

        Ok(resp.data.media)
    }

    /// Build a complete franchise chain: first walk back through PREQUELs to find
    /// the root, then walk forward through SEQUELs to collect all entries in order.
    pub async fn get_sequel_chain(&self, start_id: i64) -> Result<Vec<AniListMedia>> {
        debug!(start_id, "building franchise chain");

        // Walk back through PREQUELs to find the root of the franchise
        let mut root_id = start_id;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..20 {
            if !seen.insert(root_id) {
                break;
            }
            let media = self.get_media(root_id).await?;
            match find_prequel(&media) {
                Some(id) => {
                    debug!(current = root_id, prequel = id, "following prequel");
                    root_id = id;
                }
                None => break,
            }
        }

        if root_id != start_id {
            debug!(start_id, root_id, "found franchise root");
        }

        // Walk forward through SEQUELs from the root
        let mut chain = Vec::new();
        let mut current_id = root_id;
        seen.clear();

        for _ in 0..20 {
            if !seen.insert(current_id) {
                break;
            }

            let media = self.get_media(current_id).await?;
            let sequel_id = find_sequel(&media);

            chain.push(media);

            match sequel_id {
                Some(id) => {
                    debug!(current = current_id, sequel = id, "following sequel");
                    current_id = id;
                }
                None => break,
            }
        }

        info!(
            start_id,
            root_id,
            count = chain.len(),
            "franchise chain built"
        );
        Ok(chain)
    }
}

fn find_relation(media: &AniListMedia, relation_type: &str) -> Option<i64> {
    media.relations.as_ref().and_then(|r| {
        r.edges
            .iter()
            .find(|e| e.relation_type == relation_type && is_anime(&e.node))
            .map(|e| e.node.id)
    })
}

fn find_sequel(media: &AniListMedia) -> Option<i64> {
    find_relation(media, "SEQUEL")
}

fn find_prequel(media: &AniListMedia) -> Option<i64> {
    find_relation(media, "PREQUEL")
}

fn is_anime(node: &AniListRelationNode) -> bool {
    node.media_type
        .as_deref()
        .map(|t| t == "ANIME")
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anilist::models::*;

    fn make_media_with_sequel(id: i64, sequel_id: Option<i64>) -> AniListMedia {
        let relations = sequel_id.map(|sid| AniListRelations {
            edges: vec![AniListRelationEdge {
                relation_type: "SEQUEL".to_string(),
                node: AniListRelationNode {
                    id: sid,
                    media_type: Some("ANIME".to_string()),
                },
            }],
        });

        AniListMedia {
            id,
            title: AniListTitle {
                romaji: Some(format!("Show {id}")),
                english: None,
                native: None,
            },
            episodes: Some(12),
            season_year: None,
            format: Some("TV".into()),
            status: Some("FINISHED".into()),
            description: None,
            cover_image: None,
            airing_schedule: None,
            relations,
        }
    }

    #[test]
    fn test_find_sequel_with_sequel() {
        let media = make_media_with_sequel(1, Some(2));
        assert_eq!(find_sequel(&media), Some(2));
    }

    #[test]
    fn test_find_sequel_without_sequel() {
        let media = make_media_with_sequel(1, None);
        assert_eq!(find_sequel(&media), None);
    }

    #[test]
    fn test_find_sequel_includes_movie() {
        let media = AniListMedia {
            id: 1,
            title: AniListTitle {
                romaji: None,
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
            relations: Some(AniListRelations {
                edges: vec![AniListRelationEdge {
                    relation_type: "SEQUEL".to_string(),
                    node: AniListRelationNode {
                        id: 2,
                        media_type: Some("ANIME".to_string()),
                    },
                }],
            }),
        };
        assert_eq!(find_sequel(&media), Some(2));
    }

    #[test]
    fn test_find_sequel_filters_non_anime() {
        let media = AniListMedia {
            id: 1,
            title: AniListTitle {
                romaji: None,
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
            relations: Some(AniListRelations {
                edges: vec![AniListRelationEdge {
                    relation_type: "SEQUEL".to_string(),
                    node: AniListRelationNode {
                        id: 2,
                        media_type: Some("MANGA".to_string()),
                    },
                }],
            }),
        };
        assert_eq!(find_sequel(&media), None);
    }

    #[test]
    fn test_find_sequel_ignores_prequel() {
        let media = AniListMedia {
            id: 1,
            title: AniListTitle {
                romaji: None,
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
            relations: Some(AniListRelations {
                edges: vec![AniListRelationEdge {
                    relation_type: "PREQUEL".to_string(),
                    node: AniListRelationNode {
                        id: 2,
                        media_type: Some("ANIME".to_string()),
                    },
                }],
            }),
        };
        assert_eq!(find_sequel(&media), None);
    }
}
