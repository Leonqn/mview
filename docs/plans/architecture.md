# mview — Media Manager for RuTracker + Plex

## Context

A simplified alternative to Sonarr/Radarr focused on RuTracker. The app monitors torrent distributions, auto-updates torrents when new episodes appear, organizes files for Plex-compatible directory structure, provides a web UI via HTMX, and sends notifications through Telegram.

## Tech Stack

- **Rust** (edition 2024), **tokio** async runtime
- **axum** — web server
- **minijinja** — templating (HTMX partials)
- **reqwest** (cookie_store) — HTTP client for RuTracker and external APIs
- **scraper** — HTML parsing for RuTracker pages
- **rusqlite** (bundled) + **r2d2_sqlite** — SQLite with connection pool
- **teloxide** — Telegram bot
- **serde** + **toml** — config
- **tracing** — structured logging
- **anyhow** — error handling
- **tower-http** (ServeDir) — static file serving (axum has no built-in static file handler)
- **Pico CSS** — classless CSS framework for UI

## Directory Structure

```
src/
  main.rs                 -- entry point: config, DB, tasks, axum server
  config.rs               -- Config struct (TOML deserialization)
  db/
    mod.rs                -- connection pool, migrations
    models.rs             -- data structs (Media, Season, Episode, Torrent, Notification)
    queries.rs            -- SQL queries as functions
  rutracker/
    mod.rs
    client.rs             -- login, session, cookies, captcha detection
    search.rs             -- search, parse results page
    topic.rs              -- parse topic page (title, size, seeders, quality, episodes)
    download.rs           -- download .torrent file
    monitor.rs            -- check for distribution updates
  qbittorrent/
    mod.rs
    client.rs             -- Web API: login, add torrent, progress, files
  tmdb/
    mod.rs
    client.rs             -- search, movie/series details, seasons, episodes
    models.rs             -- TMDB API response structs
  plex/
    mod.rs
    client.rs             -- trigger library scan
    organizer.rs          -- rename/organize files for Plex
  telegram/
    mod.rs
    bot.rs                -- teloxide setup, commands
    captcha.rs            -- send captcha image, receive solution
    notifications.rs      -- new season/episode notifications
  web/
    mod.rs                -- axum Router, state, template engine
    routes/
      dashboard.rs        -- GET / — all tracked media
      search.rs           -- GET/POST /search — RuTracker + TMDB search
      series.rs           -- GET /series/:id — series page with seasons
      movies.rs           -- GET /movies/:id — movie page
      api.rs              -- HTMX endpoints (track, ignore, download, etc.)
      settings.rs         -- GET/POST /settings
  tasks/
    mod.rs                -- spawn background tasks
    rutracker_check.rs    -- periodic torrent update check (30 min)
    tmdb_check.rs         -- check for new seasons on TMDB (6 hours)
    download_monitor.rs   -- monitor qBittorrent download completion (60 sec)
  error.rs
templates/
  base.html               -- layout with Pico CSS + HTMX
  dashboard.html
  search.html
  search_results.html     -- HTMX partial
  series.html             -- series page with all seasons
  movie.html
  settings.html
  partials/
    season.html           -- season block with episodes
    torrent_row.html      -- single search result row
    notification.html     -- toast notification
static/
  style.css
  htmx.min.js             -- vendored
config.example.toml
```

## Database Schema

```sql
CREATE TABLE media (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    media_type      TEXT NOT NULL CHECK(media_type IN ('movie', 'series')),
    title           TEXT NOT NULL,
    title_original  TEXT,
    year            INTEGER,
    tmdb_id         INTEGER,
    imdb_id         TEXT,
    kinopoisk_url   TEXT,
    world_art_url   TEXT,
    poster_url      TEXT,
    overview        TEXT,
    status          TEXT NOT NULL DEFAULT 'tracking',
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE seasons (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id        INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    season_number   INTEGER NOT NULL,
    title           TEXT,
    episode_count   INTEGER,
    status          TEXT NOT NULL DEFAULT 'tracking', -- 'tracking', 'ignored', 'completed'
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(media_id, season_number)
);

CREATE TABLE episodes (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    season_id       INTEGER NOT NULL REFERENCES seasons(id) ON DELETE CASCADE,
    episode_number  INTEGER NOT NULL,
    title           TEXT,
    air_date        TEXT,
    downloaded      INTEGER NOT NULL DEFAULT 0,
    file_path       TEXT,
    UNIQUE(season_id, episode_number)
);

CREATE TABLE torrents (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id            INTEGER NOT NULL REFERENCES media(id) ON DELETE CASCADE,
    rutracker_topic_id  TEXT NOT NULL,
    title               TEXT NOT NULL,
    quality             TEXT,
    size_bytes          INTEGER,
    seeders             INTEGER,
    season_number       INTEGER,
    episode_info        TEXT,        -- "1-12", "1-24", NULL
    registered_at       TEXT,
    last_checked_at     TEXT,
    torrent_hash        TEXT,
    qbt_hash            TEXT,
    status              TEXT NOT NULL DEFAULT 'active',
    auto_update         INTEGER NOT NULL DEFAULT 1,
    created_at          TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at          TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE notifications (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    media_id            INTEGER REFERENCES media(id) ON DELETE CASCADE,
    message             TEXT NOT NULL,
    notification_type   TEXT NOT NULL,
    read                INTEGER NOT NULL DEFAULT 0,
    created_at          TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE captcha_requests (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    image_data          BLOB,
    telegram_message_id INTEGER,
    solution            TEXT,
    status              TEXT NOT NULL DEFAULT 'pending',
    created_at          TEXT NOT NULL DEFAULT (datetime('now'))
);
```

## Key Architecture Decisions

- **RuTracker session**: reqwest::Client with cookie_store(true). Login on startup, automatic re-login on redirect to login page. Captcha -> Telegram -> oneshot channel with 5 min timeout.
- **SQLite**: r2d2_sqlite connection pool (4 connections). DB calls wrapped in spawn_blocking to avoid blocking async runtime.
- **Background tasks**: tokio::spawn + tokio::time::interval. Shared Arc<AppState>.
- **New seasons**: NOT auto-downloaded — notification only (UI + Telegram).
- **HTMX**: hx-boost for navigation, hx-post + hx-swap for interactive elements, polling every 30s for notifications.
- **tower-http**: axum has no built-in static file serving. ServeDir from tower-http is the standard approach: `.nest_service("/static", ServeDir::new("static"))`.

### Download and File Organization Flow

```
1. User clicks "Download" in UI
2. mview downloads .torrent file from RuTracker
3. mview sends .torrent to qBittorrent via API with:
   - save_path = download_dir from config (e.g. /data/downloads/)
   - category = "mview"
4. qBittorrent downloads files to /data/downloads/

--- after completion ---

5. tasks/download_monitor.rs polls qBittorrent API every 60 sec:
   GET /api/v2/torrents/info?category=mview
   Checks `progress` field (1.0 = done) and `state` ("uploading" = downloaded, seeding)
6. When torrent is complete:
   a. Get file list via qBittorrent API
   b. plex::organizer creates hardlinks from /data/downloads/... to Plex media library:
      - Movies: /media/movies/Title (Year)/Title (Year).ext
      - TV: /media/tv/Title/Season XX/Title - SXXEXX - Episode Name.ext
   c. Hardlinks allow qBittorrent to keep seeding from the original location
   d. If hardlink fails (cross-filesystem) — fall back to file copy
   e. Update episodes.downloaded = 1, episodes.file_path in DB
   f. Trigger Plex API library scan
   g. Create notification + send to Telegram
7. torrents.qbt_hash in DB stores qBittorrent torrent hash for tracking

--- when distribution is updated (new episodes) ---

8. rutracker_check detects the distribution was updated
9. Downloads new .torrent, sends to qBittorrent
10. qBittorrent downloads only new files (content deduplication)
11. download_monitor detects completion, organizes new files
```

**Config directories:**
```toml
[paths]
download_dir = "/data/downloads"    # where qBittorrent downloads
movies_dir = "/media/movies"        # Plex Movies library
tv_dir = "/media/tv"                # Plex TV library
```

## Web Routes

| Method | Path | Description |
|--------|------|-------------|
| GET | `/` | Dashboard — all tracked media with status |
| GET/POST | `/search` | Search RuTracker + TMDB |
| GET | `/series/:id` | Series page (seasons, episodes, torrents, external links) |
| GET | `/movies/:id` | Movie page |
| GET/POST | `/settings` | Settings |
| POST | `/api/media/track` | Add media to tracking |
| POST | `/api/media/:id/delete` | Remove from tracking |
| POST | `/api/seasons/:id/status` | Change season status (tracking/ignored) |
| POST | `/api/torrents/:id/download` | Download torrent -> qBittorrent |
| POST | `/api/torrents/:id/update` | Force update check |
| GET | `/api/notifications` | HTMX partial: unread notifications |
| POST | `/api/notifications/:id/read` | Mark notification as read |

## Implementation Phases

### Phase 1: Foundation
Config loading, SQLite + migrations, axum server with empty dashboard.
- `Cargo.toml`, `config.rs`, `db/`, `web/mod.rs`, `templates/base.html`, `main.rs`

### Phase 2: RuTracker Client
Login, search, parse results. Search from web UI.
- `rutracker/client.rs`, `search.rs`, `topic.rs`, `web/routes/search.rs`

### Phase 3: TMDB Integration
Metadata search, enrich results. "Track" button.
- `tmdb/client.rs`, `tmdb/models.rs`, dashboard with posters

### Phase 4: qBittorrent + Download
Download .torrent, add to qBittorrent.
- `rutracker/download.rs`, `qbittorrent/client.rs`, Download button

### Phase 5: Series Detail Page
Series page: seasons, episodes, ignore toggle, external links (Kinopoisk, TMDB, IMDB, World Art).
- `web/routes/series.rs`, `templates/series.html`, `partials/season.html`

### Phase 6: File Organization + Plex
Organize completed downloads into Plex structure, trigger Plex scan.
- `plex/organizer.rs`, `plex/client.rs`, `tasks/download_monitor.rs`

### Phase 7: Background Monitoring
Periodic torrent update checks and TMDB new season checks, auto-update in qBittorrent.
- `rutracker/monitor.rs`, `tasks/rutracker_check.rs`, `tasks/tmdb_check.rs`

### Phase 8: Telegram Bot
Notifications, captcha solving, status commands.
- `telegram/bot.rs`, `captcha.rs`, `notifications.rs`

### Phase 9: Polish
Notifications UI, movie page, settings page, graceful shutdown.

## Verification

1. `cargo build` — compiles without errors
2. `cargo run` — server starts, dashboard opens in browser
3. RuTracker search from UI returns results
4. Adding a series — appears on dashboard with poster
5. Downloading a torrent — appears in qBittorrent
6. After completion — files organized into Plex structure
7. Telegram bot responds to /status, sends notifications
