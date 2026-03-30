pub mod models;
pub mod queries;

use anyhow::{Context, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;

pub type DbPool = Pool<SqliteConnectionManager>;

/// Customizer that sets PRAGMA foreign_keys=ON on every new connection.
/// SQLite PRAGMAs are per-connection, so this must run on each connection in the pool.
#[derive(Debug)]
struct ForeignKeyCustomizer;

impl r2d2::CustomizeConnection<Connection, rusqlite::Error> for ForeignKeyCustomizer {
    fn on_acquire(&self, conn: &mut Connection) -> std::result::Result<(), rusqlite::Error> {
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")?;
        Ok(())
    }
}

pub fn init_pool(db_path: &str) -> Result<DbPool> {
    let manager = SqliteConnectionManager::file(db_path);
    let pool = Pool::builder()
        .max_size(4)
        .connection_customizer(Box::new(ForeignKeyCustomizer))
        .build(manager)
        .with_context(|| format!("Failed to create database pool for {db_path}"))?;

    let conn = pool.get().with_context(|| "Failed to get DB connection")?;
    conn.execute_batch("PRAGMA journal_mode=WAL;")
        .with_context(|| "Failed to set journal_mode")?;

    run_migrations(&conn)?;

    Ok(pool)
}

pub fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS media (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            media_type      TEXT NOT NULL CHECK(media_type IN ('movie', 'series', 'anime')),
            title           TEXT NOT NULL,
            title_original  TEXT,
            year            INTEGER,
            tmdb_id         INTEGER,
            imdb_id         TEXT,
            kinopoisk_url   TEXT,
            world_art_url   TEXT,
            poster_url      TEXT,
            overview        TEXT,
            anilist_id      INTEGER,
            status          TEXT NOT NULL DEFAULT 'tracking',
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE UNIQUE INDEX IF NOT EXISTS idx_media_tmdb_id_type
            ON media(tmdb_id, media_type) WHERE tmdb_id IS NOT NULL;

        CREATE UNIQUE INDEX IF NOT EXISTS idx_media_anilist_id
            ON media(anilist_id) WHERE anilist_id IS NOT NULL;

        CREATE TABLE IF NOT EXISTS seasons (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            media_id        INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
            season_number   INTEGER NOT NULL,
            title           TEXT,
            episode_count   INTEGER,
            anilist_id      INTEGER,
            format          TEXT,
            status          TEXT NOT NULL DEFAULT 'tracking',
            created_at      TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(media_id, season_number)
        );

        CREATE TABLE IF NOT EXISTS episodes (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            season_id       INTEGER NOT NULL REFERENCES seasons(id) ON DELETE CASCADE,
            episode_number  INTEGER NOT NULL,
            title           TEXT,
            air_date        TEXT,
            downloaded      INTEGER NOT NULL DEFAULT 0,
            file_path       TEXT,
            UNIQUE(season_id, episode_number)
        );

        CREATE TABLE IF NOT EXISTS torrents (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            media_id            INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
            rutracker_topic_id  TEXT NOT NULL,
            title               TEXT NOT NULL,
            quality             TEXT,
            size_bytes          INTEGER,
            seeders             INTEGER,
            season_number       INTEGER,
            episode_info        TEXT,
            registered_at       TEXT,
            last_checked_at     TEXT,
            torrent_hash        TEXT,
            qbt_hash            TEXT,
            status              TEXT NOT NULL DEFAULT 'active',
            auto_update         INTEGER NOT NULL DEFAULT 1,
            created_at          TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at          TEXT NOT NULL DEFAULT (datetime('now')),
            UNIQUE(media_id, rutracker_topic_id)
        );

        CREATE TABLE IF NOT EXISTS notifications (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            media_id            INTEGER REFERENCES media(id) ON DELETE CASCADE,
            message             TEXT NOT NULL,
            notification_type   TEXT NOT NULL,
            read                INTEGER NOT NULL DEFAULT 0,
            created_at          TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS captcha_requests (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            image_data          BLOB,
            telegram_message_id INTEGER,
            solution            TEXT,
            status              TEXT NOT NULL DEFAULT 'pending',
            created_at          TEXT NOT NULL DEFAULT (datetime('now'))
        );
        ",
    )
    .with_context(|| "Failed to run migrations")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_pool_in_memory() {
        let pool = init_pool(":memory:").unwrap();
        let conn = pool.get().unwrap();

        // Verify all tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert!(tables.contains(&"media".to_string()));
        assert!(tables.contains(&"seasons".to_string()));
        assert!(tables.contains(&"episodes".to_string()));
        assert!(tables.contains(&"torrents".to_string()));
        assert!(tables.contains(&"notifications".to_string()));
        assert!(tables.contains(&"captcha_requests".to_string()));
    }

    #[test]
    fn test_migrations_idempotent() {
        let pool = init_pool(":memory:").unwrap();
        let conn = pool.get().unwrap();
        // Running migrations again should not fail
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn test_foreign_keys_enabled() {
        let pool = init_pool(":memory:").unwrap();
        let conn = pool.get().unwrap();
        let fk: i64 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }
}
