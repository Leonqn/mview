use anyhow::{Context, Result};
use serde::Deserialize;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub rutracker: RutrackerConfig,
    pub qbittorrent: QbittorrentConfig,
    pub tmdb: TmdbConfig,
    #[serde(default)]
    pub plex: PlexConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
    pub paths: PathsConfig,
    #[serde(default)]
    pub server: ServerConfig,
}

#[derive(Deserialize, Clone)]
pub struct RutrackerConfig {
    pub url: String,
    pub username: String,
    pub password: String,
}

impl fmt::Debug for RutrackerConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RutrackerConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Deserialize, Clone)]
pub struct QbittorrentConfig {
    pub url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

impl fmt::Debug for QbittorrentConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QbittorrentConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

#[derive(Deserialize, Clone)]
pub struct TmdbConfig {
    pub api_key: String,
}

impl fmt::Debug for TmdbConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TmdbConfig")
            .field("api_key", &"[REDACTED]")
            .finish()
    }
}

#[derive(Deserialize, Clone, Default)]
pub struct PlexConfig {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub token: String,
}

impl fmt::Debug for PlexConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlexConfig")
            .field("url", &self.url)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

#[derive(Deserialize, Clone, Default)]
pub struct TelegramConfig {
    #[serde(default)]
    pub bot_token: String,
    #[serde(default)]
    pub chat_id: i64,
}

impl fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramConfig")
            .field("bot_token", &"[REDACTED]")
            .field("chat_id", &self.chat_id)
            .finish()
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct PathsConfig {
    pub download_dir: String,
    pub movies_dir: String,
    pub tv_dir: String,
    pub anime_dir: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn default_port() -> u16 {
    3000
}

impl Config {
    pub fn load(path: &str) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {path}"))?;
        let config: Config =
            toml::from_str(&content).with_context(|| "Failed to parse config file")?;
        Ok(config)
    }

    pub fn config_path_from_args() -> String {
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "config.toml".to_string())
    }

    pub fn db_path() -> PathBuf {
        PathBuf::from("mview.db")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn valid_config_toml() -> &'static str {
        r#"
[rutracker]
url = "https://rutracker.org"
username = "user"
password = "pass"

[qbittorrent]
url = "http://localhost:8080"
username = "admin"
password = "adminpass"

[tmdb]
api_key = "abc123"

[plex]
url = "http://localhost:32400"
token = "plex-token"

[telegram]
bot_token = "123:ABC"
chat_id = 12345

[paths]
download_dir = "/data/downloads"
movies_dir = "/media/movies"
tv_dir = "/media/tv"
anime_dir = "/media/anime"

[server]
host = "0.0.0.0"
port = 8080
"#
    }

    fn write_temp_config(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn test_load_valid_config() {
        let f = write_temp_config(valid_config_toml());
        let config = Config::load(f.path().to_str().unwrap()).unwrap();
        assert_eq!(config.rutracker.url, "https://rutracker.org");
        assert_eq!(config.qbittorrent.username, "admin");
        assert_eq!(config.tmdb.api_key, "abc123");
        assert_eq!(config.plex.url, "http://localhost:32400");
        assert_eq!(config.telegram.chat_id, 12345);
        assert_eq!(config.paths.download_dir, "/data/downloads");
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 8080);
    }

    #[test]
    fn test_load_config_with_defaults() {
        let content = r#"
[rutracker]
url = "https://rutracker.org"
username = "user"
password = "pass"

[qbittorrent]
url = "http://localhost:8080"
username = "admin"
password = "adminpass"

[tmdb]
api_key = "abc123"

[paths]
download_dir = "/data/downloads"
movies_dir = "/media/movies"
tv_dir = "/media/tv"
anime_dir = "/media/anime"
"#;
        let f = write_temp_config(content);
        let config = Config::load(f.path().to_str().unwrap()).unwrap();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 3000);
        assert_eq!(config.plex.url, "");
        assert_eq!(config.telegram.bot_token, "");
    }

    #[test]
    fn test_load_config_missing_required_field() {
        let content = r#"
[rutracker]
url = "https://rutracker.org"
"#;
        let f = write_temp_config(content);
        let result = Config::load(f.path().to_str().unwrap());
        assert!(result.is_err());
    }

    #[test]
    fn test_load_config_file_not_found() {
        let result = Config::load("/nonexistent/config.toml");
        assert!(result.is_err());
    }
}
