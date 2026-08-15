use crate::protocol::{
    BOARD_THRESHOLD, DICE_COUNT, GamePhase, GameView, PendingBankView, PlayerView, WIN_SCORE,
    invite_path,
};
use crate::scoring::{has_any_score, score_dice, score_held};
use bevy::prelude::*;
use rand::RngExt;
use std::collections::HashMap;
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
}

#[derive(Clone, Debug)]
pub struct Player {
    pub id: Uuid,
    pub seat_key: Uuid,
    pub name: String,
    pub score: u32,
    pub on_board: bool,
    pub connected: bool,
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
        if self.players.is_empty() {
            0
        } else {
            (self.turn_index + 1) % self.players.len()
        }
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
        let steal_available = matches!(self.phase, GamePhase::StealWindow)
            && self.pending_bank.as_ref().is_some_and(|p| p.leftover > 0)
            && self
                .players
                .get(self.next_player_index())
                .is_some_and(|p| p.id == you && p.on_board);

        let you_can_act = match self.phase {
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
            self.status_message = format!(
                "{name} banks {points} with {leftover} dice left. {next_name} may steal!"
            );
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

        if let Some(pending) = self.pending_bank.take() {
            if let Some(idx) = self.player_index(pending.player_id) {
                self.players[idx].score += pending.points;
            }
        }
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
                self.status_message =
                    format!("{name} stole and rolled five of a kind — automatic win!");
                return Ok(());
            }
        }

        self.status_message = format!(
            "{name} steals with {} pending! Select scoring dice.",
            self.turn_points
        );
        Ok(())
    }

    pub fn rematch(&mut self, player_id: Uuid) -> Result<(), String> {
        if player_id != self.host_id {
            return Err("Only the host can start a rematch".into());
        }
        for p in &mut self.players {
            p.score = 0;
            p.on_board = false;
        }
        self.winner_id = None;
        self.pending_bank = None;
        self.phase = GamePhase::Lobby;
        self.turn_index = 0;
        self.reset_turn_state();
        self.status_message = "Rematch lobby — host can start when ready".into();
        Ok(())
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
}
