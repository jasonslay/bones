use crate::game::{GameRooms, Player, Room, generate_code};
use crate::protocol::{ClientMessage, GamePhase, ServerMessage, invite_path};
use bevy::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use uuid::Uuid;

pub type OutboundMap = Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<ServerMessage>>>>;
pub type PlayerRooms = Arc<Mutex<HashMap<Uuid, String>>>;

#[derive(Resource, Clone)]
pub struct NetChannels {
    pub commands: Arc<Mutex<Vec<NetCommand>>>,
    pub outbound: OutboundMap,
    pub player_rooms: PlayerRooms,
    pub disconnects: Arc<Mutex<Vec<Uuid>>>,
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
            (process_net_commands, check_forfeit_timeouts).chain(),
        );
    }
}

fn process_net_commands(world: &mut World) {
    let channels = world.resource::<NetChannels>().clone();
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
            if let Some(code) = find_room_for_seat(world, seat_key) {
                join_or_reclaim(world, channels, player_id, &code, name, seat_key);
                return;
            }
            let code = unique_code(world);
            let player = Player {
                id: player_id,
                seat_key,
                name,
                score: 0,
                on_board: false,
                connected: true,
                forfeited: false,
            };
            let room = Room::new(code.clone(), player);
            let entity = world.spawn(room).id();
            world
                .resource_mut::<GameRooms>()
                .by_code
                .insert(code.clone(), entity);

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
    }
}

fn find_room_for_seat(world: &World, seat_key: Uuid) -> Option<String> {
    let rooms = world.resource::<GameRooms>();
    for (code, entity) in &rooms.by_code {
        if let Some(room) = world.get::<Room>(*entity) {
            if room.seat_index(seat_key).is_some() {
                return Some(code.clone());
            }
        }
    }
    None
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
    }
    broadcast_room(world, channels, &code);
}

fn unique_code(world: &World) -> String {
    let rooms = world.resource::<GameRooms>();
    loop {
        let code = generate_code();
        if !rooms.by_code.contains_key(&code) {
            return code;
        }
    }
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
            dirty.push(code);
        }
    }
    for code in dirty {
        broadcast_room(world, &channels, &code);
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
    broadcast_room(world, channels, &code);
}
