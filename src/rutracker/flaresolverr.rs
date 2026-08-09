use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// FlareSolverr solves the Cloudflare JS challenge in a headless browser and
/// hands back the clearance cookies plus the browser's user agent. Requests
/// made with those cookies and that exact user agent (from the same egress IP)
/// pass Cloudflare without further challenges.
const SOLVE_TIMEOUT: Duration = Duration::from_secs(90);
const MAX_TIMEOUT_MS: u64 = 60_000;

pub struct Solution {
    pub cookies: Vec<Cookie>,
    pub user_agent: String,
}

#[derive(Deserialize)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    #[serde(default)]
    pub domain: String,
}

#[derive(Deserialize)]
struct FsResponse {
    status: String,
    #[serde(default)]
    message: String,
    solution: Option<FsSolution>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FsSolution {
    cookies: Vec<Cookie>,
    user_agent: String,
}

/// Ask FlareSolverr to solve the challenge for `target_url`.
pub async fn solve(flaresolverr_url: &str, target_url: &str) -> Result<Solution> {
    let endpoint = format!("{}/v1", flaresolverr_url.trim_end_matches('/'));

    let client = reqwest::Client::builder()
        .timeout(SOLVE_TIMEOUT)
        .build()
        .context("failed to create flaresolverr http client")?;

    let resp = client
        .post(&endpoint)
        .json(&serde_json::json!({
            "cmd": "request.get",
            "url": target_url,
            "maxTimeout": MAX_TIMEOUT_MS,
        }))
        .send()
        .await
        .context("flaresolverr request failed")?;

    let fs: FsResponse = resp
        .json()
        .await
        .context("failed to parse flaresolverr response")?;

    if fs.status != "ok" {
        bail!("flaresolverr failed: {}", fs.message);
    }

    let solution = fs
        .solution
        .context("flaresolverr response has no solution")?;

    Ok(Solution {
        cookies: solution.cookies,
        user_agent: solution.user_agent,
    })
}

/// Check whether a response is a Cloudflare challenge page.
pub fn is_cf_challenge(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get("cf-mitigated")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("challenge"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_solution_response() {
        let json = r#"{
            "status": "ok",
            "message": "Challenge solved!",
            "solution": {
                "url": "https://rutracker.net/forum/login.php",
                "status": 200,
                "cookies": [
                    {"name": "cf_clearance", "value": "abc", "domain": ".rutracker.net"}
                ],
                "userAgent": "Mozilla/5.0 Test"
            }
        }"#;
        let fs: FsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(fs.status, "ok");
        let sol = fs.solution.unwrap();
        assert_eq!(sol.user_agent, "Mozilla/5.0 Test");
        assert_eq!(sol.cookies[0].name, "cf_clearance");
        assert_eq!(sol.cookies[0].domain, ".rutracker.net");
    }

    #[test]
    fn test_parse_error_response() {
        let json = r#"{"status": "error", "message": "timeout"}"#;
        let fs: FsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(fs.status, "error");
        assert!(fs.solution.is_none());
    }
}
