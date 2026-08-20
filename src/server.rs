use crate::plugin::{NetChannels, NetCommand, OutboundMap, PlayerRooms};
use crate::protocol::{ClientMessage, ServerMessage, WS_PING_INTERVAL_MS};
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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;
use tower_http::services::ServeDir;
use uuid::Uuid;

/// Time to flush WebSocket close frames after SIGTERM before forcing exit.
const SHUTDOWN_DRAIN: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct AppState {
    pub channels: NetChannels,
    pub web_dir: PathBuf,
}

pub fn new_channels(store: Option<crate::store::Store>) -> NetChannels {
    let (shutdown, _) = watch::channel(false);
    NetChannels {
        commands: Arc::new(Mutex::new(Vec::new())),
        outbound: Arc::new(Mutex::new(HashMap::new())) as OutboundMap,
        player_rooms: Arc::new(Mutex::new(HashMap::new())) as PlayerRooms,
        disconnects: Arc::new(Mutex::new(Vec::new())),
        remote_events: Arc::new(Mutex::new(Vec::new())),
        shutdown,
        store,
    }
}

pub async fn serve(channels: NetChannels, web_dir: PathBuf, addr: SocketAddr) {
    let state = AppState {
        channels: channels.clone(),
        web_dir: web_dir.clone(),
    };

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/ws", get(ws_handler))
        .route("/", get(index_page))
        .route("/g/{code}", get(index_page))
        .fallback_service(ServeDir::new(&web_dir))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    tracing::info!("Bones listening on http://{addr}");

    let mut shutting_down = channels.shutdown.subscribe();
    let shutdown_tx = channels.shutdown.clone();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        wait_for_signal().await;
        tracing::info!("shutdown signal received");
        let _ = shutdown_tx.send(true);
    });

    tokio::select! {
        result = server => {
            if let Err(err) = result {
                tracing::error!("server error: {err}");
            }
        }
        _ = async {
            let _ = shutting_down.changed().await;
            tokio::time::sleep(SHUTDOWN_DRAIN).await;
        } => {
            tracing::info!("shutdown drain complete");
        }
    }
}

async fn wait_for_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        ctrl_c.await.expect("ctrl_c");
    }
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
    let mut shutdown_rx = state.channels.shutdown.subscribe();
    let mut write_shutdown = shutdown_rx.clone();
    let write_task = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(WS_PING_INTERVAL_MS));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            tokio::select! {
                _ = write_shutdown.changed() => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
                msg = rx.recv() => {
                    let Some(msg) = msg else {
                        break;
                    };
                    let Ok(text) = serde_json::to_string(&msg) else {
                        continue;
                    };
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                _ = interval.tick() => {
                    if sink.send(Message::Ping(Default::default())).await.is_err() {
                        break;
                    }
                    let Ok(text) = serde_json::to_string(&ServerMessage::Ping) else {
                        continue;
                    };
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    loop {
        let msg = tokio::select! {
            _ = shutdown_rx.changed() => break,
            msg = stream.next() => msg,
        };
        let Some(Ok(msg)) = msg else {
            break;
        };
        match msg {
            Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
                Ok(ClientMessage::Pong) => {}
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
