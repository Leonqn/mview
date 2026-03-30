use serde::{Deserialize, Serialize};

/// TMDB API search response wrapper.
#[derive(Debug, Deserialize)]
pub struct TmdbSearchResponse<T> {
    pub results: Vec<T>,
}

/// A movie result from TMDB search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbMovie {
    pub id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub release_date: Option<String>,
    pub vote_average: Option<f64>,
}

/// A TV series result from TMDB search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbTvShow {
    pub id: i64,
    pub name: String,
    pub original_name: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub first_air_date: Option<String>,
    pub vote_average: Option<f64>,
}

/// Detailed movie info from TMDB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbMovieDetails {
    pub id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub release_date: Option<String>,
    pub imdb_id: Option<String>,
    pub vote_average: Option<f64>,
    pub runtime: Option<i64>,
    pub belongs_to_collection: Option<TmdbCollectionRef>,
}

/// Reference to a collection returned in movie details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbCollectionRef {
    pub id: i64,
    pub name: String,
    pub poster_path: Option<String>,
}

/// Full collection details with all movies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbCollectionDetails {
    pub id: i64,
    pub name: String,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub parts: Vec<TmdbCollectionPart>,
}

/// A movie within a collection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbCollectionPart {
    pub id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub release_date: Option<String>,
    pub poster_path: Option<String>,
    pub overview: Option<String>,
}

/// Detailed TV series info from TMDB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbTvDetails {
    pub id: i64,
    pub name: String,
    pub original_name: Option<String>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub first_air_date: Option<String>,
    pub number_of_seasons: Option<i64>,
    pub number_of_episodes: Option<i64>,
    pub seasons: Option<Vec<TmdbSeasonSummary>>,
    pub vote_average: Option<f64>,
    pub external_ids: Option<TmdbExternalIds>,
}

/// Season summary as returned in TV details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbSeasonSummary {
    pub id: i64,
    pub season_number: i64,
    pub name: Option<String>,
    pub episode_count: Option<i64>,
    pub air_date: Option<String>,
    pub poster_path: Option<String>,
}

/// Detailed season info with episodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbSeasonDetails {
    pub id: i64,
    pub season_number: i64,
    pub name: Option<String>,
    pub episodes: Option<Vec<TmdbEpisode>>,
}

/// Episode info from TMDB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbEpisode {
    pub id: i64,
    pub episode_number: i64,
    pub name: Option<String>,
    pub air_date: Option<String>,
    pub overview: Option<String>,
}

/// External IDs for a TV show (available via append_to_response).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmdbExternalIds {
    pub imdb_id: Option<String>,
}

/// Unified search result for display in the UI, combining TMDB data.
#[derive(Debug, Clone, Serialize)]
pub struct TmdbSearchItem {
    pub tmdb_id: i64,
    pub media_type: String,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<String>,
    pub overview: Option<String>,
    pub poster_url: Option<String>,
    pub vote_average: Option<f64>,
}

impl TmdbSearchItem {
    pub fn from_movie(m: &TmdbMovie) -> Self {
        Self {
            tmdb_id: m.id,
            media_type: "movie".to_string(),
            title: m.title.clone(),
            original_title: m.original_title.clone(),
            year: m
                .release_date
                .as_ref()
                .and_then(|d| d.get(..4).map(|s| s.to_string())),
            overview: m.overview.clone(),
            poster_url: m
                .poster_path
                .as_ref()
                .map(|p| format!("https://image.tmdb.org/t/p/w780{}", p)),
            vote_average: m.vote_average,
        }
    }

    pub fn from_tv(t: &TmdbTvShow) -> Self {
        Self {
            tmdb_id: t.id,
            media_type: "series".to_string(),
            title: t.name.clone(),
            original_title: t.original_name.clone(),
            year: t
                .first_air_date
                .as_ref()
                .and_then(|d| d.get(..4).map(|s| s.to_string())),
            overview: t.overview.clone(),
            poster_url: t
                .poster_path
                .as_ref()
                .map(|p| format!("https://image.tmdb.org/t/p/w780{}", p)),
            vote_average: t.vote_average,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_movie_search_response() {
        let json = r#"{
            "page": 1,
            "results": [
                {
                    "id": 550,
                    "title": "Fight Club",
                    "original_title": "Fight Club",
                    "overview": "A ticking-Loss bomb...",
                    "poster_path": "/pB8BM7pdSp6B6Ih7QI4S2t0POsFj.jpg",
                    "release_date": "1999-10-15",
                    "vote_average": 8.4
                }
            ],
            "total_pages": 1,
            "total_results": 1
        }"#;
        let resp: TmdbSearchResponse<TmdbMovie> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].title, "Fight Club");
        assert_eq!(resp.results[0].id, 550);
    }

    #[test]
    fn test_deserialize_tv_search_response() {
        let json = r#"{
            "page": 1,
            "results": [
                {
                    "id": 1396,
                    "name": "Breaking Bad",
                    "original_name": "Breaking Bad",
                    "overview": "A high school chemistry teacher...",
                    "poster_path": "/ggFHVNu6YYI5L9pCfOacjizRGt.jpg",
                    "first_air_date": "2008-01-20",
                    "vote_average": 8.9
                }
            ],
            "total_pages": 1,
            "total_results": 1
        }"#;
        let resp: TmdbSearchResponse<TmdbTvShow> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 1);
        assert_eq!(resp.results[0].name, "Breaking Bad");
    }

    #[test]
    fn test_search_item_from_movie() {
        let movie = TmdbMovie {
            id: 550,
            title: "Fight Club".to_string(),
            original_title: Some("Fight Club".to_string()),
            overview: Some("An insomniac office worker...".to_string()),
            poster_path: Some("/poster.jpg".to_string()),
            release_date: Some("1999-10-15".to_string()),
            vote_average: Some(8.4),
        };
        let item = TmdbSearchItem::from_movie(&movie);
        assert_eq!(item.media_type, "movie");
        assert_eq!(item.year, Some("1999".to_string()));
        assert_eq!(
            item.poster_url,
            Some("https://image.tmdb.org/t/p/w780/poster.jpg".to_string())
        );
    }

    #[test]
    fn test_search_item_from_tv() {
        let tv = TmdbTvShow {
            id: 1396,
            name: "Breaking Bad".to_string(),
            original_name: Some("Breaking Bad".to_string()),
            overview: None,
            poster_path: None,
            first_air_date: Some("2008-01-20".to_string()),
            vote_average: Some(8.9),
        };
        let item = TmdbSearchItem::from_tv(&tv);
        assert_eq!(item.media_type, "series");
        assert_eq!(item.year, Some("2008".to_string()));
        assert!(item.poster_url.is_none());
    }

    #[test]
    fn test_deserialize_movie_details() {
        let json = r#"{
            "id": 550,
            "title": "Fight Club",
            "original_title": "Fight Club",
            "overview": "A ticking-Loss bomb...",
            "poster_path": "/poster.jpg",
            "release_date": "1999-10-15",
            "imdb_id": "tt0137523",
            "vote_average": 8.4,
            "runtime": 139
        }"#;
        let details: TmdbMovieDetails = serde_json::from_str(json).unwrap();
        assert_eq!(details.imdb_id, Some("tt0137523".to_string()));
        assert_eq!(details.runtime, Some(139));
    }

    #[test]
    fn test_deserialize_tv_details_with_seasons() {
        let json = r#"{
            "id": 1396,
            "name": "Breaking Bad",
            "original_name": "Breaking Bad",
            "overview": "A high school chemistry teacher...",
            "poster_path": "/poster.jpg",
            "first_air_date": "2008-01-20",
            "number_of_seasons": 5,
            "number_of_episodes": 62,
            "seasons": [
                {
                    "id": 3572,
                    "season_number": 0,
                    "name": "Specials",
                    "episode_count": 9,
                    "air_date": "2009-02-17",
                    "poster_path": null
                },
                {
                    "id": 3573,
                    "season_number": 1,
                    "name": "Season 1",
                    "episode_count": 7,
                    "air_date": "2008-01-20",
                    "poster_path": "/poster_s1.jpg"
                }
            ],
            "vote_average": 8.9,
            "external_ids": {
                "imdb_id": "tt0903747"
            }
        }"#;
        let details: TmdbTvDetails = serde_json::from_str(json).unwrap();
        assert_eq!(details.number_of_seasons, Some(5));
        let seasons = details.seasons.unwrap();
        assert_eq!(seasons.len(), 2);
        assert_eq!(seasons[1].season_number, 1);
        assert_eq!(seasons[1].episode_count, Some(7));
        assert_eq!(
            details.external_ids.unwrap().imdb_id,
            Some("tt0903747".to_string())
        );
    }

    #[test]
    fn test_deserialize_movie_details_with_collection() {
        let json = r#"{
            "id": 671,
            "title": "Harry Potter and the Philosopher's Stone",
            "original_title": "Harry Potter and the Philosopher's Stone",
            "overview": "A boy discovers he is a wizard...",
            "poster_path": "/poster.jpg",
            "release_date": "2001-11-16",
            "imdb_id": "tt0241527",
            "vote_average": 7.9,
            "runtime": 152,
            "belongs_to_collection": {
                "id": 1241,
                "name": "Harry Potter Collection",
                "poster_path": "/collection_poster.jpg"
            }
        }"#;
        let details: TmdbMovieDetails = serde_json::from_str(json).unwrap();
        assert_eq!(details.id, 671);
        let collection = details.belongs_to_collection.unwrap();
        assert_eq!(collection.id, 1241);
        assert_eq!(collection.name, "Harry Potter Collection");
        assert_eq!(
            collection.poster_path,
            Some("/collection_poster.jpg".to_string())
        );
    }

    #[test]
    fn test_deserialize_collection_details_with_parts() {
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
                    "poster_path": "/hp2.jpg",
                    "overview": "Second movie..."
                }
            ]
        }"#;
        let collection: TmdbCollectionDetails = serde_json::from_str(json).unwrap();
        assert_eq!(collection.id, 1241);
        assert_eq!(collection.name, "Harry Potter Collection");
        assert_eq!(collection.parts.len(), 2);
        assert_eq!(collection.parts[0].id, 671);
        assert_eq!(
            collection.parts[1].title,
            "Harry Potter and the Chamber of Secrets"
        );
    }

    #[test]
    fn test_deserialize_season_details() {
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
                },
                {
                    "id": 62086,
                    "episode_number": 2,
                    "name": "Cat's in the Bag...",
                    "air_date": "2008-01-27",
                    "overview": "Walt and Jesse..."
                }
            ]
        }"#;
        let season: TmdbSeasonDetails = serde_json::from_str(json).unwrap();
        assert_eq!(season.season_number, 1);
        let episodes = season.episodes.unwrap();
        assert_eq!(episodes.len(), 2);
        assert_eq!(episodes[0].name, Some("Pilot".to_string()));
    }
}
