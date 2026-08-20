use crate::game::{GameRooms, Player, Room, generate_code};
use crate::protocol::{ClientMessage, GamePhase, ServerMessage, invite_path};
use crate::store::{Store, StoreEvent};
use bevy::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{mpsc, watch};
use uuid::Uuid;

pub type OutboundMap = Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<ServerMessage>>>>;
pub type PlayerRooms = Arc<Mutex<HashMap<Uuid, String>>>;

#[derive(Resource, Clone)]
pub struct NetChannels {
    pub commands: Arc<Mutex<Vec<NetCommand>>>,
    pub outbound: OutboundMap,
    pub player_rooms: PlayerRooms,
    pub disconnects: Arc<Mutex<Vec<Uuid>>>,
    pub remote_events: Arc<Mutex<Vec<StoreEvent>>>,
    pub shutdown: watch::Sender<bool>,
    pub store: Option<Store>,
    pub bevy_tick_ms: Arc<AtomicU64>,
}

pub struct NetCommand {
    pub player_id: Uuid,
    pub msg: ClientMessage,
}

pub struct BonesGamePlugin;

impl Plugin for BonesGamePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameRooms>().add_systems(
            Update,
            (
                touch_heartbeat,
                process_net_commands,
                check_forfeit_timeouts,
                exit_on_shutdown,
            )
                .chain(),
        );
    }
}

fn process_net_commands(world: &mut World) {
    let channels = world.resource::<NetChannels>().clone();
    apply_remote_events(world, &channels);

    let mut batch = Vec::new();
    if let Ok(mut guard) = channels.commands.lock() {
        batch.append(&mut *guard);
    }

    let mut disconnected = Vec::new();
    if let Ok(mut guard) = channels.disconnects.lock() {
        disconnected.append(&mut *guard);
    }
    for player_id in disconnected {
        mark_disconnected(world, &channels, player_id);
    }

    for cmd in batch {
        handle_command(world, &channels, cmd.player_id, cmd.msg);
    }
}

fn handle_command(world: &mut World, channels: &NetChannels, player_id: Uuid, msg: ClientMessage) {
    match msg {
        ClientMessage::CreateGame { name, seat_key } => {
            let name = sanitize_name(&name);
            vacate_seat(world, channels, seat_key, None);
            let player = Player {
                id: player_id,
                seat_key,
                name,
                score: 0,
                on_board: false,
                connected: true,
                forfeited: false,
            };
            let Some(code) = insert_new_room(world, channels, player) else {
                send(
                    channels,
                    player_id,
                    ServerMessage::Error {
                        message: "Could not create a room".into(),
                    },
                );
                return;
            };
            track_membership(channels, player_id, &code);
            send(
                channels,
                player_id,
                ServerMessage::GameCreated {
                    code: code.clone(),
                    player_id,
                    invite_path: invite_path(&code),
                },
            );
            broadcast_room(world, channels, &code);
        }
        ClientMessage::JoinGame {
            code,
            name,
            seat_key,
        } => {
            let code = code.trim().to_uppercase();
            let name = sanitize_name(&name);
            vacate_seat(world, channels, seat_key, Some(&code));
            join_or_reclaim(world, channels, player_id, &code, name, seat_key);
        }
        ClientMessage::StartGame => {
            with_room_mut(world, channels, player_id, |room| {
                if player_id != room.host_id {
                    Err("Only the host can start".into())
                } else {
                    room.start()
                }
            });
        }
        ClientMessage::Roll { indices } => {
            with_room_mut(world, channels, player_id, |room| {
                room.roll(player_id, indices)
            });
        }
        ClientMessage::Select { indices } => {
            with_room_mut(world, channels, player_id, |room| {
                room.select(player_id, indices)
            });
        }
        ClientMessage::Keep { indices } => {
            with_room_mut(world, channels, player_id, |room| {
                room.keep(player_id, indices)
            });
        }
        ClientMessage::Bank { indices } => {
            with_room_mut(world, channels, player_id, |room| {
                room.bank(player_id, indices)
            });
        }
        ClientMessage::Steal => {
            with_room_mut(world, channels, player_id, |room| room.steal(player_id));
        }
        ClientMessage::DeclineSteal => {
            with_room_mut(world, channels, player_id, |room| {
                room.decline_steal(player_id)
            });
        }
        ClientMessage::EndGame => {
            with_room_mut(world, channels, player_id, |room| room.end_game(player_id));
        }
        ClientMessage::Forfeit => {
            with_room_mut(world, channels, player_id, |room| {
                room.forfeit(player_id, crate::game::ForfeitCause::Manual)
            });
        }
        ClientMessage::Rematch => {
            with_room_mut(world, channels, player_id, |room| room.rematch(player_id));
        }
        ClientMessage::LeaveGame => {
            leave_player(world, channels, player_id);
        }
        ClientMessage::Pong => {}
    }
}

fn leave_player(world: &mut World, channels: &NetChannels, player_id: Uuid) {
    let seat_key = {
        let code = channels
            .player_rooms
            .lock()
            .ok()
            .and_then(|m| m.get(&player_id).cloned());
        let Some(code) = code else {
            return;
        };
        let entity = world.resource::<GameRooms>().by_code.get(&code).copied();
        let Some(entity) = entity else {
            return;
        };
        world.get::<Room>(entity).and_then(|room| {
            room.player_index(player_id)
                .map(|i| room.players[i].seat_key)
        })
    };
    if let Some(seat_key) = seat_key {
        vacate_seat(world, channels, seat_key, None);
    }
}

fn vacate_seat(
    world: &mut World,
    channels: &NetChannels,
    seat_key: Uuid,
    keep_code: Option<&str>,
) {
    if let Some(store) = &channels.store {
        if let Ok(Some(code)) = store.get_seat(seat_key) {
            if keep_code != Some(code.as_str()) {
                let _ = load_store_room(world, store, &code);
            }
        }
    }

    let targets: Vec<(String, Entity)> = world
        .resource::<GameRooms>()
        .by_code
        .iter()
        .filter(|(code, _)| keep_code != Some(code.as_str()))
        .map(|(code, entity)| (code.clone(), *entity))
        .collect();

    let mut empty = Vec::new();
    let mut dirty = Vec::new();
    for (code, entity) in targets {
        let Some(mut room) = world.get_mut::<Room>(entity) else {
            continue;
        };
        let Some((old_id, is_empty)) = room.vacate_seat(seat_key) else {
            continue;
        };
        if is_empty {
            empty.push((code.clone(), entity));
        } else {
            dirty.push(code.clone());
        }
        drop(room);
        if let Ok(mut map) = channels.player_rooms.lock() {
            if map.get(&old_id).is_some_and(|c| c == &code) {
                map.remove(&old_id);
            }
        }
        if let Some(store) = &channels.store {
            let _ = store.del_seat(seat_key);
        }
    }

    if !empty.is_empty() {
        let mut rooms = world.resource_mut::<GameRooms>();
        for (code, _) in &empty {
            rooms.by_code.remove(code);
        }
    }
    for (code, entity) in &empty {
        if let Some(store) = &channels.store {
            if let Err(err) = store.delete(code) {
                tracing::warn!("redis delete {code}: {err}");
            }
        }
        world.despawn(*entity);
    }
    for code in dirty {
        persist_existing(world, channels, &code);
        broadcast_room(world, channels, &code);
    }
}

fn track_membership(channels: &NetChannels, player_id: Uuid, code: &str) {
    if let Ok(mut map) = channels.player_rooms.lock() {
        map.insert(player_id, code.to_string());
    }
}

fn join_or_reclaim(
    world: &mut World,
    channels: &NetChannels,
    player_id: Uuid,
    code: &str,
    name: String,
    seat_key: Uuid,
) {
    if let Some(store) = &channels.store {
        match load_store_room(world, store, code) {
            Ok(Some(_)) => {}
            Ok(None) => {
                send(
                    channels,
                    player_id,
                    ServerMessage::Error {
                        message: "Game not found".into(),
                    },
                );
                return;
            }
            Err(err) => {
                send(channels, player_id, ServerMessage::Error { message: err });
                return;
            }
        }
    }

    let entity = {
        let rooms = world.resource::<GameRooms>();
        rooms.by_code.get(code).copied()
    };
    let Some(entity) = entity else {
        send(
            channels,
            player_id,
            ServerMessage::Error {
                message: "Game not found".into(),
            },
        );
        return;
    };

    let result = {
        let mut room = world.get_mut::<Room>(entity).unwrap();
        if room.seat_index(seat_key).is_some() {
            room.reclaim_seat(seat_key, player_id, name).map(|old_id| {
                if let Ok(mut map) = channels.player_rooms.lock() {
                    map.remove(&old_id);
                }
            })
        } else if room.phase == GamePhase::Lobby {
            if room.players.len() >= 8 {
                Err("Game is full".into())
            } else {
                room.players.push(Player {
                    id: player_id,
                    seat_key,
                    name,
                    score: 0,
                    on_board: false,
                    connected: true,
                    forfeited: false,
                });
                room.status_message = format!(
                    "{} joined — {} player(s).",
                    room.players.last().unwrap().name,
                    room.players.len()
                );
                Ok(())
            }
        } else {
            Err("Game already in progress".into())
        }
    };

    if let Err(message) = result {
        send(channels, player_id, ServerMessage::Error { message });
        return;
    }

    if let Some(store) = &channels.store {
        if let Err(err) = store.set_seat(seat_key, code) {
            tracing::warn!("redis seat: {err}");
        }
    }
    persist_existing(world, channels, code);
    track_membership(channels, player_id, code);
    send(
        channels,
        player_id,
        ServerMessage::Joined {
            code: code.to_string(),
            player_id,
            invite_path: invite_path(code),
        },
    );
    broadcast_room(world, channels, code);
}

fn with_room_mut(
    world: &mut World,
    channels: &NetChannels,
    player_id: Uuid,
    f: impl FnOnce(&mut Room) -> Result<(), String>,
) {
    let code = {
        channels
            .player_rooms
            .lock()
            .ok()
            .and_then(|m| m.get(&player_id).cloned())
    };
    let Some(code) = code else {
        send(
            channels,
            player_id,
            ServerMessage::Error {
                message: "You are not in a game".into(),
            },
        );
        return;
    };
    if let Some(store) = &channels.store {
        match load_store_room(world, store, &code) {
            Ok(Some(_)) => {}
            Ok(None) => {
                send(
                    channels,
                    player_id,
                    ServerMessage::Error {
                        message: "Game not found".into(),
                    },
                );
                return;
            }
            Err(err) => {
                send(channels, player_id, ServerMessage::Error { message: err });
                return;
            }
        }
    }
    let entity = {
        let rooms = world.resource::<GameRooms>();
        rooms.by_code.get(&code).copied()
    };
    let Some(entity) = entity else {
        send(
            channels,
            player_id,
            ServerMessage::Error {
                message: "Game not found".into(),
            },
        );
        return;
    };

    let result = {
        let mut room = world.get_mut::<Room>(entity).unwrap();
        f(&mut room)
    };

    if let Err(message) = result {
        send(channels, player_id, ServerMessage::Error { message });
        return;
    }
    persist_existing(world, channels, &code);
    broadcast_room(world, channels, &code);
}

fn sanitize_name(name: &str) -> String {
    let trimmed: String = name.chars().filter(|c| !c.is_control()).take(20).collect();
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        "Player".into()
    } else {
        trimmed.to_string()
    }
}

fn insert_new_room(world: &mut World, channels: &NetChannels, player: Player) -> Option<String> {
    let seat_key = player.seat_key;
    for _ in 0..16 {
        let code = generate_code();
        if world.resource::<GameRooms>().by_code.contains_key(&code) {
            continue;
        }
        let room = Room::new(code.clone(), player.clone());
        if let Some(store) = &channels.store {
            match store.create(&room) {
                Ok(true) => {
                    let _ = store.set_seat(seat_key, &code);
                }
                Ok(false) => continue,
                Err(err) => {
                    tracing::warn!("redis create: {err}");
                    return None;
                }
            }
        }
        let entity = world.spawn(room).id();
        world
            .resource_mut::<GameRooms>()
            .by_code
            .insert(code.clone(), entity);
        return Some(code);
    }
    None
}

fn upsert_room(world: &mut World, room: Room) -> Entity {
    let code = room.code.clone();
    if let Some(entity) = world.resource::<GameRooms>().by_code.get(&code).copied() {
        if let Some(mut existing) = world.get_mut::<Room>(entity) {
            *existing = room;
        }
        entity
    } else {
        let entity = world.spawn(room).id();
        world.resource_mut::<GameRooms>().by_code.insert(code, entity);
        entity
    }
}

fn delete_local_room(world: &mut World, code: &str) {
    if let Some(entity) = world.resource_mut::<GameRooms>().by_code.remove(code) {
        world.despawn(entity);
    }
}

fn load_store_room(
    world: &mut World,
    store: &Store,
    code: &str,
) -> Result<Option<Entity>, String> {
    match store.get(code)? {
        Some(room) => Ok(Some(upsert_room(world, room))),
        None => {
            delete_local_room(world, code);
            Ok(None)
        }
    }
}

fn persist_existing(world: &mut World, channels: &NetChannels, code: &str) {
    let Some(store) = &channels.store else {
        return;
    };
    let Some(entity) = world.resource::<GameRooms>().by_code.get(code).copied() else {
        return;
    };
    let Some(mut room) = world.get_mut::<Room>(entity) else {
        return;
    };
    match store.update(&mut room) {
        Ok(true) => {}
        Ok(false) => {
            drop(room);
            tracing::debug!("redis conflict on {code}; reloading");
            let _ = load_store_room(world, store, code);
        }
        Err(err) => tracing::warn!("redis update {code}: {err}"),
    }
}

fn apply_remote_events(world: &mut World, channels: &NetChannels) {
    let mut events = Vec::new();
    if let Ok(mut q) = channels.remote_events.lock() {
        events.append(&mut *q);
    }
    for event in events {
        match event {
            StoreEvent::Upsert { room } => {
                let code = room.code.clone();
                let incoming = room.version;
                let skip = world
                    .resource::<GameRooms>()
                    .by_code
                    .get(&code)
                    .and_then(|e| world.get::<Room>(*e))
                    .is_some_and(|local| local.version >= incoming);
                upsert_room(world, room);
                if !skip {
                    broadcast_room(world, channels, &code);
                }
            }
            StoreEvent::Delete { code } => delete_local_room(world, &code),
        }
    }
}

fn send(channels: &NetChannels, player_id: Uuid, msg: ServerMessage) {
    if let Ok(map) = channels.outbound.lock() {
        if let Some(tx) = map.get(&player_id) {
            let _ = tx.send(msg);
        }
    }
}

pub fn broadcast_room(world: &World, channels: &NetChannels, code: &str) {
    let entity = {
        let rooms = world.resource::<GameRooms>();
        rooms.by_code.get(code).copied()
    };
    let Some(entity) = entity else {
        return;
    };
    let Some(room) = world.get::<Room>(entity) else {
        return;
    };
    let players: Vec<Uuid> = room.players.iter().map(|p| p.id).collect();
    for pid in players {
        let view = room.view_for(pid);
        send(channels, pid, ServerMessage::State(view));
    }
}

fn check_forfeit_timeouts(world: &mut World) {
    let channels = world.resource::<NetChannels>().clone();
    let now = crate::game::now_ms();
    let rooms: Vec<(String, Entity)> = world
        .resource::<GameRooms>()
        .by_code
        .iter()
        .map(|(code, entity)| (code.clone(), *entity))
        .collect();
    let mut dirty = Vec::new();
    for (code, entity) in rooms {
        let Some(mut room) = world.get_mut::<Room>(entity) else {
            continue;
        };
        if room.check_timeout(now) {
            drop(room);
            persist_existing(world, &channels, &code);
            dirty.push(code);
        }
    }
    for code in dirty {
        broadcast_room(world, &channels, &code);
    }
}

fn touch_heartbeat(channels: Res<NetChannels>) {
    channels
        .bevy_tick_ms
        .store(crate::game::now_ms(), Ordering::Relaxed);
}

fn exit_on_shutdown(channels: Res<NetChannels>, mut exit: MessageWriter<AppExit>) {
    if *channels.shutdown.borrow() {
        exit.write(AppExit::Success);
    }
}

pub fn mark_disconnected(world: &mut World, channels: &NetChannels, player_id: Uuid) {
    let code = {
        channels
            .player_rooms
            .lock()
            .ok()
            .and_then(|m| m.get(&player_id).cloned())
    };
    let Some(code) = code else {
        return;
    };
    let entity = {
        let rooms = world.resource::<GameRooms>();
        rooms.by_code.get(&code).copied()
    };
    let Some(entity) = entity else {
        return;
    };
    if let Some(mut room) = world.get_mut::<Room>(entity) {
        if let Some(idx) = room.player_index(player_id) {
            room.players[idx].connected = false;
            room.status_message = format!("{} disconnected", room.players[idx].name);
        }
    }
    persist_existing(world, channels, &code);
    broadcast_room(world, channels, &code);
}
