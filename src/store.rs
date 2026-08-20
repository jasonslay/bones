use crate::game::Room;
use crate::plugin::NetChannels;
use futures_util::StreamExt;
use redis::AsyncCommands;
use redis::Client;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;

/// Room and seat keys expire this long after the last Redis write.
const ROOM_TTL_SECS: u64 = 3_600;
const CHANNEL: &str = "bones:rooms";

const SAVE_LUA: &str = r#"
if ARGV[2] == 'new' then
  local ok = redis.call('SET', KEYS[1], ARGV[1], 'NX', 'EX', tonumber(ARGV[3]))
  if not ok then return 0 end
else
  local raw = redis.call('GET', KEYS[1])
  if not raw then return 0 end
  local obj = cjson.decode(raw)
  if tostring(obj.version) ~= ARGV[2] then return 0 end
  redis.call('SET', KEYS[1], ARGV[1], 'EX', tonumber(ARGV[3]))
end
redis.call('PUBLISH', KEYS[2], ARGV[4])
local ttl = tonumber(ARGV[3])
local code = cjson.decode(ARGV[1]).code
for i = 3, #KEYS do
  redis.call('SET', KEYS[i], code, 'EX', ttl)
end
return 1
"#;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StoreEvent {
    Upsert { room: Room },
    Delete { code: String },
}

#[derive(Clone)]
pub struct Store {
    client: Client,
    conn: ConnectionManager,
    handle: tokio::runtime::Handle,
}

impl Store {
    pub async fn connect(url: &str, handle: tokio::runtime::Handle) -> redis::RedisResult<Self> {
        let client = Client::open(url)?;
        let conn = ConnectionManager::new(client.clone()).await?;
        Ok(Self {
            client,
            conn,
            handle,
        })
    }

    pub async fn ping_async(&self) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;
        Ok(())
    }

    pub fn get(&self, code: &str) -> Result<Option<Room>, String> {
        self.handle
            .block_on(self.get_async(code))
            .map_err(|e| e.to_string())
    }

    pub fn create(&self, room: &Room) -> Result<bool, String> {
        self.handle
            .block_on(self.save_async(room, None))
            .map_err(|e| e.to_string())
    }

    pub fn update(&self, room: &mut Room) -> Result<bool, String> {
        let expected = room.version;
        room.version += 1;
        match self.handle.block_on(self.save_async(room, Some(expected))) {
            Ok(true) => Ok(true),
            Ok(false) => {
                room.version = expected;
                Ok(false)
            }
            Err(err) => {
                room.version = expected;
                Err(err.to_string())
            }
        }
    }

    pub fn delete(&self, code: &str) -> Result<(), String> {
        self.handle
            .block_on(self.delete_async(code))
            .map_err(|e| e.to_string())
    }

    pub fn get_seat(&self, seat_key: Uuid) -> Result<Option<String>, String> {
        self.handle
            .block_on(self.get_seat_async(seat_key))
            .map_err(|e| e.to_string())
    }

    pub fn set_seat(&self, seat_key: Uuid, code: &str) -> Result<(), String> {
        self.handle
            .block_on(self.set_seat_async(seat_key, code))
            .map_err(|e| e.to_string())
    }

    pub fn del_seat(&self, seat_key: Uuid) -> Result<(), String> {
        self.handle
            .block_on(self.del_seat_async(seat_key))
            .map_err(|e| e.to_string())
    }

    pub fn scan_all(&self) -> Result<Vec<Room>, String> {
        self.handle
            .block_on(self.scan_all_async())
            .map_err(|e| e.to_string())
    }

    pub async fn subscribe(
        &self,
        channels: NetChannels,
        mut shutdown: watch::Receiver<bool>,
        ready: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> redis::RedisResult<()> {
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub.subscribe(CHANNEL).await?;
        tracing::info!("subscribed to {CHANNEL}");
        if let Some(ready) = ready {
            let _ = ready.send(());
        }
        let mut messages = pubsub.on_message();
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break;
                    }
                }
                msg = messages.next() => {
                    let Some(msg) = msg else {
                        break;
                    };
                    let payload: String = msg.get_payload()?;
                    match serde_json::from_str::<StoreEvent>(&payload) {
                        Ok(event) => {
                            if let Ok(mut q) = channels.remote_events.lock() {
                                q.push(event);
                            }
                            channels.notify_bevy();
                        }
                        Err(err) => tracing::warn!("bad store event: {err}"),
                    }
                }
            }
        }
        Ok(())
    }

    fn room_key(code: &str) -> String {
        format!("bones:room:{code}")
    }

    fn seat_key(seat: Uuid) -> String {
        format!("bones:seat:{seat}")
    }

    async fn get_async(&self, code: &str) -> redis::RedisResult<Option<Room>> {
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::room_key(code)).await?;
        Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
    }

    async fn save_async(&self, room: &Room, expected: Option<u64>) -> redis::RedisResult<bool> {
        let json = serde_json::to_string(room).map_err(|e| {
            redis::RedisError::from((redis::ErrorKind::Client, "room json", e.to_string()))
        })?;
        let event =
            serde_json::to_string(&StoreEvent::Upsert { room: room.clone() }).map_err(|e| {
                redis::RedisError::from((redis::ErrorKind::Client, "event json", e.to_string()))
            })?;
        let expected = match expected {
            None => "new".to_string(),
            Some(v) => v.to_string(),
        };
        let mut conn = self.conn.clone();
        let script = redis::Script::new(SAVE_LUA);
        let mut invoke = script.prepare_invoke();
        invoke.key(Self::room_key(&room.code));
        invoke.key(CHANNEL);
        for player in &room.players {
            invoke.key(Self::seat_key(player.seat_key));
        }
        invoke.arg(json);
        invoke.arg(expected);
        invoke.arg(ROOM_TTL_SECS);
        invoke.arg(event);
        let saved: i32 = invoke.invoke_async(&mut conn).await?;
        Ok(saved == 1)
    }

    async fn delete_async(&self, code: &str) -> redis::RedisResult<()> {
        let event = serde_json::to_string(&StoreEvent::Delete {
            code: code.to_string(),
        })
        .map_err(|e| {
            redis::RedisError::from((redis::ErrorKind::Client, "event json", e.to_string()))
        })?;
        let mut conn = self.conn.clone();
        let _: () = conn.del(Self::room_key(code)).await?;
        let _: i32 = conn.publish(CHANNEL, event).await?;
        Ok(())
    }

    async fn get_seat_async(&self, seat: Uuid) -> redis::RedisResult<Option<String>> {
        let mut conn = self.conn.clone();
        conn.get(Self::seat_key(seat)).await
    }

    async fn set_seat_async(&self, seat: Uuid, code: &str) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        conn.set_ex(Self::seat_key(seat), code, ROOM_TTL_SECS).await
    }

    async fn del_seat_async(&self, seat: Uuid) -> redis::RedisResult<()> {
        let mut conn = self.conn.clone();
        conn.del(Self::seat_key(seat)).await
    }

    async fn scan_all_async(&self) -> redis::RedisResult<Vec<Room>> {
        let mut conn = self.conn.clone();
        let mut cursor: u64 = 0;
        let mut rooms = Vec::new();
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg("bones:room:*")
                .arg("COUNT")
                .arg(64)
                .query_async(&mut conn)
                .await?;
            for key in keys {
                let raw: Option<String> = conn.get(key).await?;
                if let Some(raw) = raw {
                    if let Ok(room) = serde_json::from_str(&raw) {
                        rooms.push(room);
                    }
                }
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        Ok(rooms)
    }
}

pub async fn connect_with_retry(url: &str, handle: tokio::runtime::Handle) -> Store {
    let mut delay = Duration::from_millis(200);
    loop {
        match Store::connect(url, handle.clone()).await {
            Ok(store) => {
                tracing::info!("connected to redis");
                return store;
            }
            Err(err) => {
                tracing::warn!("redis connect failed: {err}; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Player;

    fn player(name: &str) -> Player {
        Player {
            id: Uuid::new_v4(),
            seat_key: Uuid::new_v4(),
            name: name.into(),
            score: 0,
            on_board: false,
            connected: true,
            forfeited: false,
        }
    }

    #[test]
    #[ignore = "needs Redis; run with REDIS_URL=redis://127.0.0.1:6379/"]
    fn create_update_and_conflict() {
        let url = match std::env::var("REDIS_URL") {
            Ok(url) if !url.is_empty() => url,
            _ => return,
        };
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let handle = rt.handle().clone();
        let store = rt.block_on(Store::connect(&url, handle)).expect("redis");
        let mut room = Room::new("TSTRS".into(), player("Host"));
        assert!(store.create(&room).unwrap());
        assert!(!store.create(&room).unwrap());
        assert!(store.update(&mut room).unwrap());
        assert_eq!(room.version, 1);
        let mut stale = store.get("TSTRS").unwrap().unwrap();
        stale.version = 0;
        assert!(!store.update(&mut stale).unwrap());
        store.delete("TSTRS").unwrap();
        assert!(store.get("TSTRS").unwrap().is_none());
    }
}
