use crate::server::AppState;
use axum::Json;
use axum::extract::State;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{HeaderMap, StatusCode, header};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PUSHOVER_URL: &str = "https://api.pushover.net/1/messages.json";
const MIN_LEN: usize = 8;
const MAX_LEN: usize = 1000;
const COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct Pushover {
    token: String,
    user: String,
    http: reqwest::Client,
    limiter: Arc<Mutex<HashMap<String, Instant>>>,
}

impl Pushover {
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("PUSHOVER_API_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;
        let user = std::env::var("PUSHOVER_USER_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .ok()?;
        Some(Self {
            token,
            user,
            http,
            limiter: Arc::new(Mutex::new(HashMap::new())),
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct ReportRequest {
    pub message: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub url: String,
}

pub fn sanitize_message(raw: &str) -> Result<String, &'static str> {
    let cleaned: String = raw
        .chars()
        .filter(|c| *c == '\n' || *c == '\t' || !c.is_control())
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() < MIN_LEN {
        return Err("Say a bit more — at least a short sentence.");
    }
    if trimmed.chars().count() > MAX_LEN {
        return Err("Keep it under 1,000 characters.");
    }
    Ok(trimmed.to_string())
}

fn clip(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

pub fn client_ip(headers: &HeaderMap, addr: SocketAddr) -> String {
    headers
        .get("cf-connecting-ip")
        .or_else(|| headers.get("x-real-ip"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(',').next().unwrap_or(s).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| addr.ip().to_string())
}

fn allow(limiter: &Mutex<HashMap<String, Instant>>, ip: &str) -> bool {
    let now = Instant::now();
    let mut map = limiter.lock().expect("limiter");
    map.retain(|_, at| now.saturating_duration_since(*at) < Duration::from_secs(3600));
    if let Some(prev) = map.get(ip)
        && now.saturating_duration_since(*prev) < COOLDOWN
    {
        return false;
    }
    map.insert(ip.to_string(), now);
    true
}

#[derive(Serialize)]
struct PushoverPayload<'a> {
    token: &'a str,
    user: &'a str,
    title: &'a str,
    message: String,
}

pub async fn handle(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<ReportRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let Some(pushover) = state.pushover.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "Bug reports are not configured on this server."
            })),
        );
    };
    let message = match sanitize_message(&body.message) {
        Ok(m) => m,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": err })),
            );
        }
    };
    let ip = client_ip(&headers, addr);
    if !allow(&pushover.limiter, &ip) {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "Hang on — try again in a minute."
            })),
        );
    }

    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let name = clip(&body.name, 40);
    let code = clip(&body.code, 12).to_uppercase();
    let phase = clip(&body.phase, 24);
    let page = clip(&body.url, 200);

    let mut full = message;
    full.push_str("\n\n— ");
    full.push_str(if name.is_empty() { "someone" } else { &name });
    if !code.is_empty() {
        full.push_str(" · room ");
        full.push_str(&code);
    }
    if !phase.is_empty() {
        full.push_str(" · ");
        full.push_str(&phase);
    }
    if !page.is_empty() {
        full.push('\n');
        full.push_str(&page);
    }
    if !ua.is_empty() {
        full.push('\n');
        full.push_str(&clip(ua, 180));
    }
    let full = clip(&full, 1024);

    let payload = PushoverPayload {
        token: &pushover.token,
        user: &pushover.user,
        title: "Bones bug",
        message: full,
    };
    match pushover.http.post(PUSHOVER_URL).json(&payload).send().await {
        Ok(resp) if resp.status().is_success() => {
            (StatusCode::OK, Json(serde_json::json!({ "ok": true })))
        }
        Ok(resp) => {
            tracing::error!("pushover rejected report: {}", resp.status());
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "Could not send the report." })),
            )
        }
        Err(err) => {
            tracing::error!("pushover request failed: {err}");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": "Could not send the report." })),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn rejects_short_and_empty() {
        assert!(sanitize_message("   hi  ").is_err());
        assert!(sanitize_message("").is_err());
    }

    #[test]
    fn accepts_a_sentence() {
        let msg = sanitize_message("  Dice froze after a steal.  ").unwrap();
        assert_eq!(msg, "Dice froze after a steal.");
    }

    #[test]
    fn strips_control_chars() {
        let msg = sanitize_message("It broke\u{0007} on bank.\nTwice.").unwrap();
        assert_eq!(msg, "It broke on bank.\nTwice.");
    }

    #[test]
    fn prefers_cloudflare_connecting_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", HeaderValue::from_static("203.0.113.9"));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 1234);
        assert_eq!(client_ip(&headers, addr), "203.0.113.9");
    }
}
