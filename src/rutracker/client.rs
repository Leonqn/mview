use anyhow::{Context, Result};
use tracing::warn;

use super::auth::AuthHandle;

/// RuTracker HTTP client with channel-based authentication.
/// Uses the shared reqwest::Client from AuthHandle — cookies and connections are reused.
pub struct RutrackerClient {
    base_url: String,
    auth: AuthHandle,
}

impl RutrackerClient {
    pub fn new(base_url: &str, auth: AuthHandle) -> Self {
        let base_url = base_url.trim_end_matches('/').to_string();
        Self { base_url, auth }
    }

    /// Perform an authenticated GET request. If the response indicates
    /// a redirect to the login page (session expired), re-authenticate and retry once.
    pub async fn get(&self, url: &str) -> Result<String> {
        self.auth.ensure_authenticated().await?;
        let client = self.auth.client();

        let resp = client
            .get(url)
            .send()
            .await
            .context("failed to send GET request")?;

        if self.is_login_redirect(&resp) {
            warn!("session expired, re-authenticating");
            self.auth.invalidate();
            self.auth.ensure_authenticated().await?;
            let resp = client
                .get(url)
                .send()
                .await
                .context("failed to send GET request after re-auth")?;
            return resp
                .text()
                .await
                .context("failed to read response body after re-auth");
        }

        resp.text().await.context("failed to read response body")
    }

    /// Perform an authenticated GET request returning the raw response.
    pub async fn get_response(&self, url: &str) -> Result<reqwest::Response> {
        self.auth.ensure_authenticated().await?;
        let client = self.auth.client();

        let resp = client
            .get(url)
            .send()
            .await
            .context("failed to send GET request")?;

        if self.is_login_redirect(&resp) {
            warn!("session expired on download, re-authenticating");
            self.auth.invalidate();
            self.auth.ensure_authenticated().await?;
            return client
                .get(url)
                .send()
                .await
                .context("failed to send GET request after re-auth");
        }

        Ok(resp)
    }

    /// Check if a response is a redirect to the login page.
    fn is_login_redirect(&self, resp: &reqwest::Response) -> bool {
        if resp.status().is_redirection()
            && let Some(location) = resp.headers().get("location")
            && let Ok(loc) = location.to_str()
        {
            return loc.contains("login.php");
        }
        false
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

/// Extract captcha image URL from RuTracker login page HTML.
pub fn extract_captcha_url(html: &str, base_url: &str) -> Option<String> {
    let doc = scraper::Html::parse_document(html);
    let img_selector = scraper::Selector::parse("img[src*=\"captcha\"]").ok()?;

    if let Some(img) = doc.select(&img_selector).next() {
        let src = img.value().attr("src")?;
        if src.starts_with("http") {
            return Some(src.to_string());
        }
        return Some(format!("{}{}", base_url, src));
    }

    None
}

/// Extract captcha session ID (cap_sid) from the login form HTML.
pub fn extract_captcha_sid(html: &str) -> Option<String> {
    let doc = scraper::Html::parse_document(html);
    let input_selector = scraper::Selector::parse("input[name=\"cap_sid\"]").ok()?;

    if let Some(input) = doc.select(&input_selector).next() {
        return input.value().attr("value").map(|v| v.to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_captcha_url_found() {
        let html = r#"<html><body>
            <form>
                <img src="/forum/captcha.php?sid=abc123" />
                <input name="cap_sid" value="abc123" />
            </form>
        </body></html>"#;
        let url = extract_captcha_url(html, "https://rutracker.org");
        assert_eq!(
            url,
            Some("https://rutracker.org/forum/captcha.php?sid=abc123".to_string())
        );
    }

    #[test]
    fn test_extract_captcha_url_absolute() {
        let html = r#"<img src="https://example.com/captcha.png" />"#;
        let url = extract_captcha_url(html, "https://rutracker.org");
        assert_eq!(url, Some("https://example.com/captcha.png".to_string()));
    }

    #[test]
    fn test_extract_captcha_url_not_found() {
        let html = r#"<html><body><img src="/logo.png" /></body></html>"#;
        let url = extract_captcha_url(html, "https://rutracker.org");
        assert!(url.is_none());
    }

    #[test]
    fn test_extract_captcha_sid() {
        let html = r#"<form><input type="hidden" name="cap_sid" value="session123" /></form>"#;
        let sid = extract_captcha_sid(html);
        assert_eq!(sid, Some("session123".to_string()));
    }

    #[test]
    fn test_extract_captcha_sid_not_found() {
        let html = r#"<form><input name="other" value="x" /></form>"#;
        let sid = extract_captcha_sid(html);
        assert!(sid.is_none());
    }
}
