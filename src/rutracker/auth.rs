use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use reqwest::cookie::CookieStore;
use tokio::sync::{RwLock, mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::config::RutrackerConfig;

use super::client::{extract_captcha_sid, extract_captcha_url};

const LOGIN_PATH: &str = "/forum/login.php";
const CAPTCHA_TIMEOUT: Duration = Duration::from_secs(300);
const AUTH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of an auth request.
pub type AuthResult = Result<(), String>;

/// Message sent to the auth task.
pub enum AuthMessage {
    /// Ensure we are authenticated. Triggers login if needed.
    EnsureAuth { reply: oneshot::Sender<AuthResult> },
    /// Invalidate the current session (session expired).
    Invalidate,
    /// Submit a captcha solution from the web UI.
    SubmitCaptcha {
        code: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// Captcha data exposed to the web UI.
#[derive(Clone)]
pub struct CaptchaForWeb {
    pub image_data: Vec<u8>,
}

/// Handle for communicating with the auth task. Clone-friendly.
/// Holds the shared reqwest::Client with cookie jar — callers use it directly.
#[derive(Clone)]
pub struct AuthHandle {
    auth_tx: mpsc::Sender<AuthMessage>,
    http_client: reqwest::Client,
    pub captcha_state: Arc<RwLock<Option<CaptchaForWeb>>>,
}

impl AuthHandle {
    /// Ensure we are authenticated. Triggers login if needed, waits for completion.
    pub async fn ensure_authenticated(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.auth_tx
            .send(AuthMessage::EnsureAuth { reply: tx })
            .await
            .map_err(|_| anyhow::anyhow!("auth task is not running"))?;
        tokio::time::timeout(AUTH_RESPONSE_TIMEOUT, rx)
            .await
            .map_err(|_| anyhow::anyhow!("auth response timed out"))?
            .map_err(|_| anyhow::anyhow!("auth task stopped unexpectedly"))?
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Notify the auth task that the current session is invalid.
    pub fn invalidate(&self) {
        let _ = self.auth_tx.try_send(AuthMessage::Invalidate);
    }

    /// Submit a captcha solution from the web UI.
    pub async fn submit_captcha(&self, code: String) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.auth_tx
            .send(AuthMessage::SubmitCaptcha { reply: tx, code })
            .await
            .map_err(|_| "auth task is not running".to_string())?;
        rx.await
            .map_err(|_| "auth task dropped the reply channel".to_string())?
    }

    /// Get the shared HTTP client. Cookies are managed by the cookie jar automatically.
    pub fn client(&self) -> &reqwest::Client {
        &self.http_client
    }
}

/// Internal state for the auth task.
struct AuthTaskState {
    http_client: reqwest::Client,
    cookie_jar: Arc<reqwest::cookie::Jar>,
    config: Arc<RutrackerConfig>,
    base_url: String,
    authenticated: bool,
    waiters: Vec<oneshot::Sender<AuthResult>>,
    pending_captcha: Option<PendingCaptcha>,
    captcha_state: Arc<RwLock<Option<CaptchaForWeb>>>,
    login_in_progress: bool,
}

struct PendingCaptcha {
    cap_sid: String,
    created_at: Instant,
}

/// Spawn the auth task and return a handle for communication.
pub fn spawn_auth_task(config: Arc<RutrackerConfig>) -> AuthHandle {
    let (auth_tx, auth_rx) = mpsc::channel(32);
    let captcha_state: Arc<RwLock<Option<CaptchaForWeb>>> = Arc::new(RwLock::new(None));

    let cookie_jar = Arc::new(reqwest::cookie::Jar::default());
    let http_client = reqwest::Client::builder()
        .cookie_provider(cookie_jar.clone())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("failed to create http client");

    let handle = AuthHandle {
        auth_tx,
        http_client: http_client.clone(),
        captcha_state: captcha_state.clone(),
    };

    let base_url = config.url.trim_end_matches('/').to_string();

    let state = AuthTaskState {
        http_client,
        cookie_jar,
        config,
        base_url,
        authenticated: false,
        waiters: Vec::new(),
        pending_captcha: None,
        captcha_state,
        login_in_progress: false,
    };

    tokio::spawn(auth_task_loop(state, auth_rx));

    handle
}

async fn auth_task_loop(mut state: AuthTaskState, mut rx: mpsc::Receiver<AuthMessage>) {
    info!("auth task started");

    loop {
        let captcha_deadline = state.pending_captcha.as_ref().map(|pc| {
            let elapsed = pc.created_at.elapsed();
            if elapsed >= CAPTCHA_TIMEOUT {
                Duration::ZERO
            } else {
                CAPTCHA_TIMEOUT - elapsed
            }
        });

        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else {
                    info!("auth task channel closed, shutting down");
                    drain_waiters(&mut state.waiters, Err("auth task shutting down".to_string()));
                    return;
                };
                handle_message(&mut state, msg).await;
            }
            _ = tokio::time::sleep(captcha_deadline.unwrap_or(Duration::from_secs(3600))),
                if state.pending_captcha.is_some() =>
            {
                warn!("captcha solving timed out");
                state.pending_captcha = None;
                *state.captcha_state.write().await = None;
                state.login_in_progress = false;
                drain_waiters(&mut state.waiters, Err("captcha solving timed out".to_string()));
            }
        }
    }
}

async fn handle_message(state: &mut AuthTaskState, msg: AuthMessage) {
    match msg {
        AuthMessage::EnsureAuth { reply } => {
            if state.authenticated {
                let _ = reply.send(Ok(()));
            } else if state.pending_captcha.is_some() {
                let _ = reply.send(Err("captcha required, solve at /captcha".to_string()));
            } else {
                state.waiters.push(reply);
                if !state.login_in_progress {
                    state.login_in_progress = true;
                    start_login(state).await;
                }
            }
        }
        AuthMessage::Invalidate => {
            debug!("session invalidated");
            state.authenticated = false;
        }
        AuthMessage::SubmitCaptcha { code, reply } => {
            if let Some(pending) = state.pending_captcha.take() {
                *state.captcha_state.write().await = None;
                submit_captcha_login(state, pending, &code, reply).await;
            } else {
                let _ = reply.send(Err("no pending captcha".to_string()));
            }
        }
    }
}

async fn start_login(state: &mut AuthTaskState) {
    let login_url = format!("{}{}", state.base_url, LOGIN_PATH);
    info!(base_url = state.base_url, "logging in to rutracker");

    let body = build_login_form(&state.config.username, &state.config.password, None);

    let resp = match state
        .http_client
        .post(&login_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Referer", &login_url)
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(error) => {
            error!(?error, "login request failed");
            state.login_in_progress = false;
            drain_waiters(
                &mut state.waiters,
                Err(format!("login request failed: {error}")),
            );
            return;
        }
    };

    let status = resp.status();

    if status.is_redirection() {
        if !has_session_cookie(state) {
            warn!("login redirected but no session cookie in jar");
            state.login_in_progress = false;
            drain_waiters(
                &mut state.waiters,
                Err("login succeeded but no session cookie received".to_string()),
            );
            return;
        }
        info!("successfully logged in to rutracker");
        state.authenticated = true;
        state.login_in_progress = false;
        drain_waiters(&mut state.waiters, Ok(()));
        return;
    }

    let body = resp.text().await.unwrap_or_default();

    if body.contains("cap_code") || body.contains("captcha") {
        info!("rutracker requires captcha for login");
        handle_captcha_required(state, &body).await;
        return;
    }

    if body.contains("login-form-full") || body.contains("Неверный пароль") {
        error!("login failed: invalid credentials");
        state.login_in_progress = false;
        drain_waiters(
            &mut state.waiters,
            Err("login failed: invalid credentials".to_string()),
        );
        return;
    }

    if status.is_success() {
        info!("login returned 200, assuming success");
        state.authenticated = true;
        state.login_in_progress = false;
        drain_waiters(&mut state.waiters, Ok(()));
        return;
    }

    error!(status = %status, "login failed with unexpected status");
    state.login_in_progress = false;
    drain_waiters(
        &mut state.waiters,
        Err(format!("login failed with status: {status}")),
    );
}

async fn handle_captcha_required(state: &mut AuthTaskState, body: &str) {
    let captcha_url = match extract_captcha_url(body, &state.base_url) {
        Some(url) => url,
        None => {
            error!("could not find captcha image url in login page");
            state.login_in_progress = false;
            drain_waiters(
                &mut state.waiters,
                Err("could not find captcha image url".to_string()),
            );
            return;
        }
    };

    let cap_sid = extract_captcha_sid(body).unwrap_or_default();

    let image_data = match state.http_client.get(&captcha_url).send().await {
        Ok(r) => match r.bytes().await {
            Ok(b) => b.to_vec(),
            Err(error) => {
                error!(?error, "failed to read captcha image");
                state.login_in_progress = false;
                drain_waiters(
                    &mut state.waiters,
                    Err(format!("failed to read captcha image: {error}")),
                );
                return;
            }
        },
        Err(error) => {
            error!(?error, "failed to download captcha image");
            state.login_in_progress = false;
            drain_waiters(
                &mut state.waiters,
                Err(format!("failed to download captcha image: {error}")),
            );
            return;
        }
    };

    *state.captcha_state.write().await = Some(CaptchaForWeb {
        image_data: image_data.clone(),
    });

    state.pending_captcha = Some(PendingCaptcha {
        cap_sid,
        created_at: Instant::now(),
    });

    info!("captcha ready for solving via web ui");
}

async fn submit_captcha_login(
    state: &mut AuthTaskState,
    pending: PendingCaptcha,
    code: &str,
    reply: oneshot::Sender<Result<(), String>>,
) {
    let login_url = format!("{}{}", state.base_url, LOGIN_PATH);

    let body = build_login_form(
        &state.config.username,
        &state.config.password,
        Some((&pending.cap_sid, code)),
    );

    let resp = match state
        .http_client
        .post(&login_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Referer", &login_url)
        .body(body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(error) => {
            let msg = format!("login with captcha failed: {error}");
            error!(?error, "login with captcha request failed");
            let _ = reply.send(Err(msg.clone()));
            state.login_in_progress = false;
            drain_waiters(&mut state.waiters, Err(msg));
            return;
        }
    };

    let status = resp.status();

    if status.is_redirection() {
        if !has_session_cookie(state) {
            let msg = "login with captcha succeeded but no session cookie".to_string();
            warn!("{msg}");
            let _ = reply.send(Err(msg.clone()));
            state.login_in_progress = false;
            drain_waiters(&mut state.waiters, Err(msg));
            return;
        }
        info!("successfully logged in to rutracker with captcha");
        let _ = reply.send(Ok(()));
        state.authenticated = true;
        state.login_in_progress = false;
        drain_waiters(&mut state.waiters, Ok(()));
        return;
    }

    let body = resp.text().await.unwrap_or_default();

    if body.contains("cap_code") {
        warn!("captcha solution was incorrect");
        let _ = reply.send(Err("captcha solution was incorrect".to_string()));
        start_login(state).await;
        return;
    }

    if body.contains("login-form-full") || body.contains("Неверный пароль") {
        let msg = "login failed after captcha: invalid credentials".to_string();
        error!("{msg}");
        let _ = reply.send(Err(msg.clone()));
        state.login_in_progress = false;
        drain_waiters(&mut state.waiters, Err(msg));
        return;
    }

    info!("login with captcha returned 200, assuming success");
    let _ = reply.send(Ok(()));
    state.authenticated = true;
    state.login_in_progress = false;
    drain_waiters(&mut state.waiters, Ok(()));
}

/// Build url-encoded login form body with windows-1251 compatible encoding.
fn build_login_form(username: &str, password: &str, captcha: Option<(&str, &str)>) -> String {
    // "Вход" in windows-1251 url-encoded
    const LOGIN_BUTTON_W1251: &str = "%C2%F5%EE%E4";

    let mut form = format!(
        "login_username={}&login_password={}&login={}",
        urlencoding::encode(username),
        urlencoding::encode(password),
        LOGIN_BUTTON_W1251,
    );

    if let Some((cap_sid, cap_code)) = captcha {
        form.push_str(&format!(
            "&cap_sid={}&cap_code={}",
            urlencoding::encode(cap_sid),
            urlencoding::encode(cap_code),
        ));
    }

    form
}

fn drain_waiters(waiters: &mut Vec<oneshot::Sender<AuthResult>>, result: AuthResult) {
    for waiter in waiters.drain(..) {
        let _ = waiter.send(result.clone());
    }
}

/// Check if the cookie jar has a session cookie for the forum.
fn has_session_cookie(state: &AuthTaskState) -> bool {
    let url = format!("{}/forum/", state.base_url)
        .parse::<reqwest::Url>()
        .unwrap();
    state
        .cookie_jar
        .cookies(&url)
        .and_then(|h| h.to_str().ok().map(|s| !s.is_empty()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Arc<RutrackerConfig> {
        Arc::new(RutrackerConfig {
            url: "https://rutracker.org".to_string(),
            username: "test".to_string(),
            password: "test".to_string(),
        })
    }

    #[tokio::test]
    async fn test_auth_handle_invalidate_does_not_panic() {
        let handle = spawn_auth_task(test_config());
        handle.invalidate();
    }

    #[tokio::test]
    async fn test_captcha_state_initially_none() {
        let handle = spawn_auth_task(test_config());
        let state = handle.captcha_state.read().await;
        assert!(state.is_none());
    }

    #[tokio::test]
    async fn test_submit_captcha_no_pending() {
        let handle = spawn_auth_task(test_config());
        let result = handle.submit_captcha("test".to_string()).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no pending captcha"));
    }

    #[tokio::test]
    async fn test_ensure_auth_fails_on_unreachable_server() {
        let config = Arc::new(RutrackerConfig {
            url: "http://127.0.0.1:19999".to_string(),
            username: "test".to_string(),
            password: "test".to_string(),
        });
        let handle = spawn_auth_task(config);
        let result = handle.ensure_authenticated().await;
        assert!(result.is_err());
    }

    #[test]
    fn test_build_login_form_without_captcha() {
        let form = build_login_form("user", "pass", None);
        assert!(form.contains("login_username=user"));
        assert!(form.contains("login_password=pass"));
        assert!(form.contains("login=%C2%F5%EE%E4"));
        assert!(!form.contains("cap_sid"));
    }

    #[test]
    fn test_build_login_form_with_captcha() {
        let form = build_login_form("user", "pass", Some(("sid123", "abc")));
        assert!(form.contains("cap_sid=sid123"));
        assert!(form.contains("cap_code=abc"));
    }

    #[test]
    fn test_build_login_form_encodes_special_chars() {
        let form = build_login_form("user@test", "p&ss=w0rd", None);
        assert!(form.contains("login_username=user%40test"));
        assert!(form.contains("login_password=p%26ss%3Dw0rd"));
    }

    #[tokio::test]
    async fn test_ensure_auth_on_dead_task() {
        let (tx, _rx) = mpsc::channel(1);
        let handle = AuthHandle {
            auth_tx: tx,
            http_client: reqwest::Client::new(),
            captcha_state: Arc::new(RwLock::new(None)),
        };
        drop(_rx);
        let result = handle.ensure_authenticated().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_multiple_invalidate_does_not_panic() {
        let handle = spawn_auth_task(test_config());
        handle.invalidate();
        handle.invalidate();
        handle.invalidate();
    }

    #[test]
    fn test_client_is_accessible() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let handle = rt.block_on(async { spawn_auth_task(test_config()) });
        // Client should be usable
        let _client = handle.client();
    }
}
