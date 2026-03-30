use anyhow::{Context, Result};
use teloxide::prelude::*;
use teloxide::types::ChatId;
use tracing::{info, warn};

/// Sends a notification message to the configured Telegram chat.
pub async fn send_notification(bot: &Bot, chat_id: i64, message: &str) -> Result<()> {
    if chat_id == 0 {
        warn!("telegram chat_id not configured, skipping notification");
        return Ok(());
    }

    // Skip if bot token is empty (not configured)
    if bot.token().is_empty() {
        warn!("telegram bot_token not configured, skipping notification");
        return Ok(());
    }

    bot.send_message(ChatId(chat_id), message)
        .await
        .context("Failed to send Telegram notification")?;

    info!(message, "sent telegram notification");
    Ok(())
}

/// Format a notification message for completed downloads.
pub fn format_download_complete(torrent_title: &str) -> String {
    format!("✅ Download completed: {}", torrent_title)
}

/// Format a notification message for torrent updates (re-downloaded).
pub fn format_torrent_update(torrent_title: &str) -> String {
    format!("🔄 Torrent updated: {}", torrent_title)
}

/// Format a notification message for new seasons discovered via TMDB.
pub fn format_new_season(media_title: &str, season_number: i64) -> String {
    format!(
        "📺 New season detected: {} - Season {}",
        media_title, season_number
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_download_complete() {
        let msg = format_download_complete("Breaking.Bad.S01.1080p");
        assert_eq!(msg, "✅ Download completed: Breaking.Bad.S01.1080p");
    }

    #[test]
    fn test_format_torrent_update() {
        let msg = format_torrent_update("Breaking.Bad.S01.1080p");
        assert_eq!(msg, "🔄 Torrent updated: Breaking.Bad.S01.1080p");
    }

    #[test]
    fn test_format_new_season() {
        let msg = format_new_season("Breaking Bad", 3);
        assert_eq!(msg, "📺 New season detected: Breaking Bad - Season 3");
    }
}
