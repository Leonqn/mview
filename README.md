# mview — Media Manager for RuTracker + Plex

A simplified alternative to Sonarr/Radarr focused on RuTracker. The app monitors torrent distributions, auto-updates torrents when new episodes appear, organizes files for Plex-compatible directory structure, provides a web UI via HTMX, and sends notifications through Telegram.

## Features

- Search and browse RuTracker torrents from the web UI
- Track movies and TV series with metadata from TMDB and AniList (anime)
- Download torrents via qBittorrent integration
- Automatic file organization into Plex-compatible directory structure (hardlinks with copy fallback)
- Background monitoring: detect torrent updates on RuTracker, new seasons via TMDB, and AniList updates
- Telegram bot for notifications and captcha solving during RuTracker login
- HTMX-powered responsive web interface with Pico CSS

## Requirements

- Rust (edition 2024) — for building from source
- qBittorrent with Web UI enabled
- A TMDB API key (free at https://www.themoviedb.org/settings/api)
- Optional: Plex Media Server
- Optional: Telegram bot token (via @BotFather)

## Setup

### From source

1. Clone the repository and build:

```sh
cargo build --release
```

2. Copy the example config and edit it:

```sh
cp config.example.toml config.toml
```

3. Fill in your credentials in `config.toml` (see [Configuration](#configuration)).

4. Run the application:

```sh
./target/release/mview config.toml
```

### Docker Compose

A `docker-compose.yml` is included with a full stack: mview + qBittorrent + Plex + Watchtower (auto-updates).

```sh
docker compose up -d
```

The web UI will be available at `http://127.0.0.1:3000` (or your configured host/port).

## Configuration

See `config.example.toml` for a full example with all available options.

| Section | Required | Description |
|---|---|---|
| `[rutracker]` | Yes | RuTracker URL, username, and password |
| `[qbittorrent]` | Yes | qBittorrent Web UI URL and credentials |
| `[tmdb]` | Yes | TMDB API key |
| `[paths]` | Yes | Download directory, movies library path, TV and anime library paths |
| `[plex]` | No | Plex server URL and token for automatic library scans |
| `[telegram]` | No | Telegram bot token and chat ID for notifications |
| `[server]` | No | Host and port (defaults to `127.0.0.1:3000`) |

### Paths

- `download_dir` — where qBittorrent saves downloaded files
- `movies_dir` — Plex movies library root (e.g. `/media/movies`)
- `tv_dir` — Plex TV shows library root (e.g. `/media/tv`)
- `anime_dir` — Plex anime library root (e.g. `/media/anime`)

The organizer creates hardlinks from the download directory into the Plex library structure. If hardlinks fail (cross-filesystem), it falls back to copying.

### File Organization

Movies:
```
/media/movies/Title (Year)/Title (Year).ext
```

TV shows and anime:
```
/media/tv/Title/Season 01/Title - S01E01.ext
```

## Development

```sh
cargo test        # Run tests
cargo clippy      # Lint
cargo fmt         # Format
```

## License

Private project.
