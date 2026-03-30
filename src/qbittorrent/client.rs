use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info};

use crate::config::QbittorrentConfig;

/// qBittorrent Web API client.
pub struct QbtClient {
    client: Client,
    config: Arc<QbittorrentConfig>,
    base_url: String,
    logged_in: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbtTorrent {
    pub hash: String,
    pub name: String,
    pub state: String,
    pub progress: f64,
    pub size: i64,
    pub save_path: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub added_on: i64,
    #[serde(default)]
    pub eta: i64,
    #[serde(default)]
    pub dlspeed: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QbtTorrentFile {
    pub name: String,
    pub size: i64,
    pub progress: f64,
}

impl QbtClient {
    pub fn new(config: Arc<QbittorrentConfig>) -> Result<Self> {
        let client = Client::builder()
            .cookie_store(true)
            .build()
            .context("Failed to create qBittorrent HTTP client")?;

        let base_url = config.url.trim_end_matches('/').to_string();

        Ok(Self {
            client,
            config,
            base_url,
            logged_in: false,
        })
    }

    /// Authenticate with qBittorrent Web API.
    pub async fn login(&mut self) -> Result<()> {
        let url = format!("{}/api/v2/auth/login", self.base_url);
        info!(base_url = self.base_url, "logging in to qbittorrent");

        let params = [
            ("username", self.config.username.as_str()),
            ("password", self.config.password.as_str()),
        ];

        let resp = self
            .client
            .post(&url)
            .form(&params)
            .send()
            .await
            .context("Failed to send qBittorrent login request")?;

        let body = resp.text().await.context("Failed to read login response")?;
        debug!(body = body, "qbittorrent login response");

        if body.contains("Ok") || body.contains("ok") {
            info!("successfully logged in to qbittorrent");
            self.logged_in = true;
            Ok(())
        } else {
            self.logged_in = false;
            anyhow::bail!("qBittorrent login failed: {}", body)
        }
    }

    /// Whether authentication is needed (credentials are configured).
    fn auth_required(&self) -> bool {
        !self.config.username.is_empty() && !self.config.password.is_empty()
    }

    /// Login only if not already authenticated.
    pub async fn ensure_logged_in(&mut self) -> Result<()> {
        if !self.logged_in && self.auth_required() {
            self.login().await?;
        }
        Ok(())
    }

    /// Send an add_torrent POST request, returning the response body.
    async fn send_add_torrent(
        &self,
        torrent_bytes: &[u8],
        filename: &str,
        save_path: &str,
        category: &str,
    ) -> Result<reqwest::Response> {
        let url = format!("{}/api/v2/torrents/add", self.base_url);

        let file_part = reqwest::multipart::Part::bytes(torrent_bytes.to_vec())
            .file_name(filename.to_string())
            .mime_str("application/x-bittorrent")
            .context("Failed to set MIME type")?;

        let form = reqwest::multipart::Form::new()
            .part("torrents", file_part)
            .text("savepath", save_path.to_string())
            .text("category", category.to_string());

        self.client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .context("Failed to send add_torrent request")
    }

    /// Add a .torrent file to qBittorrent with specified save path and category.
    /// Retries once with re-login if the session has expired (403).
    pub async fn add_torrent(
        &mut self,
        torrent_bytes: &[u8],
        filename: &str,
        save_path: &str,
        category: &str,
    ) -> Result<Option<String>> {
        let mut resp = self
            .send_add_torrent(torrent_bytes, filename, save_path, category)
            .await?;

        // Retry once on 403 (session expired)
        if resp.status() == reqwest::StatusCode::FORBIDDEN && self.auth_required() {
            debug!("qbittorrent session expired during add_torrent, re-logging in");
            self.logged_in = false;
            self.login().await?;
            resp = self
                .send_add_torrent(torrent_bytes, filename, save_path, category)
                .await?;
        }

        let body = resp
            .text()
            .await
            .context("Failed to read add_torrent response")?;

        if body.contains("Fails") {
            anyhow::bail!("qBittorrent rejected torrent: {}", body)
        }

        info!(filename, "torrent added to qbittorrent");

        // qBittorrent doesn't return the hash on add, so query the torrent list
        // and find the most recently added torrent in the category.
        // Retry a few times since qBittorrent may not index the torrent immediately.
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            if let Some(t) = self.find_most_recent_torrent(category).await? {
                return Ok(Some(t.hash));
            }
            debug!(attempt, "torrent not yet visible in qbittorrent, retrying");
        }

        Ok(None)
    }

    /// Find the most recently added torrent in a category.
    async fn find_most_recent_torrent(&mut self, category: &str) -> Result<Option<QbtTorrent>> {
        let mut torrents = self.get_torrents(Some(category)).await?;
        torrents.sort_by(|a, b| b.added_on.cmp(&a.added_on));
        Ok(torrents.into_iter().next())
    }

    /// Send an API request, retrying once with re-login if the session has expired (403).
    async fn api_get(&mut self, url: &str) -> Result<reqwest::Response> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .context("Failed to send qBittorrent API request")?;

        if resp.status() == reqwest::StatusCode::FORBIDDEN && self.auth_required() {
            debug!("qbittorrent session expired, re-logging in");
            self.logged_in = false;
            self.login().await?;
            return self
                .client
                .get(url)
                .send()
                .await
                .context("Failed to send qBittorrent API request after re-login");
        }

        Ok(resp)
    }

    /// List torrents, optionally filtered by category.
    pub async fn get_torrents(&mut self, category: Option<&str>) -> Result<Vec<QbtTorrent>> {
        let mut url = format!("{}/api/v2/torrents/info", self.base_url);
        if let Some(cat) = category {
            let encoded = urlencoding::encode(cat);
            url = format!("{}?category={}", url, encoded);
        }

        let resp = self.api_get(&url).await?;

        let torrents: Vec<QbtTorrent> = resp
            .json()
            .await
            .context("Failed to parse qBittorrent torrents response")?;

        Ok(torrents)
    }

    /// Get file list for a specific torrent by hash.
    pub async fn get_torrent_files(&mut self, hash: &str) -> Result<Vec<QbtTorrentFile>> {
        let encoded_hash = urlencoding::encode(hash);
        let url = format!(
            "{}/api/v2/torrents/files?hash={}",
            self.base_url, encoded_hash
        );

        let resp = self.api_get(&url).await?;

        let files: Vec<QbtTorrentFile> = resp
            .json()
            .await
            .context("Failed to parse qBittorrent torrent files response")?;

        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl QbtClient {
        fn base_url(&self) -> &str {
            &self.base_url
        }
    }

    fn test_config() -> Arc<QbittorrentConfig> {
        Arc::new(QbittorrentConfig {
            url: "http://localhost:8080".to_string(),
            username: "admin".to_string(),
            password: "adminpass".to_string(),
        })
    }

    #[test]
    fn test_client_creation() {
        let config = test_config();
        let client = QbtClient::new(config).unwrap();
        assert_eq!(client.base_url(), "http://localhost:8080");
    }

    #[test]
    fn test_client_strips_trailing_slash() {
        let config = Arc::new(QbittorrentConfig {
            url: "http://localhost:8080/".to_string(),
            username: "admin".to_string(),
            password: "admin".to_string(),
        });
        let client = QbtClient::new(config).unwrap();
        assert_eq!(client.base_url(), "http://localhost:8080");
    }

    #[test]
    fn test_qbt_torrent_deserialization() {
        let json = r#"{
            "hash": "abc123",
            "name": "Test.Torrent",
            "state": "downloading",
            "progress": 0.5,
            "size": 1000000,
            "save_path": "/downloads",
            "category": "mview"
        }"#;
        let torrent: QbtTorrent = serde_json::from_str(json).unwrap();
        assert_eq!(torrent.hash, "abc123");
        assert_eq!(torrent.name, "Test.Torrent");
        assert_eq!(torrent.state, "downloading");
        assert!((torrent.progress - 0.5).abs() < f64::EPSILON);
        assert_eq!(torrent.size, 1000000);
        assert_eq!(torrent.category, "mview");
    }

    #[test]
    fn test_qbt_torrent_deserialization_missing_category() {
        let json = r#"{
            "hash": "abc123",
            "name": "Test",
            "state": "downloading",
            "progress": 0.0,
            "size": 0,
            "save_path": "/tmp"
        }"#;
        let torrent: QbtTorrent = serde_json::from_str(json).unwrap();
        assert_eq!(torrent.category, "");
    }

    #[test]
    fn test_qbt_torrent_file_deserialization() {
        let json = r#"{
            "name": "video.mkv",
            "size": 5000000000,
            "progress": 1.0
        }"#;
        let file: QbtTorrentFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.name, "video.mkv");
        assert_eq!(file.size, 5000000000);
        assert!((file.progress - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_add_torrent_url_construction() {
        let config = test_config();
        let client = QbtClient::new(config).unwrap();
        let expected_url = format!("{}/api/v2/torrents/add", client.base_url());
        assert_eq!(expected_url, "http://localhost:8080/api/v2/torrents/add");
    }
}
