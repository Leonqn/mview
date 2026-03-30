use anyhow::{Context, Result};
use reqwest::Client;
use tracing::debug;

use super::models::*;

const TMDB_BASE_URL: &str = "https://api.themoviedb.org/3";

pub struct TmdbClient {
    client: Client,
    api_key: String,
}

impl TmdbClient {
    pub fn new(api_key: &str) -> Result<Self> {
        let client = Client::builder()
            .build()
            .context("Failed to create TMDB HTTP client")?;

        Ok(Self {
            client,
            api_key: api_key.to_string(),
        })
    }

    /// Search for movies by title.
    pub async fn search_movie(&self, query: &str) -> Result<Vec<TmdbMovie>> {
        let url = format!("{}/search/movie", TMDB_BASE_URL);
        debug!(query, "tmdb search movie");

        let response = self
            .client
            .get(&url)
            .query(&[
                ("api_key", self.api_key.as_str()),
                ("query", query),
                ("language", "ru-RU"),
            ])
            .send()
            .await
            .context("failed to send tmdb movie search")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "tmdb movie search failed: {status} — {text}"
            ));
        }

        let data: TmdbSearchResponse<TmdbMovie> = response
            .json()
            .await
            .context("failed to parse tmdb movie search response")?;

        Ok(data.results)
    }

    /// Search for TV series by title.
    pub async fn search_tv(&self, query: &str) -> Result<Vec<TmdbTvShow>> {
        let url = format!("{}/search/tv", TMDB_BASE_URL);
        debug!(query, "tmdb search tv");

        let response = self
            .client
            .get(&url)
            .query(&[
                ("api_key", self.api_key.as_str()),
                ("query", query),
                ("language", "ru-RU"),
            ])
            .send()
            .await
            .context("failed to send tmdb tv search")?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!("tmdb tv search failed: {status} — {text}"));
        }

        let data: TmdbSearchResponse<TmdbTvShow> = response
            .json()
            .await
            .context("Failed to parse TMDB TV search response")?;

        Ok(data.results)
    }

    /// Get detailed movie info by TMDB ID.
    pub async fn get_movie(&self, tmdb_id: i64) -> Result<TmdbMovieDetails> {
        let url = format!("{}/movie/{}", TMDB_BASE_URL, tmdb_id);
        debug!(tmdb_id, "tmdb get movie");

        let resp = self
            .client
            .get(&url)
            .query(&[("api_key", self.api_key.as_str()), ("language", "ru-RU")])
            .send()
            .await
            .context("Failed to get TMDB movie details")?
            .error_for_status()
            .context("TMDB movie details API error")?;

        resp.json()
            .await
            .context("Failed to parse TMDB movie details")
    }

    /// Get detailed TV series info by TMDB ID, including seasons and external IDs.
    pub async fn get_tv(&self, tmdb_id: i64) -> Result<TmdbTvDetails> {
        let url = format!("{}/tv/{}", TMDB_BASE_URL, tmdb_id);
        debug!(tmdb_id, "tmdb get tv");

        let resp = self
            .client
            .get(&url)
            .query(&[
                ("api_key", self.api_key.as_str()),
                ("language", "ru-RU"),
                ("append_to_response", "external_ids"),
            ])
            .send()
            .await
            .context("Failed to get TMDB TV details")?
            .error_for_status()
            .context("TMDB TV details API error")?;

        resp.json().await.context("Failed to parse TMDB TV details")
    }

    /// Get detailed season info with episodes.
    pub async fn get_season(&self, tmdb_id: i64, season_number: i64) -> Result<TmdbSeasonDetails> {
        let url = format!("{}/tv/{}/season/{}", TMDB_BASE_URL, tmdb_id, season_number);
        debug!(tmdb_id, season_number, "tmdb get season");

        let resp = self
            .client
            .get(&url)
            .query(&[("api_key", self.api_key.as_str()), ("language", "ru-RU")])
            .send()
            .await
            .context("Failed to get TMDB season details")?
            .error_for_status()
            .context("TMDB season details API error")?;

        resp.json()
            .await
            .context("Failed to parse TMDB season details")
    }

    pub async fn get_collection(&self, collection_id: i64) -> Result<TmdbCollectionDetails> {
        let url = format!("{}/collection/{}", TMDB_BASE_URL, collection_id);
        debug!(collection_id, "tmdb get collection");

        let resp = self
            .client
            .get(&url)
            .query(&[("api_key", self.api_key.as_str()), ("language", "ru-RU")])
            .send()
            .await
            .context("failed to get tmdb collection")?
            .error_for_status()
            .context("tmdb collection api error")?;

        resp.json().await.context("failed to parse tmdb collection")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = TmdbClient::new("test_api_key").unwrap();
        assert_eq!(client.api_key, "test_api_key");
    }

    #[test]
    fn test_parse_movie_search_json() {
        let json = r#"{
            "page": 1,
            "results": [
                {
                    "id": 550,
                    "title": "Fight Club",
                    "original_title": "Fight Club",
                    "overview": "An insomniac office worker...",
                    "poster_path": "/pB8BM7pdSp6B6Ih7QI4S2t0POsFj.jpg",
                    "release_date": "1999-10-15",
                    "vote_average": 8.433
                }
            ],
            "total_pages": 1,
            "total_results": 1
        }"#;
        let resp: TmdbSearchResponse<TmdbMovie> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].id, 550);
        assert_eq!(resp.results[0].title, "Fight Club");
    }

    #[test]
    fn test_parse_tv_search_json() {
        let json = r#"{
            "page": 1,
            "results": [
                {
                    "id": 1396,
                    "name": "Breaking Bad",
                    "original_name": "Breaking Bad",
                    "overview": "A teacher...",
                    "poster_path": "/poster.jpg",
                    "first_air_date": "2008-01-20",
                    "vote_average": 8.9
                }
            ],
            "total_pages": 1,
            "total_results": 1
        }"#;
        let resp: TmdbSearchResponse<TmdbTvShow> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results[0].id, 1396);
        assert_eq!(resp.results[0].name, "Breaking Bad");
    }

    #[test]
    fn test_parse_movie_details_json() {
        let json = r#"{
            "id": 550,
            "title": "Fight Club",
            "original_title": "Fight Club",
            "overview": "An insomniac...",
            "poster_path": "/poster.jpg",
            "release_date": "1999-10-15",
            "imdb_id": "tt0137523",
            "vote_average": 8.4,
            "runtime": 139
        }"#;
        let details: TmdbMovieDetails = serde_json::from_str(json).unwrap();
        assert_eq!(details.imdb_id, Some("tt0137523".to_string()));
    }

    #[test]
    fn test_parse_tv_details_with_external_ids() {
        let json = r#"{
            "id": 1396,
            "name": "Breaking Bad",
            "original_name": "Breaking Bad",
            "overview": "A teacher...",
            "poster_path": "/poster.jpg",
            "first_air_date": "2008-01-20",
            "number_of_seasons": 5,
            "number_of_episodes": 62,
            "seasons": [
                {
                    "id": 3573,
                    "season_number": 1,
                    "name": "Season 1",
                    "episode_count": 7,
                    "air_date": "2008-01-20",
                    "poster_path": null
                }
            ],
            "vote_average": 8.9,
            "external_ids": {
                "imdb_id": "tt0903747"
            }
        }"#;
        let details: TmdbTvDetails = serde_json::from_str(json).unwrap();
        assert_eq!(details.number_of_seasons, Some(5));
        assert_eq!(
            details.external_ids.unwrap().imdb_id,
            Some("tt0903747".to_string())
        );
    }

    #[test]
    fn test_parse_collection_json() {
        let json = r#"{
            "id": 1241,
            "name": "Harry Potter Collection",
            "overview": "The Harry Potter film series...",
            "poster_path": "/collection.jpg",
            "parts": [
                {
                    "id": 671,
                    "title": "Harry Potter and the Philosopher's Stone",
                    "original_title": "Harry Potter and the Philosopher's Stone",
                    "release_date": "2001-11-16",
                    "poster_path": "/hp1.jpg",
                    "overview": "First movie..."
                },
                {
                    "id": 672,
                    "title": "Harry Potter and the Chamber of Secrets",
                    "original_title": "Harry Potter and the Chamber of Secrets",
                    "release_date": "2002-11-15",
                    "poster_path": null,
                    "overview": "Second movie..."
                }
            ]
        }"#;
        let collection: TmdbCollectionDetails = serde_json::from_str(json).unwrap();
        assert_eq!(collection.id, 1241);
        assert_eq!(collection.name, "Harry Potter Collection");
        assert_eq!(collection.parts.len(), 2);
        assert_eq!(collection.parts[0].id, 671);
        assert!(collection.parts[1].poster_path.is_none());
    }

    #[test]
    fn test_parse_season_details_json() {
        let json = r#"{
            "id": 3573,
            "season_number": 1,
            "name": "Season 1",
            "episodes": [
                {
                    "id": 62085,
                    "episode_number": 1,
                    "name": "Pilot",
                    "air_date": "2008-01-20",
                    "overview": "Walter White..."
                }
            ]
        }"#;
        let season: TmdbSeasonDetails = serde_json::from_str(json).unwrap();
        assert_eq!(season.episodes.unwrap().len(), 1);
    }
}
