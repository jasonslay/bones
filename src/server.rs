use crate::plugin::{NetChannels, NetCommand, OutboundMap, PlayerRooms};
use crate::protocol::{ClientMessage, ServerMessage};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tower_http::services::ServeDir;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub channels: NetChannels,
    pub web_dir: PathBuf,
}

pub fn new_channels() -> NetChannels {
    NetChannels {
        commands: Arc::new(Mutex::new(Vec::new())),
        outbound: Arc::new(Mutex::new(HashMap::new())) as OutboundMap,
        player_rooms: Arc::new(Mutex::new(HashMap::new())) as PlayerRooms,
        disconnects: Arc::new(Mutex::new(Vec::new())),
    }
}

pub async fn serve(channels: NetChannels, web_dir: PathBuf, addr: SocketAddr) {
    let state = AppState {
        channels,
        web_dir: web_dir.clone(),
    };

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_handler))
        .route("/", get(index_page))
        .route("/g/{code}", get(index_page))
        .fallback_service(ServeDir::new(&web_dir))
        .with_state(state);

    tracing::info!("Bones listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}

fn asset_ver(web_dir: &Path) -> String {
    let mut max = 0u64;
    for name in ["app.js", "styles.css", "index.html"] {
        if let Ok(meta) = std::fs::metadata(web_dir.join(name)) {
            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(UNIX_EPOCH) {
                    max = max.max(dur.as_secs());
                }
            }
        }
    }
    if max == 0 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1)
            .to_string()
    } else {
        max.to_string()
    }
}

async fn index_page(State(state): State<AppState>) -> impl IntoResponse {
    let ver = asset_ver(&state.web_dir);
    let page = std::fs::read_to_string(state.web_dir.join("index.html"))
        .unwrap_or_else(|_| "<p>Bones UI missing — run from the project root.</p>".into())
        .replace("href=\"/styles.css\"", &format!("href=\"/styles.css?v={ver}\""))
        .replace("src=\"/app.js\"", &format!("src=\"/app.js?v={ver}\""));
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, Html(page))
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| client_session(socket, state))
}

async fn client_session(socket: WebSocket, state: AppState) {
    let player_id = Uuid::new_v4();
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMessage>();

    {
        let mut map = state.channels.outbound.lock().expect("outbound");
        map.insert(player_id, tx);
    }

    let _ = sink
        .send(Message::Text(
            serde_json::to_string(&ServerMessage::Welcome { player_id })
                .unwrap()
                .into(),
        ))
        .await;

    let outbound = state.channels.outbound.clone();
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(text) = serde_json::to_string(&msg) else {
                continue;
            };
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    while let Some(Ok(msg)) = stream.next().await {
        match msg {
            Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
                Ok(client_msg) => {
                    let mut q = state.channels.commands.lock().expect("commands");
                    q.push(NetCommand {
                        player_id,
                        msg: client_msg,
                    });
                }
                Err(err) => {
                    if let Ok(map) = state.channels.outbound.lock() {
                        if let Some(tx) = map.get(&player_id) {
                            let _ = tx.send(ServerMessage::Error {
                                message: format!("Bad message: {err}"),
                            });
                        }
                    }
                }
            },
            Message::Close(_) => break,
            _ => {}
        }
    }

    write_task.abort();
    if let Ok(mut map) = outbound.lock() {
        map.remove(&player_id);
    }
    if let Ok(mut d) = state.channels.disconnects.lock() {
        d.push(player_id);
    }
}
