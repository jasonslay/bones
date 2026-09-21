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
const PUSHOVER_MAX: usize = 1024;
const USER_MAX: usize = 400;

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
    #[serde(default)]
    pub snapshot: Option<ReportSnapshot>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ReportSnapshot {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub phase: String,
    #[serde(default)]
    pub mode: String,
    pub board_threshold: Option<u32>,
    pub idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub players: Vec<ReportPlayer>,
    #[serde(default)]
    pub dice: Vec<u8>,
    #[serde(default)]
    pub selected: Vec<usize>,
    #[serde(default)]
    pub turn_points: u32,
    #[serde(default)]
    pub awaiting_keep: bool,
    #[serde(default)]
    pub bust: bool,
    #[serde(default)]
    pub steal_available: bool,
    #[serde(default)]
    pub you_can_act: bool,
    pub pending_bank: Option<ReportPendingBank>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub history: Vec<String>,
    #[serde(default)]
    pub winner: String,
    #[serde(default)]
    pub reconnecting: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct ReportPlayer {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub score: u32,
    #[serde(default)]
    pub on_board: bool,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub forfeited: bool,
    #[serde(default)]
    pub you: bool,
    #[serde(default)]
    pub host: bool,
    #[serde(default)]
    pub current: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct ReportPendingBank {
    #[serde(default)]
    pub points: u32,
    #[serde(default)]
    pub leftover: usize,
    #[serde(default)]
    pub name: String,
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

fn idle_label(secs: Option<u64>) -> String {
    match secs {
        None => "idle off".into(),
        Some(s) if s.is_multiple_of(60) => {
            let mins = s / 60;
            if mins == 1 {
                "idle 1m".into()
            } else {
                format!("idle {mins}m")
            }
        }
        Some(s) => format!("idle {s}s"),
    }
}

fn format_player(p: &ReportPlayer) -> String {
    let name = clip(&p.name, 20);
    let mut tags = Vec::new();
    if p.you {
        tags.push("you");
    }
    if p.host {
        tags.push("host");
    }
    if p.current {
        tags.push("turn");
    }
    if p.forfeited {
        tags.push("out");
    } else if !p.connected {
        tags.push("away");
    }
    let board = if p.on_board { "on" } else { "off" };
    if tags.is_empty() {
        format!("{name} {} {board}", p.score)
    } else {
        format!("{name} {} {board} ({})", p.score, tags.join(" "))
    }
}

fn format_snapshot(snap: &ReportSnapshot) -> String {
    let mode = match snap.mode.as_str() {
        "farkle" => "Farkle",
        "bones" => "Bones",
        other if !other.is_empty() => other,
        _ => "",
    };
    if mode.is_empty()
        && snap.players.is_empty()
        && snap.dice.is_empty()
        && snap.status.is_empty()
        && snap.history.is_empty()
    {
        return String::new();
    }

    let mut lines = Vec::new();
    let mut settings = Vec::new();
    if !mode.is_empty() {
        settings.push(mode.to_string());
    }
    if let Some(n) = snap.board_threshold {
        settings.push(format!("board {n}"));
    }
    settings.push(idle_label(snap.idle_timeout_secs));
    if snap.reconnecting {
        settings.push("reconnecting".into());
    }
    if !settings.is_empty() {
        lines.push(settings.join(" · "));
    }
    if !snap.players.is_empty() {
        lines.push(
            snap.players
                .iter()
                .map(format_player)
                .collect::<Vec<_>>()
                .join("; "),
        );
    }
    let mut table = Vec::new();
    if !snap.dice.is_empty() {
        table.push(format!(
            "dice {}",
            snap.dice
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    if !snap.selected.is_empty() {
        table.push(format!(
            "sel {}",
            snap.selected
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        ));
    }
    if snap.turn_points > 0 {
        table.push(format!("turn {}", snap.turn_points));
    }
    if snap.bust {
        table.push("bust".into());
    }
    if snap.awaiting_keep {
        table.push("keep".into());
    }
    if snap.you_can_act {
        table.push("can act".into());
    }
    if snap.steal_available {
        table.push("steal open".into());
    }
    if !table.is_empty() {
        lines.push(table.join(" · "));
    }
    if let Some(bank) = &snap.pending_bank {
        let who = clip(&bank.name, 20);
        let mut line = format!("pending {} leftover {}", bank.points, bank.leftover);
        if !who.is_empty() {
            line.push_str(" (");
            line.push_str(&who);
            line.push(')');
        }
        lines.push(line);
    }
    let winner = clip(&snap.winner, 20);
    if !winner.is_empty() {
        lines.push(format!("winner {winner}"));
    }
    let status = clip(&snap.status, 160);
    if !status.is_empty() {
        lines.push(format!("now {status}"));
    }
    let mut history: Vec<String> = snap
        .history
        .iter()
        .rev()
        .take(8)
        .map(|line| clip(line, 100))
        .filter(|line| !line.is_empty() && *line != status)
        .collect();
    history.reverse();
    if !history.is_empty() {
        lines.push("recent:".into());
        for line in history {
            lines.push(format!("- {line}"));
        }
    }
    lines.join("\n")
}

fn assemble_body(
    user: &str,
    name: &str,
    code: &str,
    phase: &str,
    url: &str,
    snapshot: Option<&ReportSnapshot>,
) -> String {
    let user = clip(user, USER_MAX);
    let mut header = String::from("— ");
    header.push_str(if name.is_empty() { "someone" } else { name });
    if !code.is_empty() {
        header.push_str(" · room ");
        header.push_str(code);
    }
    if !phase.is_empty() {
        header.push_str(" · ");
        header.push_str(phase);
    }
    if !url.is_empty() {
        header.push('\n');
        header.push_str(url);
    }
    let mut body = user;
    body.push_str("\n\n");
    body.push_str(&header);
    if let Some(snap) = snapshot {
        let formatted = format_snapshot(snap);
        let used = body.chars().count() + 2;
        let budget = PUSHOVER_MAX.saturating_sub(used);
        let formatted = clip(&formatted, budget);
        if !formatted.is_empty() {
            body.push_str("\n\n");
            body.push_str(&formatted);
        }
    }
    clip(&body, PUSHOVER_MAX)
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
    let mut code = clip(&body.code, 12).to_uppercase();
    let mut phase = clip(&body.phase, 24);
    if let Some(snap) = body.snapshot.as_ref() {
        if code.is_empty() {
            code = clip(&snap.code, 12).to_uppercase();
        }
        if phase.is_empty() {
            phase = clip(&snap.phase, 24);
        }
    }
    let page = clip(&body.url, 200);
    tracing::info!(%code, %phase, %ip, ua, "bug report");
    let full = assemble_body(
        &message,
        &name,
        &code,
        &phase,
        &page,
        body.snapshot.as_ref(),
    );

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

    fn sample_snapshot() -> ReportSnapshot {
        ReportSnapshot {
            code: "ABC12".into(),
            phase: "playing".into(),
            mode: "bones".into(),
            board_threshold: Some(1000),
            idle_timeout_secs: Some(600),
            players: vec![
                ReportPlayer {
                    name: "Jason".into(),
                    score: 2400,
                    on_board: true,
                    connected: true,
                    you: true,
                    host: true,
                    current: true,
                    ..Default::default()
                },
                ReportPlayer {
                    name: "Sam".into(),
                    score: 1100,
                    on_board: false,
                    connected: false,
                    ..Default::default()
                },
            ],
            dice: vec![1, 5, 2, 3, 6],
            selected: vec![0, 1],
            turn_points: 150,
            awaiting_keep: true,
            you_can_act: true,
            status: "Select scoring dice".into(),
            history: vec![
                "lobby · Waiting for players".into(),
                "playing · Jason's turn — roll when ready".into(),
                "playing · Select scoring dice".into(),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn snapshot_includes_scores_dice_and_recent_status() {
        let text = format_snapshot(&sample_snapshot());
        assert!(text.contains("Bones · board 1000 · idle 10m"));
        assert!(text.contains("Jason 2400 on (you host turn)"));
        assert!(text.contains("Sam 1100 off (away)"));
        assert!(text.contains("dice 1 5 2 3 6"));
        assert!(text.contains("sel 0,1"));
        assert!(text.contains("turn 150"));
        assert!(text.contains("now Select scoring dice"));
        assert!(text.contains("recent:"));
        assert!(text.contains("Waiting for players"));
    }

    #[test]
    fn report_body_keeps_user_text_and_fits_pushover() {
        let body = assemble_body(
            "Dice froze after a steal.",
            "Jason",
            "ABC12",
            "playing",
            "https://bones.jtslay.com/g/ABC12",
            Some(&sample_snapshot()),
        );
        assert!(body.starts_with("Dice froze after a steal."));
        assert!(body.contains("room ABC12"));
        assert!(body.contains("dice 1 5 2 3 6"));
        assert!(body.chars().count() <= 1024);
    }
}
