use crate::protocol::{
    ACTION_TIMEOUT_MS, BOARD_THRESHOLD, DICE_COUNT, GamePhase, GameView, PendingBankView,
    PlayerView, WIN_SCORE, invite_path,
};
use crate::scoring::{has_any_score, score_dice, score_held};
use bevy::prelude::*;
use rand::RngExt;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Resource, Default)]
pub struct GameRooms {
    pub by_code: HashMap<String, Entity>,
}

#[derive(Component)]
pub struct Room {
    pub code: String,
    pub host_id: Uuid,
    pub players: Vec<Player>,
    pub phase: GamePhase,
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
}

#[derive(Clone, Debug)]
pub struct Player {
    pub id: Uuid,
    pub seat_key: Uuid,
    pub name: String,
    pub score: u32,
    pub on_board: bool,
    pub connected: bool,
    pub forfeited: bool,
}

#[derive(Clone, Debug)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForfeitCause {
    Manual,
    Timeout,
}

impl Room {
    pub fn new(code: String, host: Player) -> Self {
        Self {
            code,
            host_id: host.id,
            players: vec![host],
            phase: GamePhase::Lobby,
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
        }
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
        self.action_deadline_ms = Some(now_ms().saturating_add(ACTION_TIMEOUT_MS));
    }

    fn clear_action_deadline(&mut self) {
        self.action_deadline_ms = None;
    }

    fn turn_hint(on_board: bool) -> &'static str {
        if on_board {
            "roll when ready"
        } else {
            "need 1,000 in one turn to get on the board"
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
        self.status_message = format!("{} reconnected", self.players[idx].name);
        Ok(old_id)
    }

    pub fn view_for(&self, you: Uuid) -> GameView {
        let you_forfeited = self
            .player_index(you)
            .is_some_and(|i| self.players[i].forfeited);
        let steal_available = matches!(self.phase, GamePhase::StealWindow)
            && self.pending_bank.as_ref().is_some_and(|p| p.leftover > 0)
            && self
                .players
                .get(self.next_player_index())
                .is_some_and(|p| p.id == you && p.on_board && !p.forfeited);

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
        self.status_message = format!("{name}'s turn — {}", Self::turn_hint(false));
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
            let next = format!("{}'s turn — {}", p.name, Self::turn_hint(p.on_board));
            if keep_dice {
                self.status_message = format!("{} {next}", self.status_message);
            } else {
                self.status_message = next;
            }
        }
        self.set_action_deadline();
    }

    fn dice_to_roll(&self) -> usize {
        if self.bust_showing || self.dice.is_empty() {
            DICE_COUNT
        } else {
            self.dice.len().saturating_sub(self.selected.len())
        }
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

        if !has_any_score(&self.dice) {
            let name = self
                .current_player()
                .map(|p| p.name.clone())
                .unwrap_or_default();
            self.status_message = format!("{name} busted! No scoring dice.");
            self.turn_points = 0;
            self.begin_next_turn(true);
            return Ok(());
        }

        if let Some(outcome) = score_dice(&self.dice) {
            if outcome.auto_win {
                self.winner_id = Some(player_id);
                self.phase = GamePhase::Finished;
                self.clear_action_deadline();
                let name = self
                    .current_player()
                    .map(|p| p.name.clone())
                    .unwrap_or_default();
                self.status_message =
                    format!("{name} rolled five of a kind and wins automatically!");
                return Ok(());
            }
        }

        self.status_message = "Select scoring dice, then roll again or bank".into();
        self.set_action_deadline();
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

        let outcome = score_held(&self.dice, &indices)
            .ok_or_else(|| "Select a scoring combination first".to_string())?;

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
            self.status_message = format!(
                "Hot dice! Turn total {} — roll all five again or bank",
                self.turn_points
            );
        } else {
            self.status_message = format!(
                "Kept for {} — turn total {}. Roll remaining or bank.",
                outcome.points, self.turn_points
            );
        }
        self.set_action_deadline();
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
            self.keep(player_id, indices)?;
            if self.phase != GamePhase::Playing || self.winner_id.is_some() {
                return Ok(());
            }
        }
        if self.turn_points == 0 {
            return Err("Nothing to bank".into());
        }

        let cur = self.current_player().ok_or("No current player")?;
        let on_board = cur.on_board;
        let score = cur.score;
        let name = cur.name.clone();
        let points = self.turn_points;
        let leftover = self.steal_leftover;

        if !on_board {
            if points < BOARD_THRESHOLD {
                return Err(format!(
                    "Need at least {BOARD_THRESHOLD} in one turn to get on the board"
                ));
            }
            let player = self.current_player_mut().unwrap();
            player.score = points;
            player.on_board = true;
            self.status_message = format!("{name} is on the board with {points}!");
            self.begin_next_turn(false);
            return Ok(());
        }

        let new_score = score + points;
        if new_score > WIN_SCORE {
            return Err(format!(
                "Must hit exactly {WIN_SCORE}. Banking would make {new_score}."
            ));
        }
        if new_score == WIN_SCORE {
            let player = self.current_player_mut().unwrap();
            player.score = WIN_SCORE;
            self.winner_id = Some(player_id);
            self.phase = GamePhase::Finished;
            self.clear_action_deadline();
            self.status_message = format!("{name} hits exactly {WIN_SCORE} and wins!");
            return Ok(());
        }

        let next_idx = self.next_player_index();
        let next_on_board = self.players.get(next_idx).is_some_and(|p| p.on_board);

        if leftover > 0 && next_on_board {
            let next_name = self.players[next_idx].name.clone();
            self.pending_bank = Some(PendingBank {
                player_id,
                points,
                leftover,
            });
            self.phase = GamePhase::StealWindow;
            self.status_message =
                format!("{name} banks {points} with {leftover} dice left. {next_name} may steal!");
            self.set_action_deadline();
            Ok(())
        } else {
            let player = self.current_player_mut().unwrap();
            player.score += points;
            self.status_message = format!("{name} banks {points}. Score: {}", player.score);
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

        self.turn_index = next;
        self.turn_points = pending.points;
        self.pending_bank = None;
        self.phase = GamePhase::Playing;
        self.steal_leftover = 0;
        self.selected.clear();
        self.dice = roll_n(pending.leftover);
        self.awaiting_keep = true;

        let name = self
            .current_player()
            .map(|p| p.name.clone())
            .unwrap_or_default();

        if !has_any_score(&self.dice) {
            if let Some(idx) = self.player_index(pending.player_id) {
                self.players[idx].score += pending.points;
            }
            self.status_message = format!("{name} failed the steal — bust!");
            self.turn_points = 0;
            self.begin_next_turn(true);
            return Ok(());
        }

        if let Some(outcome) = score_dice(&self.dice) {
            if outcome.auto_win {
                self.winner_id = Some(player_id);
                self.phase = GamePhase::Finished;
                self.clear_action_deadline();
                self.status_message =
                    format!("{name} stole and rolled five of a kind — automatic win!");
                return Ok(());
            }
        }

        self.status_message = format!(
            "{name} steals with {} pending! Select scoring dice.",
            self.turn_points
        );
        self.set_action_deadline();
        Ok(())
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
            Some((name, score)) => format!("Host ended the game. {name} wins with {score}."),
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
            ForfeitCause::Timeout => format!("{name} forfeited — no play within 1 minute."),
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
                Some((winner, score)) => format!("{reason} {winner} wins with {score}."),
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
        room.status_message = format!("{} busted! No scoring dice.", room.players[0].name);
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
        assert!(room.status_message.contains("wins with 2400"));
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
        let id = room.players[0].id;
        room.action_deadline_ms = Some(now_ms() + 5_000);
        room.dice = vec![1, 2, 3, 4, 6];
        room.awaiting_keep = true;
        room.keep(id, vec![0]).unwrap();
        let deadline = room.action_deadline_ms.expect("deadline");
        let now = now_ms();
        assert!(deadline >= now + crate::protocol::ACTION_TIMEOUT_MS - 2_000);
        assert!(deadline <= now + crate::protocol::ACTION_TIMEOUT_MS + 2_000);
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
}
