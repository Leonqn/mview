use anyhow::{Context, Result};
use tracing::info;

use super::client::RutrackerClient;

/// The download endpoint path on RuTracker.
const DOWNLOAD_PATH: &str = "/forum/dl.php";

impl RutrackerClient {
    /// Download a .torrent file from RuTracker by topic ID.
    /// Returns the raw torrent file bytes.
    pub async fn download_torrent(&self, topic_id: &str) -> Result<Vec<u8>> {
        let url = format!("{}{}?t={}", self.base_url(), DOWNLOAD_PATH, topic_id);
        info!(topic_id, "downloading torrent");

        let resp = self
            .get_response(&url)
            .await
            .context("failed to download torrent")?;

        let bytes = resp
            .bytes()
            .await
            .context("failed to read torrent file bytes")?;

        if bytes.is_empty() {
            anyhow::bail!("Downloaded torrent file is empty for topic {}", topic_id);
        }

        // Validate response is a bencoded torrent file (must start with 'd')
        if bytes[0] != b'd' {
            anyhow::bail!(
                "Downloaded data for topic {} is not a valid torrent file (expected bencoded dict)",
                topic_id
            );
        }

        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_download_url_construction() {
        let expected = format!("https://rutracker.org{DOWNLOAD_PATH}?t=12345");
        assert_eq!(expected, "https://rutracker.org/forum/dl.php?t=12345");
    }
}
