use std::time::Duration;

use anyhow::{Context, Result};
use teloxide::prelude::*;
use teloxide::types::{ChatId, InputFile, MessageId};
use tokio::sync::oneshot;
use tracing::{info, warn};

const CAPTCHA_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes

/// A pending captcha that waits for a solution from Telegram.
pub struct PendingCaptcha {
    pub message_id: MessageId,
    pub solver: oneshot::Sender<String>,
}

/// Shared state for captcha solving via Telegram replies.
pub struct CaptchaSolver {
    pub bot: Bot,
    pub chat_id: i64,
    /// Currently pending captcha, if any. Protected by a mutex since
    /// only one captcha should be active at a time.
    pub pending: tokio::sync::Mutex<Option<PendingCaptcha>>,
}

impl CaptchaSolver {
    pub fn new(bot: Bot, chat_id: i64) -> Self {
        Self {
            bot,
            chat_id,
            pending: tokio::sync::Mutex::new(None),
        }
    }

    /// Send a captcha image to Telegram and wait for the user's reply.
    /// Returns the captcha solution text, or an error on timeout/failure.
    pub async fn solve_captcha(&self, image_data: Vec<u8>) -> Result<String> {
        if self.chat_id == 0 {
            anyhow::bail!("Telegram chat_id not configured for captcha solving");
        }

        let (tx, rx) = oneshot::channel();

        // Send the captcha image to Telegram
        let input_file = InputFile::memory(image_data).file_name("captcha.png");
        let sent = self
            .bot
            .send_photo(ChatId(self.chat_id), input_file)
            .caption("🔐 RuTracker captcha. Reply with the solution:")
            .await
            .context("Failed to send captcha image to Telegram")?;

        let message_id = sent.id;
        info!(message_id = ?message_id, "sent captcha to telegram");

        // Store the pending captcha
        {
            let mut pending = self.pending.lock().await;
            *pending = Some(PendingCaptcha {
                message_id,
                solver: tx,
            });
        }

        // Wait for the solution with a timeout
        match tokio::time::timeout(CAPTCHA_TIMEOUT, rx).await {
            Ok(Ok(solution)) => {
                info!("received captcha solution from telegram");
                Ok(solution)
            }
            Ok(Err(_)) => {
                // Channel was dropped without sending
                self.clear_pending().await;
                anyhow::bail!("Captcha solver channel closed unexpectedly")
            }
            Err(_) => {
                // Timeout
                self.clear_pending().await;
                warn!(timeout = ?CAPTCHA_TIMEOUT, "captcha solving timed out");
                anyhow::bail!(
                    "Captcha solving timed out after {} seconds",
                    CAPTCHA_TIMEOUT.as_secs()
                )
            }
        }
    }

    /// Handle a reply message from Telegram that might be a captcha solution.
    /// Returns true if the message was consumed as a captcha solution.
    pub async fn handle_reply(&self, reply_to_message_id: MessageId, text: &str) -> bool {
        let mut pending = self.pending.lock().await;
        if let Some(captcha) = pending.as_ref()
            && captcha.message_id == reply_to_message_id
            && let Some(captcha) = pending.take()
        {
            let _ = captcha.solver.send(text.trim().to_string());
            return true;
        }
        false
    }

    async fn clear_pending(&self) {
        let mut pending = self.pending.lock().await;
        *pending = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_captcha_timeout_constant() {
        assert_eq!(CAPTCHA_TIMEOUT, Duration::from_secs(300));
    }

    #[tokio::test]
    async fn test_captcha_solver_no_chat_id() {
        let bot = Bot::new("fake:token");
        let solver = CaptchaSolver::new(bot, 0);
        let result = solver.solve_captcha(vec![1, 2, 3]).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not configured"));
    }

    #[tokio::test]
    async fn test_captcha_handle_reply_no_pending() {
        let bot = Bot::new("fake:token");
        let solver = CaptchaSolver::new(bot, 12345);
        // No pending captcha, should return false
        let consumed = solver.handle_reply(MessageId(999), "solution").await;
        assert!(!consumed);
    }

    #[tokio::test]
    async fn test_captcha_handle_reply_wrong_message() {
        let bot = Bot::new("fake:token");
        let solver = CaptchaSolver::new(bot, 12345);

        let (tx, _rx) = oneshot::channel();
        {
            let mut pending = solver.pending.lock().await;
            *pending = Some(PendingCaptcha {
                message_id: MessageId(100),
                solver: tx,
            });
        }

        // Reply to wrong message
        let consumed = solver.handle_reply(MessageId(999), "solution").await;
        assert!(!consumed);

        // Pending should still be there
        let pending = solver.pending.lock().await;
        assert!(pending.is_some());
    }

    #[tokio::test]
    async fn test_captcha_handle_reply_correct_message() {
        let bot = Bot::new("fake:token");
        let solver = CaptchaSolver::new(bot, 12345);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = solver.pending.lock().await;
            *pending = Some(PendingCaptcha {
                message_id: MessageId(100),
                solver: tx,
            });
        }

        // Reply to correct message
        let consumed = solver.handle_reply(MessageId(100), " abc123 ").await;
        assert!(consumed);

        // Should receive trimmed solution
        let solution = rx.await.unwrap();
        assert_eq!(solution, "abc123");

        // Pending should be cleared
        let pending = solver.pending.lock().await;
        assert!(pending.is_none());
    }
}
