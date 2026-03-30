use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use crate::web::AppState;

pub struct PlexClient {
    client: reqwest::Client,
    base_url: String,
    token: String,
}

/// A Plex library section with its key and root paths.
#[derive(Debug)]
struct Section {
    key: String,
    paths: Vec<String>,
}

impl PlexClient {
    pub fn new(base_url: &str, token: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .build()
            .context("Failed to create Plex HTTP client")?;

        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        })
    }

    /// Fetch all library sections with their root paths.
    async fn get_sections(&self) -> Result<Vec<Section>> {
        let sections_url = format!("{}/library/sections", self.base_url);
        let resp = self
            .client
            .get(&sections_url)
            .header("X-Plex-Token", &self.token)
            .header("Accept", "application/json")
            .send()
            .await
            .context("failed to get plex library sections")?;

        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("failed to get plex library sections: HTTP {}", status);
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse plex sections response")?;

        let mut sections = Vec::new();
        if let Some(directories) = body
            .get("MediaContainer")
            .and_then(|mc| mc.get("Directory"))
            .and_then(|d| d.as_array())
        {
            for dir in directories {
                if let Some(key) = dir.get("key").and_then(|k| k.as_str()) {
                    let paths = dir
                        .get("Location")
                        .and_then(|l| l.as_array())
                        .map(|locs| {
                            locs.iter()
                                .filter_map(|loc| loc.get("path").and_then(|p| p.as_str()))
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default();

                    sections.push(Section {
                        key: key.to_string(),
                        paths,
                    });
                }
            }
        }

        Ok(sections)
    }

    /// Trigger a Plex scan for specific paths. Finds the matching section for each
    /// path and triggers a targeted scan instead of scanning the entire library.
    pub async fn scan_paths(&self, paths: &[String]) -> Result<()> {
        let sections = self.get_sections().await?;

        for scan_path in paths {
            let section = sections.iter().find(|s| {
                s.paths
                    .iter()
                    .any(|root| scan_path.starts_with(root.as_str()))
            });

            match section {
                Some(section) => {
                    let url = format!("{}/library/sections/{}/refresh", self.base_url, section.key);
                    let _ = self
                        .client
                        .get(&url)
                        .query(&[("path", scan_path.as_str())])
                        .header("X-Plex-Token", &self.token)
                        .send()
                        .await;
                    info!(
                        section = section.key,
                        path = scan_path,
                        "triggered plex scan for path"
                    );
                }
                None => {
                    debug!(path = scan_path, "no plex section found for path, skipping");
                }
            }
        }

        Ok(())
    }
}

/// Trigger a Plex library scan for specific paths, logging errors without failing.
pub async fn scan(state: &Arc<AppState>, paths: &[String]) {
    if state.config.plex.url.is_empty() || state.config.plex.token.is_empty() {
        debug!("plex not configured, skipping scan");
        return;
    }

    match PlexClient::new(&state.config.plex.url, &state.config.plex.token) {
        Ok(plex) => {
            if let Err(error) = plex.scan_paths(paths).await {
                warn!(?error, "failed to trigger plex scan");
            }
        }
        Err(error) => {
            warn!(?error, "failed to create plex client");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plex_client_creation() {
        let client = PlexClient::new("http://localhost:32400", "test-token");
        assert!(client.is_ok());
    }

    #[test]
    fn test_plex_client_trims_trailing_slash() {
        let client = PlexClient::new("http://localhost:32400/", "token").unwrap();
        assert_eq!(client.base_url, "http://localhost:32400");
    }
}
