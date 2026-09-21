use crate::protocol::{
    GameMode, GamePhase, GameView, PendingBankView, PlayerView, WIN_SCORE, invite_path,
};
use crate::scoring::{can_keep_die, has_any_score, has_playable_keep, score_dice, score_held};
use bevy::prelude::*;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Resource, Default)]
pub struct GameRooms {
    pub by_code: HashMap<String, Entity>,
}

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct Room {
    pub code: String,
    pub host_id: Uuid,
    pub players: Vec<Player>,
    pub phase: GamePhase,
    #[serde(default)]
    pub mode: GameMode,
    #[serde(default = "default_board_threshold")]
    pub board_threshold: u32,
    /// `None` disables idle forfeit. Missing field on older rooms stays off.
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    pub turn_index: usize,
    pub dice: Vec<u8>,
    pub selected: Vec<usize>,
    pub turn_points: u32,
    pub awaiting_keep: bool,
    pub steal_leftover: usize,
    /// Last roll scored nothing; keep those dice on the table until the next roll.
    pub bust_showing: bool,
    pub pending_bank: Option<PendingBank>,
    pub winner_id: Option<Uuid>,
    pub status_message: String,
    pub action_deadline_ms: Option<u64>,
    #[serde(default)]
    pub version: u64,
}

fn default_board_threshold() -> u32 {
    GameMode::Bones.default_board_threshold()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub id: Uuid,
    pub seat_key: Uuid,
    pub name: String,
    pub score: u32,
    pub on_board: bool,
    pub connected: bool,
    pub forfeited: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingBank {
    pub player_id: Uuid,
    pub points: u32,
    pub leftover: usize,
}

pub fn generate_code() -> String {
    const CHARS: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::rng();
    (0..5)
        .map(|_| CHARS[rng.random_range(0..CHARS.len())] as char)
        .collect()
}

fn roll_n(n: usize) -> Vec<u8> {
    let mut rng = rand::rng();
    (0..n).map(|_| rng.random_range(1..=6)).collect()
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn format_points(n: u32) -> String {
    let digits: Vec<char> = n.to_string().chars().collect();
    let mut out = String::new();
    for (i, ch) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*ch);
    }
    out
}

fn format_duration_secs(secs: u64) -> String {
    if secs < 60 {
        if secs == 1 {
            "1 second".into()
        } else {
            format!("{secs} seconds")
        }
    } else {
        let mins = secs / 60;
        if mins == 1 {
            "1 minute".into()
        } else {
            format!("{mins} minutes")
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForfeitCause {
    Manual,
    Timeout,
}

impl Room {
    pub fn new(code: String, host: Player) -> Self {
        let mode = GameMode::Bones;
        Self {
            code,
            host_id: host.id,
            players: vec![host],
            phase: GamePhase::Lobby,
            mode,
            board_threshold: mode.default_board_threshold(),
            idle_timeout_secs: None,
            turn_index: 0,
            dice: Vec::new(),
            selected: Vec::new(),
            turn_points: 0,
            awaiting_keep: false,
            steal_leftover: 0,
            bust_showing: false,
            pending_bank: None,
            winner_id: None,
            status_message: "Waiting for players… share the invite link.".into(),
            action_deadline_ms: None,
            version: 0,
        }
    }

    pub fn dice_count(&self) -> usize {
        self.mode.dice_count()
    }

    pub fn player_index(&self, id: Uuid) -> Option<usize> {
        self.players.iter().position(|p| p.id == id)
    }

    pub fn seat_index(&self, seat_key: Uuid) -> Option<usize> {
        self.players.iter().position(|p| p.seat_key == seat_key)
    }

    pub fn current_player(&self) -> Option<&Player> {
        self.players.get(self.turn_index)
    }

    pub fn current_player_mut(&mut self) -> Option<&mut Player> {
        self.players.get_mut(self.turn_index)
    }

    pub fn next_player_index(&self) -> usize {
        let n = self.players.len();
        if n == 0 {
            return 0;
        }
        for step in 1..=n {
            let idx = (self.turn_index + step) % n;
            if !self.players[idx].forfeited {
                return idx;
            }
        }
        self.turn_index
    }

    fn active_count(&self) -> usize {
        self.players.iter().filter(|p| !p.forfeited).count()
    }

    pub fn acting_player_id(&self) -> Option<Uuid> {
        match self.phase {
            GamePhase::Playing => self.current_player().filter(|p| !p.forfeited).map(|p| p.id),
            GamePhase::StealWindow => self
                .players
                .get(self.next_player_index())
                .filter(|p| !p.forfeited)
                .map(|p| p.id),
            GamePhase::Lobby | GamePhase::Finished => None,
        }
    }

    fn set_action_deadline(&mut self) {
        self.action_deadline_ms = self
            .idle_timeout_secs
            .map(|secs| now_ms().saturating_add(secs.saturating_mul(1000)));
    }

    fn clear_action_deadline(&mut self) {
        self.action_deadline_ms = None;
    }

    fn turn_hint(&self, on_board: bool) -> String {
        if on_board {
            "roll when ready".into()
        } else {
            format!(
                "need {} in one turn to get on the board",
                format_points(self.board_threshold)
            )
        }
    }

    pub fn reclaim_seat(
        &mut self,
        seat_key: Uuid,
        new_id: Uuid,
        name: String,
    ) -> Result<Uuid, String> {
        let idx = self
            .seat_index(seat_key)
            .ok_or_else(|| "No seat to reclaim".to_string())?;
        let old_id = self.players[idx].id;
        self.players[idx].id = new_id;
        self.players[idx].connected = true;
        self.players[idx].name = name;
        self.retarget_player_id(old_id, new_id);
        self.status_message = format!("{} reconnected", self.players[idx].name);
        Ok(old_id)
    }

    fn retarget_player_id(&mut self, old_id: Uuid, new_id: Uuid) {
        if self.host_id == old_id {
            self.host_id = new_id;
        }
        if self.winner_id == Some(old_id) {
            self.winner_id = Some(new_id);
        }
        if let Some(pending) = &mut self.pending_bank {
            if pending.player_id == old_id {
                pending.player_id = new_id;
            }
        }
    }

    /// Drop this seat from a lobby, or unbind the live connection from a started game.
    /// Returns the connection id that was sitting in the seat.
    pub fn vacate_seat(&mut self, seat_key: Uuid) -> Option<(Uuid, bool)> {
        let idx = self.seat_index(seat_key)?;
        if self.phase == GamePhase::Lobby {
            let removed = self.players.remove(idx);
            if !self.players.is_empty() && self.host_id == removed.id {
                self.host_id = self.players[0].id;
            }
            if !self.players.is_empty() {
                self.status_message =
                    format!("{} left — {} player(s).", removed.name, self.players.len());
            }
            return Some((removed.id, self.players.is_empty()));
        }

        let pid = self.players[idx].id;
        if matches!(self.phase, GamePhase::Playing | GamePhase::StealWindow)
            && !self.players[idx].forfeited
        {
            let _ = self.forfeit(pid, ForfeitCause::Manual);
        }
        self.detach_connection(seat_key).map(|id| (id, false))
    }

    /// Keep the scoreboard row, but stop sending this connection room updates.
    pub fn detach_connection(&mut self, seat_key: Uuid) -> Option<Uuid> {
        let idx = self.seat_index(seat_key)?;
        let old_id = self.players[idx].id;
        let ghost = Uuid::new_v4();
        self.players[idx].id = ghost;
        self.players[idx].connected = false;
        self.retarget_player_id(old_id, ghost);
        Some(old_id)
    }

    pub fn view_for(&self, you: Uuid) -> GameView {
        let you_forfeited = self
            .player_index(you)
            .is_some_and(|i| self.players[i].forfeited);
        let steal_available = matches!(self.phase, GamePhase::StealWindow)
            && self.next_can_steal()
            && self
                .players
                .get(self.next_player_index())
                .is_some_and(|p| p.id == you);

        let you_can_act = !you_forfeited
            && match self.phase {
                GamePhase::Lobby => you == self.host_id && self.players.len() >= 2,
                GamePhase::Playing => {
                    self.current_player().is_some_and(|p| p.id == you) && self.winner_id.is_none()
                }
                GamePhase::StealWindow => steal_available,
                GamePhase::Finished => you == self.host_id,
            };

        GameView {
            code: self.code.clone(),
            invite_path: invite_path(&self.code),
            phase: self.phase,
            mode: self.mode,
            board_threshold: self.board_threshold,
            dice_count: self.dice_count(),
            idle_timeout_secs: self.idle_timeout_secs,
            players: self
                .players
                .iter()
                .map(|p| PlayerView {
                    id: p.id,
                    name: p.name.clone(),
                    score: p.score,
                    on_board: p.on_board,
                    connected: p.connected,
                    forfeited: p.forfeited,
                })
                .collect(),
            current_player_id: self.current_player().map(|p| p.id),
            you_are: you,
            host_id: self.host_id,
            dice: self.dice.clone(),
            selected: self.selected.clone(),
            turn_points: self.turn_points,
            awaiting_keep: self.awaiting_keep,
            bust: self.bust_showing,
            pending_bank: self.pending_bank.as_ref().map(|p| PendingBankView {
                player_id: p.player_id,
                points: p.points,
                leftover: p.leftover,
            }),
            steal_available,
            you_can_act,
            message: self.status_message.clone(),
            winner_id: self.winner_id,
            action_deadline_ms: self.action_deadline_ms,
        }
    }

    pub fn update_settings(
        &mut self,
        mode: GameMode,
        idle_timeout_secs: Option<u64>,
        board_threshold: u32,
    ) -> Result<(), String> {
        if self.phase != GamePhase::Lobby {
            return Err("Settings can only be changed in the lobby".into());
        }
        if let Some(secs) = idle_timeout_secs {
            if !crate::protocol::IDLE_TIMEOUT_OPTIONS_SECS.contains(&secs) {
                return Err("Invalid idle forfeit time".into());
            }
        }
        let board_threshold = if mode != self.mode {
            mode.default_board_threshold()
        } else {
            board_threshold
        };
        if board_threshold > WIN_SCORE {
            return Err(format!(
                "On-the-board minimum cannot exceed {}",
                format_points(WIN_SCORE)
            ));
        }
        self.mode = mode;
        self.board_threshold = board_threshold;
        self.idle_timeout_secs = idle_timeout_secs;
        let idle = match idle_timeout_secs {
            Some(secs) => format!("idle forfeit after {}", format_duration_secs(secs)),
            None => "idle forfeit off".into(),
        };
        let board = if board_threshold == 0 {
            "no minimum to get on the board".into()
        } else {
            format!(
                "need {} to get on the board",
                format_points(board_threshold)
            )
        };
        self.status_message = format!("{} — {board} · {idle}. Waiting for players…", mode.label());
        Ok(())
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.phase != GamePhase::Lobby {
            return Err("Game already started".into());
        }
        if self.players.len() < 2 {
            return Err("Need at least 2 players".into());
        }
        self.phase = GamePhase::Playing;
        self.turn_index = 0;
        self.reset_turn_state();
        self.set_action_deadline();
        let name = self
            .current_player()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        self.status_message = format!("{name}'s turn — {}", self.turn_hint(false));
        Ok(())
    }

    fn reset_turn_state(&mut self) {
        self.dice.clear();
        self.selected.clear();
        self.turn_points = 0;
        self.awaiting_keep = false;
        self.steal_leftover = 0;
        self.bust_showing = false;
    }

    fn begin_next_turn(&mut self, keep_dice: bool) {
        self.pending_bank = None;
        self.phase = GamePhase::Playing;
        self.turn_index = self.next_player_index();
        if keep_dice {
            self.selected.clear();
            self.turn_points = 0;
            self.awaiting_keep = false;
            self.steal_leftover = 0;
            self.bust_showing = true;
        } else {
            self.reset_turn_state();
        }
        if let Some(p) = self.current_player() {
            if !keep_dice {
                self.status_message = format!("{}'s turn — {}", p.name, self.turn_hint(p.on_board));
            }
        }
        self.set_action_deadline();
    }

    fn next_can_steal(&self) -> bool {
        let Some(pending) = &self.pending_bank else {
            return false;
        };
        self.next_can_steal_points(pending.points, pending.leftover)
    }

    fn next_can_steal_points(&self, points: u32, leftover: usize) -> bool {
        if leftover == 0 || points == 0 {
            return false;
        }
        self.players.get(self.next_player_index()).is_some_and(|p| {
            !p.forfeited && p.on_board && p.score.saturating_add(points) <= WIN_SCORE
        })
    }

    fn dice_to_roll(&self) -> usize {
        if self.bust_showing || self.dice.is_empty() {
            self.dice_count()
        } else {
            self.dice.len().saturating_sub(self.selected.len())
        }
    }

    pub fn select(&mut self, player_id: Uuid, indices: Vec<usize>) -> Result<(), String> {
        if self.phase != GamePhase::Playing {
            return Err("Cannot select dice now".into());
        }
        if self.current_player().ok_or("No current player")?.id != player_id {
            return Err("Not your turn".into());
        }
        if !self.awaiting_keep || self.dice.is_empty() || self.bust_showing {
            return Err("Nothing to select".into());
        }
        let mut unique = indices;
        unique.sort_unstable();
        unique.dedup();
        if unique.iter().any(|&i| i >= self.dice.len()) {
            return Err("Invalid die".into());
        }
        let mode = self.mode;
        unique.retain(|&i| can_keep_die(mode, &self.dice, i));
        self.selected = unique;
        Ok(())
    }

    pub fn roll(&mut self, player_id: Uuid, indices: Vec<usize>) -> Result<(), String> {
        if self.phase != GamePhase::Playing {
            return Err("Not your moment to roll".into());
        }
        if self.current_player().ok_or("No current player")?.id != player_id {
            return Err("Not your turn".into());
        }
        if self.awaiting_keep {
            self.keep(player_id, indices)?;
            if self.phase != GamePhase::Playing || self.winner_id.is_some() {
                return Ok(());
            }
        }

        let count = self.dice_to_roll();
        if count == 0 {
            return Err("No dice left to roll".into());
        }

        self.dice = roll_n(count);
        self.selected.clear();
        self.awaiting_keep = true;
        self.steal_leftover = 0;
        self.bust_showing = false;
        self.resolve_rolled_dice(player_id, None)
    }

    /// Handle bust / auto-win / continue after `self.dice` was just rolled.
    /// `steal_from` credits a busted steal back to the banker.
    fn resolve_rolled_dice(
        &mut self,
        player_id: Uuid,
        steal_from: Option<(Uuid, u32)>,
    ) -> Result<(), String> {
        let name = self
            .current_player()
            .map(|p| p.name.clone())
            .unwrap_or_default();

        let bust = |room: &mut Room, overshoot: bool| {
            if let Some((banker_id, points)) = steal_from {
                if let Some(idx) = room.player_index(banker_id) {
                    room.players[idx].score += points;
                }
                room.status_message = if overshoot {
                    format!(
                        "{name} busted the steal — nothing scores without going over {}.",
                        format_points(WIN_SCORE)
                    )
                } else {
                    format!("{name} busted the steal.")
                };
            } else {
                room.status_message = if overshoot {
                    format!(
                        "{name} busted — nothing scores without going over {}.",
                        format_points(WIN_SCORE)
                    )
                } else {
                    format!("{name} busted.")
                };
            }
            room.turn_points = 0;
            room.begin_next_turn(true);
        };

        if !has_any_score(self.mode, &self.dice) {
            bust(self, false);
            return Ok(());
        }

        if !self.has_playable_keep_now() {
            bust(self, true);
            return Ok(());
        }

        if let Some(outcome) = score_dice(self.mode, &self.dice) {
            if outcome.auto_win {
                self.winner_id = Some(player_id);
                self.phase = GamePhase::Finished;
                self.clear_action_deadline();
                self.status_message = if steal_from.is_some() {
                    format!("{name} stole and rolled five of a kind — automatic win!")
                } else {
                    format!("{name} rolled five of a kind and wins automatically!")
                };
                return Ok(());
            }
        }

        self.status_message = if steal_from.is_some() {
            format!(
                "{name} steals with {} pending! Select scoring dice.",
                format_points(self.turn_points)
            )
        } else {
            "Select scoring dice, then roll again or bank".into()
        };
        self.set_action_deadline();
        Ok(())
    }

    fn has_playable_keep_now(&self) -> bool {
        let Some(cur) = self.current_player() else {
            return false;
        };
        has_playable_keep(
            self.mode,
            &self.dice,
            cur.score,
            self.turn_points,
            cur.on_board,
            WIN_SCORE,
        )
    }

    fn check_keep_legal(&self, points: u32, auto_win: bool) -> Result<(), String> {
        if auto_win || points == 0 {
            return Ok(());
        }
        let cur = self.current_player().ok_or("No current player")?;
        if !cur.on_board {
            return Ok(());
        }
        let pending = self.turn_points.saturating_add(points);
        let new_score = cur.score.saturating_add(pending);
        if new_score > WIN_SCORE {
            return Err(format!(
                "Must hit exactly {}. That keep would make {}.",
                format_points(WIN_SCORE),
                format_points(new_score)
            ));
        }
        Ok(())
    }

    pub fn keep(&mut self, player_id: Uuid, indices: Vec<usize>) -> Result<(), String> {
        if self.phase != GamePhase::Playing {
            return Err("Cannot keep dice now".into());
        }
        let cur = self.current_player().ok_or("No current player")?;
        if cur.id != player_id {
            return Err("Not your turn".into());
        }
        if !self.awaiting_keep || self.dice.is_empty() {
            return Err("Roll first".into());
        }

        let outcome = score_held(self.mode, &self.dice, &indices)
            .ok_or_else(|| "Select a scoring combination first".to_string())?;

        self.check_keep_legal(outcome.points, outcome.auto_win)?;

        if outcome.auto_win {
            self.winner_id = Some(player_id);
            self.phase = GamePhase::Finished;
            self.clear_action_deadline();
            let name = self
                .current_player()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            self.status_message = format!("{name} rolled five of a kind and wins automatically!");
            return Ok(());
        }

        self.turn_points += outcome.points;
        self.selected = outcome.used;
        self.awaiting_keep = false;
        self.steal_leftover = self.dice.len().saturating_sub(self.selected.len());

        if self.selected.len() == self.dice.len() {
            self.steal_leftover = 0;
            self.dice.clear();
            self.selected.clear();
            let n = self.dice_count();
            self.status_message = format!(
                "Hot dice! Turn total {} — roll all {n} again or bank",
                format_points(self.turn_points)
            );
        } else {
            self.status_message = format!(
                "Kept for {} — turn total {}. Roll remaining or bank.",
                format_points(outcome.points),
                format_points(self.turn_points)
            );
        }
        self.set_action_deadline();
        Ok(())
    }

    fn check_bank_legal(&self, points: u32) -> Result<(), String> {
        if points == 0 {
            return Err("Nothing to bank".into());
        }
        let cur = self.current_player().ok_or("No current player")?;
        if !cur.on_board {
            if points < self.board_threshold {
                return Err(format!(
                    "Need at least {} in one turn to get on the board",
                    format_points(self.board_threshold)
                ));
            }
            return Ok(());
        }
        let new_score = cur.score.saturating_add(points);
        if new_score > WIN_SCORE {
            return Err(format!(
                "Must hit exactly {}. Banking would make {}.",
                format_points(WIN_SCORE),
                format_points(new_score)
            ));
        }
        Ok(())
    }

    pub fn bank(&mut self, player_id: Uuid, indices: Vec<usize>) -> Result<(), String> {
        if self.phase != GamePhase::Playing {
            return Err("Cannot bank now".into());
        }
        if self.current_player().ok_or("No current player")?.id != player_id {
            return Err("Not your turn".into());
        }
        if self.awaiting_keep {
            let outcome = score_held(self.mode, &self.dice, &indices)
                .ok_or_else(|| "Select a scoring combination first".to_string())?;
            if outcome.auto_win {
                return self.keep(player_id, indices);
            }
            self.check_bank_legal(self.turn_points.saturating_add(outcome.points))?;
            self.keep(player_id, indices)?;
            if self.phase != GamePhase::Playing || self.winner_id.is_some() {
                return Ok(());
            }
        } else {
            self.check_bank_legal(self.turn_points)?;
        }

        let cur = self.current_player().ok_or("No current player")?;
        let on_board = cur.on_board;
        let score = cur.score;
        let name = cur.name.clone();
        let points = self.turn_points;
        let leftover = self.steal_leftover;

        if !on_board {
            let player = self.current_player_mut().unwrap();
            player.score = points;
            player.on_board = true;
            self.status_message =
                format!("{} is on the board with {}!", name, format_points(points));
            self.begin_next_turn(false);
            return Ok(());
        }

        let new_score = score + points;
        if new_score == WIN_SCORE {
            let player = self.current_player_mut().unwrap();
            player.score = WIN_SCORE;
            self.winner_id = Some(player_id);
            self.phase = GamePhase::Finished;
            self.clear_action_deadline();
            self.status_message = format!(
                "{} hits exactly {} and wins!",
                name,
                format_points(WIN_SCORE)
            );
            return Ok(());
        }

        if self.next_can_steal_points(points, leftover) {
            let next_idx = self.next_player_index();
            let next_name = self.players[next_idx].name.clone();
            self.pending_bank = Some(PendingBank {
                player_id,
                points,
                leftover,
            });
            self.phase = GamePhase::StealWindow;
            self.status_message = format!(
                "{} banks {} with {leftover} dice left. {next_name} may steal!",
                name,
                format_points(points)
            );
            self.set_action_deadline();
            Ok(())
        } else {
            let player = self.current_player_mut().unwrap();
            player.score += points;
            self.status_message = format!(
                "{} banks {}. Score: {}",
                name,
                format_points(points),
                format_points(player.score)
            );
            self.begin_next_turn(false);
            Ok(())
        }
    }

    pub fn decline_steal(&mut self, player_id: Uuid) -> Result<(), String> {
        if self.phase != GamePhase::StealWindow {
            return Err("No steal to decline".into());
        }
        let next = self.next_player_index();
        let next_player = self.players.get(next).ok_or("No next player")?;
        if next_player.id != player_id {
            return Err("Only the next player can decline".into());
        }

        self.apply_pending_bank();
        self.begin_next_turn(false);
        Ok(())
    }

    pub fn steal(&mut self, player_id: Uuid) -> Result<(), String> {
        if self.phase != GamePhase::StealWindow {
            return Err("No steal available".into());
        }
        let pending = self.pending_bank.clone().ok_or("No pending bank")?;
        let next = self.next_player_index();
        let next_player = self.players.get(next).ok_or("No next player")?;
        if next_player.id != player_id {
            return Err("Only the next player can steal".into());
        }
        if !next_player.on_board {
            return Err("You must be on the board to steal".into());
        }
        if pending.leftover == 0 {
            return Err("No leftover dice".into());
        }
        if next_player.score.saturating_add(pending.points) > WIN_SCORE {
            return Err(format!(
                "Stealing would go over {}",
                format_points(WIN_SCORE)
            ));
        }

        self.turn_index = next;
        self.turn_points = pending.points;
        self.pending_bank = None;
        self.phase = GamePhase::Playing;
        self.steal_leftover = 0;
        self.selected.clear();
        self.dice = roll_n(pending.leftover);
        self.awaiting_keep = true;
        self.bust_showing = false;
        self.resolve_rolled_dice(player_id, Some((pending.player_id, pending.points)))
    }

    fn apply_pending_bank(&mut self) {
        if let Some(pending) = self.pending_bank.take() {
            if let Some(idx) = self.player_index(pending.player_id) {
                self.players[idx].score += pending.points;
            }
        }
    }

    fn leader_id(&self) -> Option<Uuid> {
        let mut best: Option<u32> = None;
        let mut leaders = Vec::new();
        for p in self.players.iter().filter(|p| p.on_board && !p.forfeited) {
            match best {
                None => {
                    best = Some(p.score);
                    leaders = vec![p.id];
                }
                Some(score) if p.score > score => {
                    best = Some(p.score);
                    leaders = vec![p.id];
                }
                Some(score) if p.score == score => leaders.push(p.id),
                _ => {}
            }
        }
        if leaders.len() == 1 {
            Some(leaders[0])
        } else {
            None
        }
    }

    pub fn end_game(&mut self, player_id: Uuid) -> Result<(), String> {
        if player_id != self.host_id {
            return Err("Only the host can end the game".into());
        }
        if !matches!(self.phase, GamePhase::Playing | GamePhase::StealWindow) {
            return Err("Game is not in progress".into());
        }
        self.apply_pending_bank();
        self.phase = GamePhase::Finished;
        self.clear_action_deadline();
        self.winner_id = self.leader_id();
        self.status_message = match self.winner_id.and_then(|id| {
            self.players
                .iter()
                .find(|p| p.id == id)
                .map(|p| (p.name.clone(), p.score))
        }) {
            Some((name, score)) => format!(
                "Host ended the game. {name} wins with {}.",
                format_points(score)
            ),
            None => "Host ended the game. No winner.".into(),
        };
        Ok(())
    }

    pub fn rematch(&mut self, player_id: Uuid) -> Result<(), String> {
        if player_id != self.host_id {
            return Err("Only the host can start a rematch".into());
        }
        for p in &mut self.players {
            p.score = 0;
            p.on_board = false;
            p.forfeited = false;
        }
        self.winner_id = None;
        self.pending_bank = None;
        self.phase = GamePhase::Lobby;
        self.turn_index = 0;
        self.reset_turn_state();
        self.clear_action_deadline();
        self.status_message = "Rematch lobby — host can start when ready".into();
        Ok(())
    }

    pub fn forfeit(&mut self, player_id: Uuid, cause: ForfeitCause) -> Result<(), String> {
        if !matches!(self.phase, GamePhase::Playing | GamePhase::StealWindow) {
            return Err("Game is not in progress".into());
        }
        let idx = self
            .player_index(player_id)
            .ok_or_else(|| "Not in this game".to_string())?;
        if self.players[idx].forfeited {
            return Err("Already forfeited".into());
        }

        let was_actor = self.acting_player_id() == Some(player_id);
        let was_current = self.current_player().is_some_and(|p| p.id == player_id);
        let in_steal = self.phase == GamePhase::StealWindow;
        let name = self.players[idx].name.clone();
        self.players[idx].forfeited = true;

        if self.host_id == player_id {
            if let Some(next) = self.players.iter().find(|p| !p.forfeited) {
                self.host_id = next.id;
            }
        }

        let reason = match cause {
            ForfeitCause::Manual => format!("{name} forfeited."),
            ForfeitCause::Timeout => {
                let wait = self
                    .idle_timeout_secs
                    .map(format_duration_secs)
                    .unwrap_or_else(|| "the time limit".into());
                format!("{name} forfeited — no play within {wait}.")
            }
        };

        if self.active_count() <= 1 {
            self.apply_pending_bank();
            self.phase = GamePhase::Finished;
            self.clear_action_deadline();
            self.winner_id = self.players.iter().find(|p| !p.forfeited).map(|p| p.id);
            self.status_message = match self.winner_id.and_then(|id| {
                self.players
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| (p.name.clone(), p.score))
            }) {
                Some((winner, score)) => {
                    format!("{reason} {winner} wins with {}.", format_points(score))
                }
                None => format!("{reason} No winner."),
            };
            return Ok(());
        }

        if in_steal && was_actor {
            self.apply_pending_bank();
            self.begin_next_turn(false);
            self.status_message = format!("{reason} {}", self.status_message);
            return Ok(());
        }

        if was_current {
            self.pending_bank = None;
            self.begin_next_turn(false);
            self.status_message = format!("{reason} {}", self.status_message);
            return Ok(());
        }

        self.status_message = reason;
        Ok(())
    }

    pub fn check_timeout(&mut self, now: u64) -> bool {
        if !matches!(self.phase, GamePhase::Playing | GamePhase::StealWindow) {
            return false;
        }
        let Some(deadline) = self.action_deadline_ms else {
            return false;
        };
        if now < deadline {
            return false;
        }
        let Some(actor) = self.acting_player_id() else {
            return false;
        };
        self.forfeit(actor, ForfeitCause::Timeout).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn room_with_two() -> Room {
        let mut room = Room::new("TEST1".into(), player("A"));
        room.players.push(player("B"));
        room.start().unwrap();
        room
    }

    #[test]
    fn needs_two_to_start() {
        let mut room = Room::new("X".into(), player("Solo"));
        assert!(room.start().is_err());
    }

    #[test]
    fn board_threshold_enforced() {
        let mut room = room_with_two();
        let id = room.players[0].id;
        room.dice = vec![5, 2, 3, 4, 6];
        room.awaiting_keep = true;
        room.keep(id, vec![0]).unwrap();
        assert_eq!(room.turn_points, 50);
        let err = room.bank(id, vec![]).unwrap_err();
        assert!(err.contains("1,000") || err.contains("1000"));
    }

    #[test]
    fn host_can_switch_to_farkle() {
        let mut room = Room::new("FARK".into(), player("A"));
        room.players.push(player("B"));
        room.update_settings(GameMode::Farkle, Some(60), 500)
            .unwrap();
        assert_eq!(room.mode, GameMode::Farkle);
        assert_eq!(room.board_threshold, 500);
        assert_eq!(room.dice_count(), 6);
        room.start().unwrap();
        let id = room.players[0].id;
        room.dice = vec![5, 2, 3, 4, 6, 6];
        room.awaiting_keep = true;
        room.keep(id, vec![0]).unwrap();
        assert_eq!(room.turn_points, 50);
        let err = room.bank(id, vec![]).unwrap_err();
        assert!(err.contains("500"));
        room.dice = vec![1, 1, 1, 2, 3, 4];
        room.awaiting_keep = true;
        room.turn_points = 0;
        room.bank(id, vec![0, 1, 2]).unwrap();
        assert_eq!(room.players[0].score, 1000);
        assert!(room.players[0].on_board);
    }

    #[test]
    fn settings_locked_after_start() {
        let mut room = room_with_two();
        assert!(
            room.update_settings(GameMode::Farkle, Some(60), 500)
                .is_err()
        );
    }

    #[test]
    fn idle_forfeit_defaults_off() {
        let mut room = Room::new("NEW".into(), player("A"));
        assert!(room.idle_timeout_secs.is_none());
        room.players.push(player("B"));
        room.start().unwrap();
        assert!(room.action_deadline_ms.is_none());
        assert!(!room.check_timeout(now_ms().saturating_add(120_000)));
    }

    #[test]
    fn custom_board_threshold_enforced() {
        let mut room = Room::new("CUST".into(), player("A"));
        room.players.push(player("B"));
        room.update_settings(GameMode::Bones, None, 2_000).unwrap();
        assert_eq!(room.board_threshold, 2_000);
        room.start().unwrap();
        let id = room.players[0].id;
        room.dice = vec![1, 1, 1, 2, 3];
        room.awaiting_keep = true;
        let err = room.bank(id, vec![0, 1, 2]).unwrap_err();
        assert!(err.contains("2,000") || err.contains("2000"));
        room.dice = vec![1, 1, 1, 1, 1];
        room.awaiting_keep = true;
        room.turn_points = 0;
        room.bank(id, vec![0, 1, 2, 3, 4]).unwrap();
        assert_eq!(room.players[0].score, 2_000);
        assert!(room.players[0].on_board);
    }

    #[test]
    fn invalid_board_threshold_rejected() {
        let mut room = Room::new("BAD".into(), player("A"));
        assert!(room.update_settings(GameMode::Bones, None, 10_001).is_err());
        assert_eq!(room.board_threshold, 1_000);
        room.update_settings(GameMode::Bones, None, 750).unwrap();
        assert_eq!(room.board_threshold, 750);
    }

    #[test]
    fn switching_mode_resets_board_threshold_to_game_default() {
        let mut room = Room::new("SNAP".into(), player("A"));
        room.update_settings(GameMode::Bones, None, 2_000).unwrap();
        assert_eq!(room.board_threshold, 2_000);
        room.update_settings(GameMode::Farkle, None, 2_000).unwrap();
        assert_eq!(room.mode, GameMode::Farkle);
        assert_eq!(room.board_threshold, 500);
        room.update_settings(GameMode::Farkle, None, 750).unwrap();
        assert_eq!(room.board_threshold, 750);
        room.update_settings(GameMode::Bones, None, 750).unwrap();
        assert_eq!(room.mode, GameMode::Bones);
        assert_eq!(room.board_threshold, 1_000);
    }

    #[test]
    fn idle_forfeit_can_be_disabled() {
        let mut room = Room::new("OFF".into(), player("A"));
        room.players.push(player("B"));
        room.update_settings(GameMode::Bones, None, 1_000).unwrap();
        assert!(room.idle_timeout_secs.is_none());
        room.start().unwrap();
        assert!(room.action_deadline_ms.is_none());
        assert!(!room.check_timeout(now_ms().saturating_add(120_000)));
        assert!(!room.players[0].forfeited);
    }

    #[test]
    fn idle_forfeit_allows_longer_lobby_options() {
        let mut room = Room::new("LONG".into(), player("A"));
        for secs in [600, 900, 1_200, 1_800] {
            room.update_settings(GameMode::Bones, Some(secs), 1_000)
                .unwrap();
            assert_eq!(room.idle_timeout_secs, Some(secs));
        }
        assert!(
            room.update_settings(GameMode::Bones, Some(7 * 60), 1_000)
                .is_err()
        );
        assert_eq!(format_duration_secs(1_800), "30 minutes");
    }

    #[test]
    fn farkle_three_pairs_does_not_bust() {
        let mut room = Room::new("PAIR".into(), player("A"));
        room.players.push(player("B"));
        room.update_settings(GameMode::Farkle, Some(60), 500)
            .unwrap();
        room.start().unwrap();
        let id = room.players[0].id;
        room.dice = vec![2, 2, 3, 3, 4, 4];
        room.awaiting_keep = true;
        room.bank(id, vec![0, 1, 2, 3, 4, 5]).unwrap();
        assert_eq!(room.players[0].score, 1500);
        assert!(room.players[0].on_board);
    }

    #[test]
    fn reclaim_keeps_host() {
        let seat = Uuid::new_v4();
        let old_id = Uuid::new_v4();
        let mut room = Room::new(
            "R".into(),
            Player {
                id: old_id,
                seat_key: seat,
                name: "Host".into(),
                score: 500,
                on_board: true,
                connected: false,
                forfeited: false,
            },
        );
        room.players.push(player("Guest"));
        let new_id = Uuid::new_v4();
        room.reclaim_seat(seat, new_id, "Host".into()).unwrap();
        assert_eq!(room.host_id, new_id);
        assert_eq!(room.players[0].id, new_id);
        assert!(room.players[0].connected);
        assert_eq!(room.players[0].score, 500);
        assert_eq!(room.view_for(new_id).invite_path, "/g/R");
    }

    #[test]
    fn bank_keeps_then_banks() {
        let mut room = room_with_two();
        let id = room.players[0].id;
        room.dice = vec![1, 1, 1, 2, 3];
        room.awaiting_keep = true;
        room.bank(id, vec![0, 1, 2]).unwrap();
        assert_eq!(room.players[0].score, 1000);
        assert!(room.players[0].on_board);
        assert_eq!(room.current_player().unwrap().id, room.players[1].id);
    }

    #[test]
    fn bust_keeps_dice_on_the_table() {
        let mut room = room_with_two();
        let b = room.players[1].id;
        room.dice = vec![2, 3, 4, 6, 6];
        room.awaiting_keep = true;
        room.status_message = format!("{} busted.", room.players[0].name);
        room.turn_points = 0;
        room.begin_next_turn(true);
        assert_eq!(room.dice, vec![2, 3, 4, 6, 6]);
        assert!(room.bust_showing);
        assert_eq!(room.current_player().unwrap().id, b);
        assert!(!room.awaiting_keep);
        assert_eq!(room.dice_to_roll(), 5);
        let view = room.view_for(b);
        assert!(view.bust);
        assert_eq!(view.dice, vec![2, 3, 4, 6, 6]);
        assert_eq!(view.message, format!("{} busted.", room.players[0].name));
    }

    #[test]
    fn invite_path_includes_code() {
        let room = Room::new("ABC12".into(), player("Host"));
        assert_eq!(room.view_for(room.host_id).invite_path, "/g/ABC12");
        assert_eq!(room.view_for(room.host_id).code, "ABC12");
    }

    #[test]
    fn host_can_end_game_early() {
        let mut room = room_with_two();
        let host = room.host_id;
        let guest = room.players[1].id;
        room.players[0].score = 2400;
        room.players[0].on_board = true;
        room.players[1].score = 1100;
        room.players[1].on_board = true;

        assert!(room.end_game(guest).is_err());
        room.end_game(host).unwrap();
        assert_eq!(room.phase, GamePhase::Finished);
        assert_eq!(room.winner_id, Some(host));
        assert!(room.status_message.contains("wins with 2,400"));
    }

    #[test]
    fn end_game_tie_has_no_winner() {
        let mut room = room_with_two();
        room.players[0].score = 1500;
        room.players[0].on_board = true;
        room.players[1].score = 1500;
        room.players[1].on_board = true;
        room.end_game(room.host_id).unwrap();
        assert_eq!(room.phase, GamePhase::Finished);
        assert_eq!(room.winner_id, None);
        assert!(room.status_message.contains("No winner"));
    }

    #[test]
    fn end_game_applies_pending_bank() {
        let mut room = room_with_two();
        let host = room.host_id;
        room.players[0].score = 1000;
        room.players[0].on_board = true;
        room.players[1].score = 2000;
        room.players[1].on_board = true;
        room.pending_bank = Some(PendingBank {
            player_id: host,
            points: 400,
            leftover: 2,
        });
        room.phase = GamePhase::StealWindow;
        room.end_game(host).unwrap();
        assert_eq!(room.players[0].score, 1400);
        assert_eq!(room.winner_id, Some(room.players[1].id));
        assert!(room.pending_bank.is_none());
    }

    #[test]
    fn cannot_end_game_in_lobby() {
        let mut room = Room::new("LOBBY".into(), player("Host"));
        room.players.push(player("Guest"));
        assert!(room.end_game(room.host_id).is_err());
    }

    fn room_with_three() -> Room {
        let mut room = Room::new("TEST3".into(), player("A"));
        room.players.push(player("B"));
        room.players.push(player("C"));
        room.start().unwrap();
        room
    }

    #[test]
    fn player_can_forfeit_and_opponent_wins() {
        let mut room = room_with_two();
        let host = room.host_id;
        let guest = room.players[1].id;
        room.players[1].score = 800;
        room.forfeit(host, ForfeitCause::Manual).unwrap();
        assert!(room.players[0].forfeited);
        assert_eq!(room.phase, GamePhase::Finished);
        assert_eq!(room.winner_id, Some(guest));
        assert_eq!(room.host_id, guest);
        assert!(room.status_message.contains("forfeited"));
        assert!(room.action_deadline_ms.is_none());
    }

    #[test]
    fn cannot_forfeit_in_lobby() {
        let mut room = Room::new("LOBBY".into(), player("Host"));
        room.players.push(player("Guest"));
        assert!(room.forfeit(room.host_id, ForfeitCause::Manual).is_err());
    }

    #[test]
    fn forfeit_skips_remaining_turns() {
        let mut room = room_with_three();
        let a = room.players[0].id;
        let b = room.players[1].id;
        let c = room.players[2].id;
        room.forfeit(b, ForfeitCause::Manual).unwrap();
        assert_eq!(room.phase, GamePhase::Playing);
        assert_eq!(room.current_player().unwrap().id, a);
        room.players[0].on_board = true;
        room.turn_points = 100;
        room.awaiting_keep = false;
        room.bank(a, vec![]).unwrap();
        assert_eq!(room.phase, GamePhase::Playing);
        assert_eq!(room.current_player().unwrap().id, c);
    }

    #[test]
    fn host_forfeit_transfers_host() {
        let mut room = room_with_three();
        let a = room.host_id;
        let b = room.players[1].id;
        room.forfeit(a, ForfeitCause::Manual).unwrap();
        assert_eq!(room.host_id, b);
        assert_eq!(room.phase, GamePhase::Playing);
        assert_eq!(room.current_player().unwrap().id, b);
        assert!(room.players[0].forfeited);
    }

    #[test]
    fn timeout_forfeits_acting_player() {
        let mut room = room_with_two();
        room.idle_timeout_secs = Some(60);
        let guest = room.players[1].id;
        room.action_deadline_ms = Some(now_ms().saturating_sub(1));
        assert!(room.check_timeout(now_ms()));
        assert!(room.players[0].forfeited);
        assert_eq!(room.winner_id, Some(guest));
        assert!(room.status_message.contains("1 minute"));
    }

    #[test]
    fn playing_resets_the_action_deadline() {
        let mut room = room_with_two();
        room.idle_timeout_secs = Some(60);
        let id = room.players[0].id;
        room.dice = vec![1, 2, 3, 4, 6];
        room.awaiting_keep = true;
        room.keep(id, vec![0]).unwrap();
        let deadline = room.action_deadline_ms.expect("deadline");
        let now = now_ms();
        let timeout_ms = room.idle_timeout_secs.unwrap() * 1000;
        assert!(deadline >= now + timeout_ms - 2_000);
        assert!(deadline <= now + timeout_ms + 2_000);
    }

    #[test]
    fn steal_timeout_applies_pending_bank() {
        let mut room = room_with_three();
        let a = room.players[0].id;
        let b = room.players[1].id;
        room.players[0].score = 1000;
        room.players[0].on_board = true;
        room.players[1].on_board = true;
        room.pending_bank = Some(PendingBank {
            player_id: a,
            points: 350,
            leftover: 2,
        });
        room.phase = GamePhase::StealWindow;
        room.action_deadline_ms = Some(now_ms().saturating_sub(1));
        assert_eq!(room.acting_player_id(), Some(b));
        assert!(room.check_timeout(now_ms()));
        assert!(room.players[1].forfeited);
        assert_eq!(room.players[0].score, 1350);
        assert_eq!(room.phase, GamePhase::Playing);
        assert_eq!(room.current_player().unwrap().id, room.players[2].id);
    }

    #[test]
    fn select_is_visible_to_other_players() {
        let mut room = room_with_two();
        let a = room.players[0].id;
        let b = room.players[1].id;
        room.dice = vec![1, 5, 2, 3, 4];
        room.awaiting_keep = true;
        room.select(a, vec![0, 1]).unwrap();
        assert_eq!(room.selected, vec![0, 1]);
        assert_eq!(room.view_for(b).selected, vec![0, 1]);
        assert!(room.select(b, vec![2]).is_err());
        room.select(a, vec![]).unwrap();
        assert!(room.view_for(b).selected.is_empty());
        room.select(a, vec![0, 2]).unwrap();
        assert_eq!(room.selected, vec![0]);
    }

    #[test]
    fn bank_overshoot_lets_you_unmark_and_win() {
        let mut room = room_with_two();
        let id = room.players[0].id;
        room.players[0].score = 9_900;
        room.players[0].on_board = true;
        room.dice = vec![1, 1, 5];
        room.awaiting_keep = true;
        let err = room.bank(id, vec![0, 1, 2]).unwrap_err();
        assert!(err.contains("exactly") || err.contains("10,000") || err.contains("10000"));
        assert!(room.awaiting_keep);
        assert_eq!(room.turn_points, 0);
        assert_eq!(room.players[0].score, 9_900);
        room.select(id, vec![0]).unwrap();
        assert_eq!(room.selected, vec![0]);
        room.bank(id, vec![0]).unwrap();
        assert_eq!(room.phase, GamePhase::Finished);
        assert_eq!(room.winner_id, Some(id));
        assert_eq!(room.players[0].score, 10_000);
    }

    #[test]
    fn keep_rejects_selection_that_would_go_over() {
        let mut room = room_with_two();
        let id = room.players[0].id;
        room.players[0].score = 9_800;
        room.players[0].on_board = true;
        room.mode = GameMode::Farkle;
        room.dice = vec![2, 4, 5, 3, 3, 3];
        room.awaiting_keep = true;
        let err = room.keep(id, vec![3, 4, 5]).unwrap_err();
        assert!(err.contains("exactly") || err.contains("10,000"));
        assert_eq!(room.turn_points, 0);
        assert!(room.awaiting_keep);
        // Lone 5 still fits under the cap.
        room.keep(id, vec![2]).unwrap();
        assert_eq!(room.turn_points, 50);
    }

    #[test]
    fn roll_busts_when_only_scoring_keeps_go_over() {
        let mut room = room_with_two();
        let id = room.players[0].id;
        let next = room.players[1].id;
        room.players[0].score = 9_800;
        room.players[0].on_board = true;
        room.mode = GameMode::Farkle;
        room.dice = vec![2, 4, 3, 3, 3, 6];
        room.selected.clear();
        room.awaiting_keep = true;
        room.bust_showing = false;
        room.turn_points = 0;
        room.resolve_rolled_dice(id, None).unwrap();
        assert_eq!(room.current_player().unwrap().id, next);
        assert!(room.bust_showing);
        assert!(room.status_message.contains("going over"));
        assert_eq!(room.dice, vec![2, 4, 3, 3, 3, 6]);
    }

    #[test]
    fn bank_skips_steal_when_next_would_go_over() {
        let mut room = room_with_two();
        let a = room.players[0].id;
        let b = room.players[1].id;
        room.players[0].score = 1000;
        room.players[0].on_board = true;
        room.players[1].score = 9400;
        room.players[1].on_board = true;
        room.dice = vec![1, 1, 1, 2, 3];
        room.awaiting_keep = true;
        room.bank(a, vec![0, 1, 2]).unwrap();
        assert_eq!(room.phase, GamePhase::Playing);
        assert!(room.pending_bank.is_none());
        assert_eq!(room.players[0].score, 2000);
        assert_eq!(room.current_player().unwrap().id, b);
        assert!(!room.view_for(b).steal_available);
    }

    #[test]
    fn bank_offers_steal_when_next_can_take_it() {
        let mut room = room_with_two();
        let a = room.players[0].id;
        let b = room.players[1].id;
        room.players[0].score = 1000;
        room.players[0].on_board = true;
        room.players[1].score = 1000;
        room.players[1].on_board = true;
        room.dice = vec![1, 1, 1, 2, 3];
        room.awaiting_keep = true;
        room.bank(a, vec![0, 1, 2]).unwrap();
        assert_eq!(room.phase, GamePhase::StealWindow);
        let pending = room.pending_bank.as_ref().unwrap();
        assert_eq!(pending.points, 1000);
        assert_eq!(pending.leftover, 2);
        assert!(room.view_for(b).steal_available);
        assert!(!room.view_for(a).steal_available);
    }

    #[test]
    fn bank_offers_steal_when_it_hits_exactly_10000() {
        let mut room = room_with_two();
        let a = room.players[0].id;
        let b = room.players[1].id;
        room.players[0].score = 1000;
        room.players[0].on_board = true;
        room.players[1].score = 9000;
        room.players[1].on_board = true;
        room.dice = vec![1, 1, 1, 2, 3];
        room.awaiting_keep = true;
        room.bank(a, vec![0, 1, 2]).unwrap();
        assert_eq!(room.phase, GamePhase::StealWindow);
        assert!(room.view_for(b).steal_available);
    }

    #[test]
    fn steal_rejects_when_it_would_go_over() {
        let mut room = room_with_two();
        let a = room.players[0].id;
        let b = room.players[1].id;
        room.players[0].score = 1000;
        room.players[0].on_board = true;
        room.players[1].score = 9500;
        room.players[1].on_board = true;
        room.pending_bank = Some(PendingBank {
            player_id: a,
            points: 1000,
            leftover: 2,
        });
        room.phase = GamePhase::StealWindow;
        let err = room.steal(b).unwrap_err();
        assert!(err.contains("over") || err.contains("10,000"));
        assert_eq!(room.phase, GamePhase::StealWindow);
        assert_eq!(room.players[0].score, 1000);
    }

    #[test]
    fn rematch_clears_forfeit() {
        let mut room = room_with_three();
        let a = room.host_id;
        room.forfeit(room.players[1].id, ForfeitCause::Manual)
            .unwrap();
        room.phase = GamePhase::Finished;
        room.rematch(a).unwrap();
        assert!(room.players.iter().all(|p| !p.forfeited));
        assert!(room.action_deadline_ms.is_none());
    }

    #[test]
    fn vacate_lobby_removes_player_and_transfers_host() {
        let mut room = Room::new("LOBBY".into(), player("Host"));
        room.players.push(player("Guest"));
        let host_id = room.players[0].id;
        let host_seat = room.players[0].seat_key;
        let guest_id = room.players[1].id;
        let (old_id, empty) = room.vacate_seat(host_seat).unwrap();
        assert_eq!(old_id, host_id);
        assert!(!empty);
        assert_eq!(room.players.len(), 1);
        assert_eq!(room.host_id, guest_id);
        assert!(room.status_message.contains("left"));
    }

    #[test]
    fn vacate_last_lobby_player_empties_room() {
        let mut room = Room::new("SOLO".into(), player("Host"));
        let seat = room.players[0].seat_key;
        let (_, empty) = room.vacate_seat(seat).unwrap();
        assert!(empty);
        assert!(room.players.is_empty());
    }

    #[test]
    fn vacate_started_game_detaches_without_dropping_row() {
        let mut room = room_with_two();
        let seat = room.players[0].seat_key;
        let old_id = room.players[0].id;
        let (released, empty) = room.vacate_seat(seat).unwrap();
        assert_eq!(released, old_id);
        assert!(!empty);
        assert_eq!(room.players.len(), 2);
        assert_ne!(room.players[0].id, old_id);
        assert!(!room.players[0].connected);
        assert!(room.players[0].forfeited);
        assert_eq!(room.phase, GamePhase::Finished);
    }

    #[test]
    fn vacate_already_forfeited_does_not_remove_row() {
        let mut room = room_with_three();
        let seat = room.players[1].seat_key;
        let old_id = room.players[1].id;
        room.forfeit(old_id, ForfeitCause::Manual).unwrap();
        let (released, empty) = room.vacate_seat(seat).unwrap();
        assert_eq!(released, old_id);
        assert!(!empty);
        assert_eq!(room.players.len(), 3);
        assert!(room.players[1].forfeited);
        assert!(!room.players[1].connected);
        assert_ne!(room.players[1].id, old_id);
        assert_eq!(room.phase, GamePhase::Playing);
    }
}
