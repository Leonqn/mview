use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use super::models::*;

// --- Media ---

fn row_to_media(row: &rusqlite::Row) -> rusqlite::Result<Media> {
    Ok(Media {
        id: row.get(0)?,
        media_type: row.get(1)?,
        title: row.get(2)?,
        title_original: row.get(3)?,
        year: row.get(4)?,
        tmdb_id: row.get(5)?,
        imdb_id: row.get(6)?,
        kinopoisk_url: row.get(7)?,
        world_art_url: row.get(8)?,
        poster_url: row.get(9)?,
        overview: row.get(10)?,
        anilist_id: row.get(11)?,
        status: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

pub fn insert_media(conn: &Connection, media: &Media) -> Result<i64> {
    conn.execute(
        "INSERT INTO media (media_type, title, title_original, year, tmdb_id, imdb_id, kinopoisk_url, world_art_url, poster_url, overview, anilist_id, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            media.media_type,
            media.title,
            media.title_original,
            media.year,
            media.tmdb_id,
            media.imdb_id,
            media.kinopoisk_url,
            media.world_art_url,
            media.poster_url,
            media.overview,
            media.anilist_id,
            media.status,
        ],
    )
    .with_context(|| "Failed to insert media")?;
    Ok(conn.last_insert_rowid())
}

pub fn get_media(conn: &Connection, id: i64) -> Result<Option<Media>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_type, title, title_original, year, tmdb_id, imdb_id, kinopoisk_url, world_art_url, poster_url, overview, anilist_id, status, created_at, updated_at
             FROM media WHERE id = ?1",
        )
        .with_context(|| "Failed to prepare get_media")?;

    let result = stmt
        .query_row(params![id], row_to_media)
        .optional()
        .with_context(|| "Failed to get media")?;

    Ok(result)
}

pub fn get_media_by_tmdb_id(
    conn: &Connection,
    tmdb_id: i64,
    media_type: &str,
) -> Result<Option<Media>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_type, title, title_original, year, tmdb_id, imdb_id, kinopoisk_url, world_art_url, poster_url, overview, anilist_id, status, created_at, updated_at
             FROM media WHERE tmdb_id = ?1 AND media_type = ?2",
        )
        .with_context(|| "Failed to prepare get_media_by_tmdb_id")?;

    let result = stmt
        .query_row(params![tmdb_id, media_type], row_to_media)
        .optional()
        .with_context(|| "Failed to get media by tmdb_id")?;

    Ok(result)
}

pub fn get_media_by_anilist_id(conn: &Connection, anilist_id: i64) -> Result<Option<Media>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_type, title, title_original, year, tmdb_id, imdb_id, kinopoisk_url, world_art_url, poster_url, overview, anilist_id, status, created_at, updated_at
             FROM media WHERE anilist_id = ?1",
        )
        .with_context(|| "Failed to prepare get_media_by_anilist_id")?;

    let result = stmt
        .query_row(params![anilist_id], row_to_media)
        .optional()
        .with_context(|| "Failed to get media by anilist_id")?;

    Ok(result)
}

pub fn find_media_by_title(conn: &Connection, title: &str) -> Result<Option<Media>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_type, title, title_original, year, tmdb_id, imdb_id, kinopoisk_url, world_art_url, poster_url, overview, anilist_id, status, created_at, updated_at
             FROM media WHERE title = ?1 COLLATE NOCASE OR title_original = ?1 COLLATE NOCASE LIMIT 1",
        )
        .with_context(|| "Failed to prepare find_media_by_title")?;

    let result = stmt
        .query_row(params![title], row_to_media)
        .optional()
        .with_context(|| "Failed to find media by title")?;

    Ok(result)
}

pub fn delete_seasons_for_media(conn: &Connection, media_id: i64) -> Result<()> {
    conn.execute("DELETE FROM seasons WHERE media_id = ?1", params![media_id])
        .with_context(|| "Failed to delete seasons for media")?;
    Ok(())
}

pub fn update_media_anilist(
    conn: &Connection,
    id: i64,
    anilist_id: i64,
    media_type: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE media SET anilist_id = ?1, media_type = ?2, updated_at = datetime('now') WHERE id = ?3",
        params![anilist_id, media_type, id],
    )
    .with_context(|| "Failed to update media with anilist info")?;
    Ok(())
}

pub fn get_all_media(conn: &Connection) -> Result<Vec<Media>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_type, title, title_original, year, tmdb_id, imdb_id, kinopoisk_url, world_art_url, poster_url, overview, anilist_id, status, created_at, updated_at
             FROM media ORDER BY created_at DESC",
        )
        .with_context(|| "Failed to prepare get_all_media")?;

    let rows = stmt
        .query_map([], row_to_media)
        .with_context(|| "Failed to get all media")?;

    rows.collect::<Result<Vec<_>, _>>()
        .with_context(|| "Failed to collect media rows")
}

pub fn delete_media(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM media WHERE id = ?1", params![id])
        .with_context(|| "Failed to delete media")?;
    Ok(())
}

// --- Seasons ---

fn row_to_season(row: &rusqlite::Row) -> rusqlite::Result<Season> {
    Ok(Season {
        id: row.get(0)?,
        media_id: row.get(1)?,
        season_number: row.get(2)?,
        title: row.get(3)?,
        episode_count: row.get(4)?,
        anilist_id: row.get(5)?,
        format: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
    })
}

pub fn insert_season(conn: &Connection, season: &Season) -> Result<i64> {
    conn.execute(
        "INSERT INTO seasons (media_id, season_number, title, episode_count, anilist_id, format, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            season.media_id,
            season.season_number,
            season.title,
            season.episode_count,
            season.anilist_id,
            season.format,
            season.status,
        ],
    )
    .with_context(|| "Failed to insert season")?;
    Ok(conn.last_insert_rowid())
}

pub fn get_seasons_for_media(conn: &Connection, media_id: i64) -> Result<Vec<Season>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_id, season_number, title, episode_count, anilist_id, format, status, created_at
             FROM seasons WHERE media_id = ?1 ORDER BY season_number",
        )
        .with_context(|| "Failed to prepare get_seasons_for_media")?;

    let rows = stmt
        .query_map(params![media_id], row_to_season)
        .with_context(|| "Failed to get seasons")?;

    rows.collect::<Result<Vec<_>, _>>()
        .with_context(|| "Failed to collect season rows")
}

pub fn get_season(conn: &Connection, id: i64) -> Result<Option<Season>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_id, season_number, title, episode_count, anilist_id, format, status, created_at
             FROM seasons WHERE id = ?1",
        )
        .with_context(|| "Failed to prepare get_season")?;

    let result = stmt
        .query_row(params![id], row_to_season)
        .optional()
        .with_context(|| "Failed to get season")?;

    Ok(result)
}

pub fn get_tracking_seasons_for_media(conn: &Connection, media_id: i64) -> Result<Vec<Season>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_id, season_number, title, episode_count, anilist_id, format, status, created_at
             FROM seasons WHERE media_id = ?1 AND status = 'tracking' ORDER BY season_number",
        )
        .with_context(|| "Failed to prepare get_tracking_seasons_for_media")?;

    let rows = stmt
        .query_map(params![media_id], row_to_season)
        .with_context(|| "Failed to get tracking seasons")?;

    rows.collect::<Result<Vec<_>, _>>()
        .with_context(|| "Failed to collect tracking season rows")
}

pub fn check_and_complete_season(conn: &Connection, season_id: i64) -> Result<bool> {
    let episodes = get_episodes_for_season(conn, season_id)?;
    if episodes.is_empty() {
        return Ok(false);
    }

    let all_downloaded = episodes.iter().all(|ep| ep.downloaded);
    if !all_downloaded {
        return Ok(false);
    }

    let today: String = conn.query_row("SELECT date('now')", [], |row| row.get(0))?;
    let all_aired = episodes.iter().all(|ep| {
        ep.air_date
            .as_ref()
            .map(|d| d.as_str() <= today.as_str())
            .unwrap_or(false)
    });

    if all_aired {
        update_season_status(conn, season_id, "completed")?;
        return Ok(true);
    }
    Ok(false)
}

pub fn update_season_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE seasons SET status = ?1 WHERE id = ?2",
        params![status, id],
    )
    .with_context(|| "Failed to update season status")?;
    Ok(())
}

// --- Episodes ---

pub fn insert_episode(conn: &Connection, episode: &Episode) -> Result<i64> {
    conn.execute(
        "INSERT INTO episodes (season_id, episode_number, title, air_date, downloaded, file_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            episode.season_id,
            episode.episode_number,
            episode.title,
            episode.air_date,
            episode.downloaded,
            episode.file_path,
        ],
    )
    .with_context(|| "Failed to insert episode")?;
    Ok(conn.last_insert_rowid())
}

pub fn get_episodes_for_season(conn: &Connection, season_id: i64) -> Result<Vec<Episode>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, season_id, episode_number, title, air_date, downloaded, file_path
             FROM episodes WHERE season_id = ?1 ORDER BY episode_number",
        )
        .with_context(|| "Failed to prepare get_episodes_for_season")?;

    let rows = stmt
        .query_map(params![season_id], |row| {
            Ok(Episode {
                id: row.get(0)?,
                season_id: row.get(1)?,
                episode_number: row.get(2)?,
                title: row.get(3)?,
                air_date: row.get(4)?,
                downloaded: row.get(5)?,
                file_path: row.get(6)?,
            })
        })
        .with_context(|| "Failed to get episodes")?;

    rows.collect::<Result<Vec<_>, _>>()
        .with_context(|| "Failed to collect episode rows")
}

pub fn update_episode_downloaded(
    conn: &Connection,
    id: i64,
    downloaded: bool,
    file_path: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE episodes SET downloaded = ?1, file_path = ?2 WHERE id = ?3",
        params![downloaded, file_path, id],
    )
    .with_context(|| "Failed to update episode downloaded")?;
    Ok(())
}

// --- Torrents ---

fn row_to_torrent(row: &rusqlite::Row) -> rusqlite::Result<Torrent> {
    Ok(Torrent {
        id: row.get(0)?,
        media_id: row.get(1)?,
        rutracker_topic_id: row.get(2)?,
        title: row.get(3)?,
        quality: row.get(4)?,
        size_bytes: row.get(5)?,
        seeders: row.get(6)?,
        season_number: row.get(7)?,
        episode_info: row.get(8)?,
        registered_at: row.get(9)?,
        last_checked_at: row.get(10)?,
        torrent_hash: row.get(11)?,
        qbt_hash: row.get(12)?,
        status: row.get(13)?,
        auto_update: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

pub fn insert_torrent(conn: &Connection, torrent: &Torrent) -> Result<i64> {
    conn.execute(
        "INSERT INTO torrents (media_id, rutracker_topic_id, title, quality, size_bytes, seeders, season_number, episode_info, registered_at, torrent_hash, qbt_hash, status, auto_update)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            torrent.media_id,
            torrent.rutracker_topic_id,
            torrent.title,
            torrent.quality,
            torrent.size_bytes,
            torrent.seeders,
            torrent.season_number,
            torrent.episode_info,
            torrent.registered_at,
            torrent.torrent_hash,
            torrent.qbt_hash,
            torrent.status,
            torrent.auto_update,
        ],
    )
    .with_context(|| "Failed to insert torrent")?;
    Ok(conn.last_insert_rowid())
}

pub fn get_torrent(conn: &Connection, id: i64) -> Result<Option<Torrent>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_id, rutracker_topic_id, title, quality, size_bytes, seeders, season_number, episode_info, registered_at, last_checked_at, torrent_hash, qbt_hash, status, auto_update, created_at, updated_at
             FROM torrents WHERE id = ?1",
        )
        .with_context(|| "Failed to prepare get_torrent")?;

    let result = stmt
        .query_row(params![id], row_to_torrent)
        .optional()
        .with_context(|| "Failed to get torrent")?;

    Ok(result)
}

pub fn get_torrents_for_media(conn: &Connection, media_id: i64) -> Result<Vec<Torrent>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_id, rutracker_topic_id, title, quality, size_bytes, seeders, season_number, episode_info, registered_at, last_checked_at, torrent_hash, qbt_hash, status, auto_update, created_at, updated_at
             FROM torrents WHERE media_id = ?1 ORDER BY created_at DESC",
        )
        .with_context(|| "Failed to prepare get_torrents_for_media")?;

    let rows = stmt
        .query_map(params![media_id], row_to_torrent)
        .with_context(|| "Failed to get torrents")?;

    rows.collect::<Result<Vec<_>, _>>()
        .with_context(|| "Failed to collect torrent rows")
}

pub fn get_auto_update_torrents(conn: &Connection) -> Result<Vec<Torrent>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_id, rutracker_topic_id, title, quality, size_bytes, seeders, season_number, episode_info, registered_at, last_checked_at, torrent_hash, qbt_hash, status, auto_update, created_at, updated_at
             FROM torrents WHERE status IN ('active', 'completed') AND auto_update = 1",
        )
        .with_context(|| "Failed to prepare get_auto_update_torrents")?;

    let rows = stmt
        .query_map([], row_to_torrent)
        .with_context(|| "Failed to get active auto-update torrents")?;

    rows.collect::<Result<Vec<_>, _>>()
        .with_context(|| "Failed to collect torrent rows")
}

pub fn update_torrent_auto_update(conn: &Connection, id: i64, auto_update: bool) -> Result<()> {
    conn.execute(
        "UPDATE torrents SET auto_update = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![auto_update, id],
    )
    .with_context(|| "Failed to update torrent auto_update")?;
    Ok(())
}

pub fn update_torrent_checked(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE torrents SET last_checked_at = datetime('now'), updated_at = datetime('now') WHERE id = ?1",
        params![id],
    )
    .with_context(|| "Failed to update torrent last_checked_at")?;
    Ok(())
}

pub fn update_torrent_qbt_hash(conn: &Connection, id: i64, qbt_hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE torrents SET qbt_hash = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![qbt_hash, id],
    )
    .with_context(|| "Failed to update torrent qbt_hash")?;
    Ok(())
}

pub fn update_torrent_registered_at(conn: &Connection, id: i64, registered_at: &str) -> Result<()> {
    conn.execute(
        "UPDATE torrents SET registered_at = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![registered_at, id],
    )
    .with_context(|| "Failed to update torrent registered_at")?;
    Ok(())
}

pub fn update_torrent_hash(conn: &Connection, id: i64, torrent_hash: &str) -> Result<()> {
    conn.execute(
        "UPDATE torrents SET torrent_hash = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![torrent_hash, id],
    )
    .with_context(|| "Failed to update torrent_hash")?;
    Ok(())
}

pub fn update_torrent_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE torrents SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
        params![status, id],
    )
    .with_context(|| "Failed to update torrent status")?;
    Ok(())
}

pub fn delete_torrent(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM torrents WHERE id = ?1", params![id])
        .with_context(|| "Failed to delete torrent")?;
    Ok(())
}

pub fn get_active_torrents_with_qbt_hash(conn: &Connection) -> Result<Vec<Torrent>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_id, rutracker_topic_id, title, quality, size_bytes, seeders, season_number, episode_info, registered_at, last_checked_at, torrent_hash, qbt_hash, status, auto_update, created_at, updated_at
             FROM torrents WHERE status = 'active' AND qbt_hash IS NOT NULL",
        )
        .with_context(|| "Failed to prepare get_active_torrents_with_qbt_hash")?;

    let rows = stmt
        .query_map([], row_to_torrent)
        .with_context(|| "Failed to get active torrents with qbt_hash")?;

    rows.collect::<Result<Vec<_>, _>>()
        .with_context(|| "Failed to collect torrent rows")
}

// --- Notifications ---

pub fn insert_notification(conn: &Connection, notification: &Notification) -> Result<i64> {
    conn.execute(
        "INSERT INTO notifications (media_id, message, notification_type, read)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            notification.media_id,
            notification.message,
            notification.notification_type,
            notification.read,
        ],
    )
    .with_context(|| "Failed to insert notification")?;
    Ok(conn.last_insert_rowid())
}

pub fn get_unread_notifications(conn: &Connection) -> Result<Vec<Notification>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, media_id, message, notification_type, read, created_at
             FROM notifications WHERE read = 0 ORDER BY created_at DESC LIMIT 50",
        )
        .with_context(|| "Failed to prepare get_unread_notifications")?;

    let rows = stmt
        .query_map([], |row| {
            Ok(Notification {
                id: row.get(0)?,
                media_id: row.get(1)?,
                message: row.get(2)?,
                notification_type: row.get(3)?,
                read: row.get(4)?,
                created_at: row.get(5)?,
            })
        })
        .with_context(|| "Failed to get unread notifications")?;

    rows.collect::<Result<Vec<_>, _>>()
        .with_context(|| "Failed to collect notification rows")
}

pub fn mark_notification_read(conn: &Connection, id: i64) -> Result<()> {
    conn.execute(
        "UPDATE notifications SET read = 1 WHERE id = ?1",
        params![id],
    )
    .with_context(|| "Failed to mark notification read")?;
    Ok(())
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn update_media_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
        conn.execute(
            "UPDATE media SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
            params![status, id],
        )
        .with_context(|| "Failed to update media status")?;
        Ok(())
    }

    fn delete_torrent(conn: &Connection, id: i64) -> Result<()> {
        conn.execute("DELETE FROM torrents WHERE id = ?1", params![id])
            .with_context(|| "Failed to delete torrent")?;
        Ok(())
    }

    fn insert_captcha_request(conn: &Connection, image_data: Option<&[u8]>) -> Result<i64> {
        conn.execute(
            "INSERT INTO captcha_requests (image_data) VALUES (?1)",
            params![image_data],
        )
        .with_context(|| "Failed to insert captcha request")?;
        Ok(conn.last_insert_rowid())
    }

    fn update_captcha_solution(
        conn: &Connection,
        id: i64,
        solution: &str,
        telegram_message_id: Option<i64>,
    ) -> Result<()> {
        conn.execute(
            "UPDATE captcha_requests SET solution = ?1, telegram_message_id = ?2, status = 'solved' WHERE id = ?3",
            params![solution, telegram_message_id, id],
        )
        .with_context(|| "Failed to update captcha solution")?;
        Ok(())
    }

    fn get_pending_captcha(conn: &Connection) -> Result<Option<CaptchaRequest>> {
        let mut stmt = conn
            .prepare(
                "SELECT id, image_data, telegram_message_id, solution, status, created_at
                 FROM captcha_requests WHERE status = 'pending' ORDER BY created_at DESC LIMIT 1",
            )
            .with_context(|| "Failed to prepare get_pending_captcha")?;

        let result = stmt
            .query_row([], |row| {
                Ok(CaptchaRequest {
                    id: row.get(0)?,
                    image_data: row.get(1)?,
                    telegram_message_id: row.get(2)?,
                    solution: row.get(3)?,
                    status: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .optional()
            .with_context(|| "Failed to get pending captcha")?;

        Ok(result)
    }

    fn setup_db() -> r2d2::PooledConnection<r2d2_sqlite::SqliteConnectionManager> {
        let pool = db::init_pool(":memory:").unwrap();
        pool.get().unwrap()
    }

    fn sample_media() -> Media {
        Media {
            id: 0,
            media_type: "series".to_string(),
            title: "Test Series".to_string(),
            title_original: Some("Test Original".to_string()),
            year: Some(2024),
            tmdb_id: Some(12345),
            imdb_id: Some("tt1234567".to_string()),
            kinopoisk_url: None,
            world_art_url: None,
            poster_url: Some("https://example.com/poster.jpg".to_string()),
            overview: Some("A test series".to_string()),
            anilist_id: None,
            status: "tracking".to_string(),
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn test_media_crud() {
        let conn = setup_db();
        let media = sample_media();

        // Insert
        let id = insert_media(&conn, &media).unwrap();
        assert!(id > 0);

        // Get
        let fetched = get_media(&conn, id).unwrap().unwrap();
        assert_eq!(fetched.title, "Test Series");
        assert_eq!(fetched.media_type, "series");
        assert_eq!(fetched.tmdb_id, Some(12345));

        // Get all
        let all = get_all_media(&conn).unwrap();
        assert_eq!(all.len(), 1);

        // Update status
        update_media_status(&conn, id, "completed").unwrap();
        let updated = get_media(&conn, id).unwrap().unwrap();
        assert_eq!(updated.status, "completed");

        // Delete
        delete_media(&conn, id).unwrap();
        let deleted = get_media(&conn, id).unwrap();
        assert!(deleted.is_none());
    }

    #[test]
    fn test_season_crud() {
        let conn = setup_db();
        let media = sample_media();
        let media_id = insert_media(&conn, &media).unwrap();

        let season = Season {
            id: 0,
            media_id,
            season_number: 1,
            title: Some("Season 1".to_string()),
            episode_count: Some(12),
            anilist_id: None,
            format: None,
            status: "tracking".to_string(),
            created_at: String::new(),
        };

        let season_id = insert_season(&conn, &season).unwrap();
        assert!(season_id > 0);

        let seasons = get_seasons_for_media(&conn, media_id).unwrap();
        assert_eq!(seasons.len(), 1);
        assert_eq!(seasons[0].season_number, 1);
        assert_eq!(seasons[0].episode_count, Some(12));

        update_season_status(&conn, season_id, "ignored").unwrap();
        let seasons = get_seasons_for_media(&conn, media_id).unwrap();
        assert_eq!(seasons[0].status, "ignored");
    }

    #[test]
    fn test_episode_crud() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();

        let season = Season {
            id: 0,
            media_id,
            season_number: 1,
            title: None,
            episode_count: None,
            anilist_id: None,
            format: None,
            status: "tracking".to_string(),
            created_at: String::new(),
        };
        let season_id = insert_season(&conn, &season).unwrap();

        let episode = Episode {
            id: 0,
            season_id,
            episode_number: 1,
            title: Some("Pilot".to_string()),
            air_date: Some("2024-01-15".to_string()),
            downloaded: false,
            file_path: None,
        };
        let ep_id = insert_episode(&conn, &episode).unwrap();
        assert!(ep_id > 0);

        let episodes = get_episodes_for_season(&conn, season_id).unwrap();
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].title, Some("Pilot".to_string()));
        assert!(!episodes[0].downloaded);

        update_episode_downloaded(&conn, ep_id, true, Some("/media/tv/Test/S01E01.mkv")).unwrap();
        let episodes = get_episodes_for_season(&conn, season_id).unwrap();
        assert!(episodes[0].downloaded);
        assert_eq!(
            episodes[0].file_path,
            Some("/media/tv/Test/S01E01.mkv".to_string())
        );
    }

    #[test]
    fn test_torrent_crud() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();

        let torrent = Torrent {
            id: 0,
            media_id,
            rutracker_topic_id: "123456".to_string(),
            title: "Test.Series.S01.1080p".to_string(),
            quality: Some("1080p".to_string()),
            size_bytes: Some(5_000_000_000),
            seeders: Some(42),
            season_number: Some(1),
            episode_info: Some("1-12".to_string()),
            registered_at: Some("2024-01-01".to_string()),
            last_checked_at: None,
            torrent_hash: None,
            qbt_hash: None,
            status: "active".to_string(),
            auto_update: true,
            created_at: String::new(),
            updated_at: String::new(),
        };

        let torrent_id = insert_torrent(&conn, &torrent).unwrap();
        assert!(torrent_id > 0);

        let torrents = get_torrents_for_media(&conn, media_id).unwrap();
        assert_eq!(torrents.len(), 1);
        assert_eq!(torrents[0].rutracker_topic_id, "123456");
        assert_eq!(torrents[0].size_bytes, Some(5_000_000_000));

        let auto_update = get_auto_update_torrents(&conn).unwrap();
        assert_eq!(auto_update.len(), 1);

        update_torrent_qbt_hash(&conn, torrent_id, "abc123hash").unwrap();
        update_torrent_checked(&conn, torrent_id).unwrap();

        let torrents = get_torrents_for_media(&conn, media_id).unwrap();
        assert_eq!(torrents[0].qbt_hash, Some("abc123hash".to_string()));
        assert!(torrents[0].last_checked_at.is_some());

        delete_torrent(&conn, torrent_id).unwrap();
        let torrents = get_torrents_for_media(&conn, media_id).unwrap();
        assert!(torrents.is_empty());
    }

    #[test]
    fn test_get_torrent_by_id() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();

        let torrent = Torrent {
            id: 0,
            media_id,
            rutracker_topic_id: "789".to_string(),
            title: "Test.Movie.1080p".to_string(),
            quality: Some("1080p".to_string()),
            size_bytes: Some(2_000_000_000),
            seeders: Some(20),
            season_number: None,
            episode_info: None,
            registered_at: None,
            last_checked_at: None,
            torrent_hash: None,
            qbt_hash: None,
            status: "active".to_string(),
            auto_update: false,
            created_at: String::new(),
            updated_at: String::new(),
        };

        let torrent_id = insert_torrent(&conn, &torrent).unwrap();
        let fetched = get_torrent(&conn, torrent_id).unwrap().unwrap();
        assert_eq!(fetched.rutracker_topic_id, "789");
        assert_eq!(fetched.title, "Test.Movie.1080p");
        assert_eq!(fetched.quality, Some("1080p".to_string()));

        let not_found = get_torrent(&conn, 99999).unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_notification_crud() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();

        let notification = Notification {
            id: 0,
            media_id: Some(media_id),
            message: "New episode available".to_string(),
            notification_type: "new_episode".to_string(),
            read: false,
            created_at: String::new(),
        };

        let notif_id = insert_notification(&conn, &notification).unwrap();
        assert!(notif_id > 0);

        let unread = get_unread_notifications(&conn).unwrap();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].message, "New episode available");

        mark_notification_read(&conn, notif_id).unwrap();
        let unread = get_unread_notifications(&conn).unwrap();
        assert!(unread.is_empty());
    }

    #[test]
    fn test_captcha_crud() {
        let conn = setup_db();

        let captcha_id = insert_captcha_request(&conn, Some(b"fake_image_data")).unwrap();
        assert!(captcha_id > 0);

        let pending = get_pending_captcha(&conn).unwrap().unwrap();
        assert_eq!(pending.status, "pending");
        assert_eq!(pending.image_data, Some(b"fake_image_data".to_vec()));

        update_captcha_solution(&conn, captcha_id, "ABC123", Some(999)).unwrap();
        let pending = get_pending_captcha(&conn).unwrap();
        assert!(pending.is_none()); // no longer pending
    }

    #[test]
    fn test_cascade_delete() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();

        let season = Season {
            id: 0,
            media_id,
            season_number: 1,
            title: None,
            episode_count: None,
            anilist_id: None,
            format: None,
            status: "tracking".to_string(),
            created_at: String::new(),
        };
        let season_id = insert_season(&conn, &season).unwrap();

        let episode = Episode {
            id: 0,
            season_id,
            episode_number: 1,
            title: None,
            air_date: None,
            downloaded: false,
            file_path: None,
        };
        insert_episode(&conn, &episode).unwrap();

        let torrent = Torrent {
            id: 0,
            media_id,
            rutracker_topic_id: "999".to_string(),
            title: "test".to_string(),
            quality: None,
            size_bytes: None,
            seeders: None,
            season_number: None,
            episode_info: None,
            registered_at: None,
            last_checked_at: None,
            torrent_hash: None,
            qbt_hash: None,
            status: "active".to_string(),
            auto_update: true,
            created_at: String::new(),
            updated_at: String::new(),
        };
        insert_torrent(&conn, &torrent).unwrap();

        // Delete media should cascade to seasons, episodes, torrents
        delete_media(&conn, media_id).unwrap();

        let seasons = get_seasons_for_media(&conn, media_id).unwrap();
        assert!(seasons.is_empty());

        let torrents = get_torrents_for_media(&conn, media_id).unwrap();
        assert!(torrents.is_empty());
    }

    #[test]
    fn test_get_season_by_id() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();
        let season = Season {
            id: 0,
            media_id,
            season_number: 2,
            title: Some("Season 2".to_string()),
            episode_count: Some(10),
            anilist_id: None,
            format: None,
            status: "tracking".to_string(),
            created_at: String::new(),
        };
        let season_id = insert_season(&conn, &season).unwrap();

        let fetched = get_season(&conn, season_id).unwrap().unwrap();
        assert_eq!(fetched.season_number, 2);
        assert_eq!(fetched.title, Some("Season 2".to_string()));

        let not_found = get_season(&conn, 99999).unwrap();
        assert!(not_found.is_none());
    }

    #[test]
    fn test_get_tracking_seasons_for_media() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();

        insert_season(
            &conn,
            &Season {
                id: 0,
                media_id,
                season_number: 1,
                title: None,
                episode_count: None,
                anilist_id: None,
                format: None,
                status: "tracking".to_string(),
                created_at: String::new(),
            },
        )
        .unwrap();
        insert_season(
            &conn,
            &Season {
                id: 0,
                media_id,
                season_number: 2,
                title: None,
                episode_count: None,
                anilist_id: None,
                format: None,
                status: "ignored".to_string(),
                created_at: String::new(),
            },
        )
        .unwrap();
        insert_season(
            &conn,
            &Season {
                id: 0,
                media_id,
                season_number: 3,
                title: None,
                episode_count: None,
                anilist_id: None,
                format: None,
                status: "tracking".to_string(),
                created_at: String::new(),
            },
        )
        .unwrap();

        let tracking = get_tracking_seasons_for_media(&conn, media_id).unwrap();
        assert_eq!(tracking.len(), 2);
        assert_eq!(tracking[0].season_number, 1);
        assert_eq!(tracking[1].season_number, 3);
    }

    #[test]
    fn test_check_and_complete_season_all_downloaded() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();
        let season_id = insert_season(
            &conn,
            &Season {
                id: 0,
                media_id,
                season_number: 1,
                title: None,
                episode_count: Some(2),
                anilist_id: None,
                format: None,
                status: "tracking".to_string(),
                created_at: String::new(),
            },
        )
        .unwrap();

        insert_episode(
            &conn,
            &Episode {
                id: 0,
                season_id,
                episode_number: 1,
                title: None,
                air_date: Some("2020-01-01".to_string()),
                downloaded: true,
                file_path: None,
            },
        )
        .unwrap();
        insert_episode(
            &conn,
            &Episode {
                id: 0,
                season_id,
                episode_number: 2,
                title: None,
                air_date: Some("2020-01-08".to_string()),
                downloaded: true,
                file_path: None,
            },
        )
        .unwrap();

        let completed = check_and_complete_season(&conn, season_id).unwrap();
        assert!(completed);

        let season = get_season(&conn, season_id).unwrap().unwrap();
        assert_eq!(season.status, "completed");
    }

    #[test]
    fn test_check_and_complete_season_not_all_downloaded() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();
        let season_id = insert_season(
            &conn,
            &Season {
                id: 0,
                media_id,
                season_number: 1,
                title: None,
                episode_count: Some(2),
                anilist_id: None,
                format: None,
                status: "tracking".to_string(),
                created_at: String::new(),
            },
        )
        .unwrap();

        insert_episode(
            &conn,
            &Episode {
                id: 0,
                season_id,
                episode_number: 1,
                title: None,
                air_date: Some("2020-01-01".to_string()),
                downloaded: true,
                file_path: None,
            },
        )
        .unwrap();
        insert_episode(
            &conn,
            &Episode {
                id: 0,
                season_id,
                episode_number: 2,
                title: None,
                air_date: Some("2020-01-08".to_string()),
                downloaded: false,
                file_path: None,
            },
        )
        .unwrap();

        let completed = check_and_complete_season(&conn, season_id).unwrap();
        assert!(!completed);

        let season = get_season(&conn, season_id).unwrap().unwrap();
        assert_eq!(season.status, "tracking");
    }

    #[test]
    fn test_check_and_complete_season_future_episodes() {
        let conn = setup_db();
        let media_id = insert_media(&conn, &sample_media()).unwrap();
        let season_id = insert_season(
            &conn,
            &Season {
                id: 0,
                media_id,
                season_number: 1,
                title: None,
                episode_count: Some(1),
                anilist_id: None,
                format: None,
                status: "tracking".to_string(),
                created_at: String::new(),
            },
        )
        .unwrap();

        insert_episode(
            &conn,
            &Episode {
                id: 0,
                season_id,
                episode_number: 1,
                title: None,
                air_date: Some("2099-01-01".to_string()),
                downloaded: true,
                file_path: None,
            },
        )
        .unwrap();

        let completed = check_and_complete_season(&conn, season_id).unwrap();
        assert!(!completed);
    }
}
