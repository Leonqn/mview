use tracing::{debug, info};

use crate::db::models::Torrent;

/// Result of checking a torrent for updates on RuTracker.
#[derive(Debug)]
pub struct UpdateCheckResult {
    pub has_update: bool,
    pub new_registered_at: Option<String>,
    pub new_torrent_hash: Option<String>,
}

/// Compare the stored torrent hash with the current one from RuTracker
/// to detect if the distribution has been updated (new episodes, re-uploads, etc.).
/// Falls back to comparing registered_at if hash is unavailable.
pub fn check_update(
    torrent: &Torrent,
    new_registered_at: Option<&str>,
    new_torrent_hash: Option<&str>,
) -> UpdateCheckResult {
    // Primary: compare torrent hash (info hash from magnet link)
    let has_update = match (&torrent.torrent_hash, new_torrent_hash) {
        (Some(old), Some(new)) => {
            let updated = !old.eq_ignore_ascii_case(new);
            if updated {
                info!(
                    title = torrent.title,
                    topic_id = torrent.rutracker_topic_id,
                    old_hash = old.as_str(),
                    new_hash = new,
                    "torrent hash changed, distribution updated"
                );
            } else {
                debug!(
                    title = torrent.title,
                    topic_id = torrent.rutracker_topic_id,
                    "torrent hash unchanged"
                );
            }
            updated
        }
        (None, Some(_)) => {
            debug!(
                title = torrent.title,
                topic_id = torrent.rutracker_topic_id,
                "first torrent hash recorded, not treating as update"
            );
            false
        }
        _ => {
            // Fallback: compare registered_at
            match (&torrent.registered_at, new_registered_at) {
                (Some(old), Some(new)) => {
                    let updated = old != new;
                    if updated {
                        info!(
                            title = torrent.title,
                            topic_id = torrent.rutracker_topic_id,
                            old_registered_at = old.as_str(),
                            new_registered_at = new,
                            "torrent registered_at changed"
                        );
                    }
                    updated
                }
                _ => false,
            }
        }
    };

    UpdateCheckResult {
        has_update,
        new_registered_at: new_registered_at.map(|s| s.to_string()),
        new_torrent_hash: new_torrent_hash.map(|s| s.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_torrent(registered_at: Option<&str>, torrent_hash: Option<&str>) -> Torrent {
        Torrent {
            id: 1,
            media_id: 1,
            rutracker_topic_id: "123456".to_string(),
            title: "Test.Series.S01".to_string(),
            quality: Some("1080p".to_string()),
            size_bytes: Some(5_000_000_000),
            seeders: Some(10),
            season_number: Some(1),
            episode_info: Some("1-12".to_string()),
            registered_at: registered_at.map(|s| s.to_string()),
            last_checked_at: None,
            torrent_hash: torrent_hash.map(|s| s.to_string()),
            qbt_hash: Some("abc123".to_string()),
            status: "active".to_string(),
            auto_update: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn test_check_update_hash_changed() {
        let torrent = make_torrent(None, Some("aaa111"));
        let result = check_update(&torrent, None, Some("bbb222"));
        assert!(result.has_update);
    }

    #[test]
    fn test_check_update_hash_unchanged() {
        let torrent = make_torrent(None, Some("aaa111"));
        let result = check_update(&torrent, None, Some("AAA111"));
        assert!(!result.has_update);
    }

    #[test]
    fn test_check_update_first_hash() {
        let torrent = make_torrent(None, None);
        let result = check_update(&torrent, None, Some("aaa111"));
        assert!(!result.has_update);
        assert_eq!(result.new_torrent_hash.as_deref(), Some("aaa111"));
    }

    #[test]
    fn test_check_update_fallback_registered_at() {
        let torrent = make_torrent(Some("2024-01-15"), None);
        let result = check_update(&torrent, Some("2024-02-20"), None);
        assert!(result.has_update);
    }

    #[test]
    fn test_check_update_no_info() {
        let torrent = make_torrent(None, None);
        let result = check_update(&torrent, None, None);
        assert!(!result.has_update);
    }
}
