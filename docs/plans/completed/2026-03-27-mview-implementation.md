# mview - Media Manager Implementation Plan

## Overview

Implement mview - a media manager for RuTracker + Plex, following the 9-phase architecture defined in docs/plans/architecture.md. The app monitors torrent distributions, auto-updates torrents when new episodes appear, organizes files for Plex-compatible directory structure, provides a web UI via HTMX, and sends notifications through Telegram.

## Context

- Files involved: entire project (greenfield - only Cargo.toml and src/main.rs exist)
- Related patterns: architecture defined in docs/plans/architecture.md
- Dependencies: tokio, axum, minijinja, reqwest, scraper, rusqlite, r2d2_sqlite, teloxide, serde, toml, tracing, anyhow, tower-http, pico.css, htmx

## Development Approach

- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Follow the phase order from architecture.md - each phase builds on the previous
- Use spawn_blocking for all SQLite calls
- **CRITICAL: every task MUST include new/updated tests**
- **CRITICAL: all tests must pass before starting next task**

## Implementation Steps

### Task 1: Foundation - Config and Dependencies

**Files:**
- Modify: `Cargo.toml`
- Create: `src/config.rs`
- Create: `config.example.toml`
- Modify: `src/main.rs`

- [x] Add all dependencies to Cargo.toml: tokio (full), axum, minijinja, reqwest (cookies, json), scraper, rusqlite (bundled), r2d2_sqlite, teloxide, serde (derive), toml, tracing, tracing-subscriber, anyhow, tower-http (fs)
- [x] Create config.rs with Config struct: rutracker (url, username, password), qbittorrent (url, username, password), tmdb (api_key), plex (url, token), telegram (bot_token, chat_id), paths (download_dir, movies_dir, tv_dir), server (host, port)
- [x] Implement Config::load() that reads TOML file from path (CLI arg or default)
- [x] Create config.example.toml with all fields documented
- [x] Update main.rs to load config and init tracing
- [x] Write tests for config loading (valid config, missing fields, defaults)
- [x] Run cargo build and cargo test - must pass before task 2

### Task 2: Foundation - Database Layer

**Files:**
- Create: `src/db/mod.rs`
- Create: `src/db/models.rs`
- Create: `src/db/queries.rs`
- Modify: `src/main.rs`

- [x] Create db/mod.rs: init_pool() returning r2d2::Pool, run_migrations() with all CREATE TABLE statements from architecture
- [x] Create db/models.rs: Media, Season, Episode, Torrent, Notification, CaptchaRequest structs with serde Serialize/Deserialize
- [x] Create db/queries.rs: insert/get/update/delete functions for each model, wrapping rusqlite calls
- [x] Update main.rs to initialize DB pool on startup
- [x] Write tests for migrations (schema creation), CRUD operations for each model
- [x] Run cargo test - must pass before task 3

### Task 3: Foundation - Axum Server with Empty Dashboard

**Files:**
- Create: `src/web/mod.rs`
- Create: `src/web/routes/dashboard.rs`
- Create: `src/error.rs`
- Create: `templates/base.html`
- Create: `templates/dashboard.html`
- Create: `static/style.css`
- Modify: `src/main.rs`

- [x] Create error.rs with AppError type implementing IntoResponse
- [x] Create web/mod.rs: AppState (db pool, config), Router setup, minijinja Environment init
- [x] Create web/routes/dashboard.rs: GET / handler that renders dashboard template with empty media list
- [x] Create templates/base.html with Pico CSS CDN link, HTMX vendored script, navigation
- [x] Create templates/dashboard.html extending base, showing tracked media grid
- [x] Download htmx.min.js to static/, create static/style.css
- [x] Configure tower-http ServeDir for /static
- [x] Update main.rs: create AppState, build router, start server
- [x] Write tests for route responses (status codes, content type)
- [x] Run cargo test - must pass before task 4

### Task 4: RuTracker Client - Login and Search

**Files:**
- Create: `src/rutracker/mod.rs`
- Create: `src/rutracker/client.rs`
- Create: `src/rutracker/search.rs`
- Create: `src/rutracker/topic.rs`

- [x] Create rutracker/client.rs: RutrackerClient with reqwest::Client (cookie_store=true), login() method, auto re-login on redirect detection
- [x] Create rutracker/search.rs: search() method returning Vec of search results, parse HTML results page with scraper (title, size, seeders, topic_id)
- [x] Create rutracker/topic.rs: parse_topic() to extract detailed info (quality, episode_info, registered_at, kinopoisk/imdb/world_art links)
- [x] Write tests with mock HTML responses for parsing logic
- [x] Run cargo test - must pass before task 5

### Task 5: RuTracker Search from Web UI

**Files:**
- Create: `src/web/routes/search.rs`
- Create: `templates/search.html`
- Create: `templates/search_results.html`
- Modify: `src/web/mod.rs`

- [x] Create web/routes/search.rs: GET /search renders search form, POST /search calls RuTracker search and returns results
- [x] Create templates/search.html with search form (hx-post for HTMX submission)
- [x] Create templates/search_results.html as HTMX partial showing result rows
- [x] Add RutrackerClient to AppState, add search routes to router
- [x] Write tests for search route handlers
- [x] Run cargo test - must pass before task 6

### Task 6: TMDB Integration

**Files:**
- Create: `src/tmdb/mod.rs`
- Create: `src/tmdb/client.rs`
- Create: `src/tmdb/models.rs`
- Modify: `src/web/routes/search.rs`
- Modify: `src/web/routes/dashboard.rs`
- Modify: `templates/dashboard.html`

- [x] Create tmdb/models.rs: API response structs (SearchResult, MovieDetails, SeriesDetails, SeasonDetails)
- [x] Create tmdb/client.rs: TmdbClient with search_movie(), search_tv(), get_movie(), get_tv(), get_season() methods
- [x] Modify search route to also query TMDB and merge/enrich results with metadata and posters
- [x] Add POST /api/media/track endpoint: save media + seasons + episodes to DB from TMDB data
- [x] Update dashboard to show tracked media with posters and status
- [x] Write tests for TMDB response parsing, track endpoint
- [x] Run cargo test - must pass before task 7

### Task 7: qBittorrent Client and Download Flow

**Files:**
- Create: `src/qbittorrent/mod.rs`
- Create: `src/qbittorrent/client.rs`
- Create: `src/rutracker/download.rs`
- Create: `templates/partials/torrent_row.html`
- Modify: `src/web/routes/search.rs`

- [x] Create qbittorrent/client.rs: QbtClient with login(), add_torrent() (multipart upload with save_path and category), get_torrents(), get_torrent_files() methods
- [x] Create rutracker/download.rs: download_torrent() returning .torrent file bytes
- [x] Add POST /api/torrents/:id/download endpoint: download .torrent from RuTracker, send to qBittorrent, save torrent record in DB
- [x] Create torrent_row.html partial with download button
- [x] Write tests for qBittorrent API request construction, download flow
- [x] Run cargo test - must pass before task 8

### Task 8: Series Detail Page

**Files:**
- Create: `src/web/routes/series.rs`
- Create: `templates/series.html`
- Create: `templates/partials/season.html`
- Modify: `src/web/mod.rs`

- [x] Create web/routes/series.rs: GET /series/:id handler loading media + seasons + episodes + torrents from DB
- [x] Create templates/series.html: poster, metadata, external links (TMDB, IMDB, Kinopoisk, World Art), seasons list
- [x] Create templates/partials/season.html: episodes table with download status, ignore toggle
- [x] Add POST /api/seasons/:id/status endpoint to toggle tracking/ignored
- [x] Write tests for series page data loading, season status toggle
- [x] Run cargo test - must pass before task 9

### Task 9: File Organization and Plex Integration

**Files:**
- Create: `src/plex/mod.rs`
- Create: `src/plex/organizer.rs`
- Create: `src/plex/client.rs`

- [x] Create plex/organizer.rs: organize_files() that creates hardlinks from download dir to Plex structure (Movies: /Title (Year)/Title.ext, TV: /Title/Season XX/Title - SXXEXX.ext), with copy fallback on cross-filesystem
- [x] Create plex/client.rs: PlexClient with trigger_scan() method
- [x] Write tests for path generation logic (movie naming, TV episode naming, season folder naming)
- [x] Run cargo test - must pass before task 10

### Task 10: Background Tasks - Download Monitor

**Files:**
- Create: `src/tasks/mod.rs`
- Create: `src/tasks/download_monitor.rs`
- Modify: `src/main.rs`

- [x] Create tasks/mod.rs: spawn_tasks() that launches all background tasks with Arc<AppState>
- [x] Create tasks/download_monitor.rs: poll qBittorrent every 60s for completed mview-category torrents, run organize_files(), update DB (episodes.downloaded, file_path), trigger Plex scan
- [x] Update main.rs to spawn background tasks after server setup
- [x] Write tests for download completion detection logic, DB updates
- [x] Run cargo test - must pass before task 11

### Task 11: Background Tasks - RuTracker and TMDB Monitoring

**Files:**
- Create: `src/rutracker/monitor.rs`
- Create: `src/tasks/rutracker_check.rs`
- Create: `src/tasks/tmdb_check.rs`
- Modify: `src/web/routes/api.rs`

- [x] Create rutracker/monitor.rs: check_updates() comparing registered_at timestamps to detect distribution updates
- [x] Create tasks/rutracker_check.rs: every 30 min check all active torrents with auto_update=1, download updated .torrent and re-add to qBittorrent
- [x] Create tasks/tmdb_check.rs: every 6 hours check for new seasons via TMDB API, create notification (no auto-download)
- [x] Add POST /api/torrents/:id/update endpoint for manual update check
- [x] Write tests for update detection logic, notification creation
- [x] Run cargo test - must pass before task 12

### Task 12: Telegram Bot

**Files:**
- Create: `src/telegram/mod.rs`
- Create: `src/telegram/bot.rs`
- Create: `src/telegram/captcha.rs`
- Create: `src/telegram/notifications.rs`
- Modify: `src/main.rs`

- [x] Create telegram/bot.rs: teloxide bot setup with /status command (show tracked media count, active downloads)
- [x] Create telegram/captcha.rs: send_captcha() sends image to chat, wait for reply via oneshot channel with 5 min timeout
- [x] Create telegram/notifications.rs: send_notification() for new episodes, completed downloads, torrent updates
- [x] Integrate captcha flow into RuTracker client login
- [x] Update main.rs to start bot alongside server
- [x] Write tests for notification message formatting, command parsing
- [x] Run cargo test - must pass before task 13

### Task 13: Polish - Movies, Notifications UI, Settings

**Files:**
- Create: `src/web/routes/movies.rs`
- Create: `src/web/routes/settings.rs`
- Create: `src/web/routes/api.rs`
- Create: `templates/movie.html`
- Create: `templates/settings.html`
- Create: `templates/partials/notification.html`
- Modify: `src/web/mod.rs`

- [x] Create web/routes/movies.rs: GET /movies/:id with movie details, torrents, download status
- [x] Create templates/movie.html with poster, metadata, external links, download controls
- [x] Create web/routes/settings.rs: GET/POST /settings for viewing/updating config
- [x] Create templates/settings.html with config form
- [x] Create web/routes/api.rs: GET /api/notifications (HTMX partial polling every 30s), POST /api/notifications/:id/read, POST /api/media/:id/delete
- [x] Create templates/partials/notification.html toast component
- [x] Add graceful shutdown handling in main.rs
- [x] Write tests for movie page, settings CRUD, notification endpoints
- [x] Run cargo test - must pass before task 14

### Task 14: Verify Acceptance Criteria

- [x] Run full test suite: cargo test
- [x] Run clippy: cargo clippy -- -D warnings
- [x] Run cargo fmt --check
- [x] Verify all routes from architecture are implemented
- [x] Verify all DB tables are created via migrations
- [x] Verify all background tasks are spawned

### Task 15: Update Documentation

- [x] Update README.md with project description, setup instructions, configuration guide
- [x] Move this plan to `docs/plans/completed/`
