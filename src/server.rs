use crate::plugin::{NetChannels, NetCommand, OutboundMap, PlayerRooms};
use crate::protocol::{ClientMessage, ServerMessage};
use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tower_http::services::ServeDir;
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub channels: NetChannels,
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
    let index_html = std::fs::read_to_string(web_dir.join("index.html"))
        .unwrap_or_else(|_| "<p>Bones UI missing — run from the project root.</p>".into());
    let state = AppState { channels };

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/g/{code}", get({
            let page = index_html.clone();
            move || {
                let page = page.clone();
                async move { Html(page) }
            }
        }))
        .fallback_service(ServeDir::new(&web_dir))
        .with_state(state);

    tracing::info!("Bones listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
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
