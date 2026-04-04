use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Media {
    pub id: i64,
    pub media_type: String,
    pub title: String,
    pub title_original: Option<String>,
    pub year: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub kinopoisk_url: Option<String>,
    pub world_art_url: Option<String>,
    pub poster_url: Option<String>,
    pub overview: Option<String>,
    pub anilist_id: Option<i64>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Season {
    pub id: i64,
    pub media_id: i64,
    pub season_number: i64,
    pub title: Option<String>,
    pub episode_count: Option<i64>,
    pub anilist_id: Option<i64>,
    pub format: Option<String>,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: i64,
    pub season_id: i64,
    pub episode_number: i64,
    pub title: Option<String>,
    pub air_date: Option<String>,
    pub downloaded: bool,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Torrent {
    pub id: i64,
    pub media_id: i64,
    pub rutracker_topic_id: String,
    pub title: String,
    pub quality: Option<String>,
    pub size_bytes: Option<i64>,
    pub seeders: Option<i64>,
    pub season_number: Option<i64>,
    pub episode_info: Option<String>,
    pub registered_at: Option<String>,
    pub last_checked_at: Option<String>,
    pub torrent_hash: Option<String>,
    pub qbt_hash: Option<String>,
    pub status: String,
    pub auto_update: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: i64,
    pub media_id: Option<i64>,
    pub message: String,
    pub notification_type: String,
    pub read: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCache {
    pub id: i64,
    pub season_id: i64,
    pub results_count: i64,
    pub last_searched_at: String,
}

#[cfg(test)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptchaRequest {
    pub id: i64,
    pub image_data: Option<Vec<u8>>,
    pub telegram_message_id: Option<i64>,
    pub solution: Option<String>,
    pub status: String,
    pub created_at: String,
}
