use std::sync::Arc;

use teloxide::prelude::*;
use teloxide::types::Me;
use teloxide::utils::command::BotCommands;
use tracing::{error, info, warn};

use crate::db::queries;
use crate::web::AppState;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase")]
enum Command {
    /// Show bot status and tracked media info
    Status,
    /// Show help
    Help,
}

/// Start the Telegram bot. This function runs indefinitely.
/// The bot handles /status and /help commands.
pub async fn run_bot(state: Arc<AppState>) {
    let token = &state.config.telegram.bot_token;
    if token.is_empty() {
        warn!("telegram bot_token not configured, bot will not start");
        return;
    }

    let bot = Bot::new(token);
    let chat_id = state.config.telegram.chat_id;

    info!("starting telegram bot");

    let handler = Update::filter_message().endpoint(
        move |bot: Bot, msg: Message, me: Me, state: Arc<AppState>| async move {
            handle_message(bot, msg, me, state, chat_id).await;
            respond(())
        },
    );

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .default_handler(|_upd| async {})
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}

async fn handle_message(
    bot: Bot,
    msg: Message,
    me: Me,
    state: Arc<AppState>,
    allowed_chat_id: i64,
) {
    // Only respond to the configured chat; ignore all messages if chat_id is not set
    if allowed_chat_id == 0 || msg.chat.id.0 != allowed_chat_id {
        return;
    }

    let text = match msg.text() {
        Some(t) => t,
        None => return,
    };

    // Try to parse as a command
    if let Ok(cmd) = Command::parse(text, me.username()) {
        match cmd {
            Command::Status => {
                let response = build_status_message(&state).await;
                if let Err(error) = bot.send_message(msg.chat.id, response).await {
                    error!(?error, "failed to send status message");
                }
            }
            Command::Help => {
                let response = "mview Bot Commands:\n/status - Show tracked media and downloads\n/help - Show this help";
                if let Err(error) = bot.send_message(msg.chat.id, response).await {
                    error!(?error, "failed to send help message");
                }
            }
        }
    }
}

/// Build the status message showing tracked media count and active downloads.
pub async fn build_status_message(state: &Arc<AppState>) -> String {
    let pool = state.db.clone();
    let stats = tokio::task::spawn_blocking(move || {
        let conn = pool.get()?;
        let media = queries::get_all_media(&conn)?;
        let active_torrents = queries::get_active_torrents_with_qbt_hash(&conn)?;
        let unread = queries::get_unread_notifications(&conn)?;
        Ok::<_, anyhow::Error>((media, active_torrents, unread))
    })
    .await;

    match stats {
        Ok(Ok((media, active_torrents, unread))) => {
            let movies = media.iter().filter(|m| m.media_type == "movie").count();
            let series = media.iter().filter(|m| m.media_type == "series").count();

            format!(
                "📊 mview Status\n\n\
                 Tracked media: {} ({} movies, {} series)\n\
                 Active downloads: {}\n\
                 Unread notifications: {}",
                media.len(),
                movies,
                series,
                active_torrents.len(),
                unread.len()
            )
        }
        _ => "❌ Failed to fetch status from database".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_command_parse() {
        // Test that BotCommands derive works
        let descriptions = Command::descriptions();
        let desc_str = descriptions.to_string();
        assert!(desc_str.contains("status"));
        assert!(desc_str.contains("help"));
    }
}
