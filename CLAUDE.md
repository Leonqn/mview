# CLAUDE.md

## Purpose

mview is a Rust web application that tracks anime/TV/movies, auto-discovers torrents on rutracker, downloads via qBittorrent, and organizes completed downloads into a Plex-compatible library layout (hardlinks by default). It runs in Docker alongside Plex/qBittorrent.

## Architecture

**Pipeline:** user tracks media → DB stores seasons+episodes → user/scheduler searches rutracker → selected torrent added to qBittorrent → `download_monitor` polls for completion → files hardlinked into Plex dirs → Plex scan triggered.

**Key modules:**
- `src/web/` — axum routes (dashboard, series detail, search, api). Templates in `templates/` (minijinja + htmx).
- `src/db/` — SQLite schema + queries (rusqlite + r2d2). Migrations in `db/mod.rs`.
- `src/anilist/`, `src/tmdb/` — metadata sources. AniList for anime, TMDB for series/movies.
- `src/rutracker/` — torrent search + auth (handles captcha via Telegram).
- `src/qbittorrent/` — qBittorrent Web API client.
- `src/plex/` — Plex scan trigger + organizer (hardlink/copy, filename generation).
- `src/tasks/` — background workers: `download_monitor`, `tmdb_check`, `anilist_check` for periodic updates.
- `src/telegram/` — notifications + captcha relay.

**Data model:**
- `media` (type: movie/series/anime) → `seasons` → `episodes`
- `torrents` tied to `media_id` + `season_number`, status: `active`/`completed`/`failed`
- Movies: one season with one placeholder episode, marked `downloaded=1` once torrent completes.
- AniList anime: sequel chain flattened as seasons of one media record.

## Dev commands

```bash
cargo build
cargo test
cargo fmt
cargo clippy
```

Database lives at `mview.db` (configurable via env/config). In-memory `:memory:` is used in tests.

## Conventions

**Logging (tracing):** lowercase messages, no format placeholders, structured fields.
```rust
info!(torrent_id, title = %torrent.title, "torrent deleted");  // good
info!("Deleted torrent {}", torrent_id);                         // bad
```

**Error handling:** `anyhow::Result` at boundaries, `thiserror` for domain errors in `src/error.rs`.

**Templates:** server-rendered HTML via minijinja. HTMX drives partial updates with `hx-get`/`hx-post` targeting element IDs with `hx-swap="innerHTML"` or `outerHTML`.

**Plex layout:** `{anime_dir|tv_dir}/{Series Title}/Season NN/{Series Title} - SNNEMM - {Episode Title}.ext`. Movies: `{movies_dir}/{Title} ({Year})/{Title} ({Year}).ext`.

**Hardlinks:** `organizer::organize_file` prefers hard links, falls back to copy with a warning. Copy fallback doubles disk usage — log a WARN.

## Common patterns

**DB ops off async:** wrap blocking DB calls in `tokio::task::spawn_blocking` — connection pool is synchronous.

**Episode → file matching in `download_monitor::organize_series_files`:**
1. Filter video files to root directory only (excludes NC/, Extras/).
2. If `file count == episode count` and every file has a parseable episode number, use positional matching (handles 0-indexed torrents like Beatrice-Raws 00–12 → DB eps 1–13 OR 0–12).
3. Otherwise match by extracted episode number.

**AniList episodes:** `streamingEpisodes` gives real titles and can include Episode 0 (e.g. UBW Prologue). Use `parsed_streaming_episodes()` to get `(episode_number, title)` pairs. Fall back to `1..=episodes` generation only if `streamingEpisodes` is empty.

**Torrent delete endpoint:** default keeps torrent seeding in qBittorrent (only cleans DB + Plex hardlinks + triggers Plex scan). `delete_files=true` opt-in also removes source from qBittorrent.
