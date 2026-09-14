use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WIN_SCORE: u32 = 10_000;
pub const BONES_BOARD_THRESHOLD: u32 = 1_000;
pub const FARKLE_BOARD_THRESHOLD: u32 = 500;
pub const BONES_DICE_COUNT: usize = 5;
pub const FARKLE_DICE_COUNT: usize = 6;
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60;
/// Allowed idle forfeit durations (seconds). `None` disables the timer.
pub const IDLE_TIMEOUT_OPTIONS_SECS: &[u64] = &[30, 60, 120, 300];
/// Keepalive interval. Stays well under typical proxy idle timeouts (~100s).
pub const WS_PING_INTERVAL_MS: u64 = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GameMode {
    #[default]
    Bones,
    Farkle,
}

impl GameMode {
    pub fn dice_count(self) -> usize {
        match self {
            GameMode::Bones => BONES_DICE_COUNT,
            GameMode::Farkle => FARKLE_DICE_COUNT,
        }
    }

    pub fn default_board_threshold(self) -> u32 {
        match self {
            GameMode::Bones => BONES_BOARD_THRESHOLD,
            GameMode::Farkle => FARKLE_BOARD_THRESHOLD,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            GameMode::Bones => "Bones",
            GameMode::Farkle => "Farkle",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    CreateGame {
        name: String,
        #[serde(default = "Uuid::new_v4")]
        seat_key: Uuid,
    },
    JoinGame {
        code: String,
        name: String,
        #[serde(default = "Uuid::new_v4")]
        seat_key: Uuid,
    },
    UpdateSettings {
        mode: GameMode,
        /// `null` disables idle forfeit. Otherwise one of the allowed durations.
        idle_timeout_secs: Option<u64>,
    },
    StartGame,
    Roll {
        #[serde(default)]
        indices: Vec<usize>,
    },
    Select {
        indices: Vec<usize>,
    },
    Keep {
        indices: Vec<usize>,
    },
    Bank {
        #[serde(default)]
        indices: Vec<usize>,
    },
    Steal,
    DeclineSteal,
    EndGame,
    Forfeit,
    Rematch,
    LeaveGame,
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Welcome {
        player_id: Uuid,
    },
    Error {
        message: String,
    },
    GameCreated {
        code: String,
        player_id: Uuid,
        invite_path: String,
    },
    Joined {
        code: String,
        player_id: Uuid,
        invite_path: String,
    },
    State(GameView),
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameView {
    pub code: String,
    pub invite_path: String,
    pub phase: GamePhase,
    pub mode: GameMode,
    pub board_threshold: u32,
    pub dice_count: usize,
    pub idle_timeout_secs: Option<u64>,
    pub players: Vec<PlayerView>,
    pub current_player_id: Option<Uuid>,
    pub you_are: Uuid,
    pub host_id: Uuid,
    pub dice: Vec<u8>,
    pub selected: Vec<usize>,
    pub turn_points: u32,
    pub awaiting_keep: bool,
    pub bust: bool,
    pub pending_bank: Option<PendingBankView>,
    pub steal_available: bool,
    pub you_can_act: bool,
    pub message: String,
    pub winner_id: Option<Uuid>,
    pub action_deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerView {
    pub id: Uuid,
    pub name: String,
    pub score: u32,
    pub on_board: bool,
    pub connected: bool,
    pub forfeited: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingBankView {
    pub player_id: Uuid,
    pub points: u32,
    pub leftover: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GamePhase {
    Lobby,
    Playing,
    StealWindow,
    Finished,
}

pub fn invite_path(code: &str) -> String {
    format!("/g/{code}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_pong_wire_format() {
        assert_eq!(
            serde_json::to_string(&ServerMessage::Ping).unwrap(),
            r#"{"type":"ping"}"#
        );
        let pong: ClientMessage = serde_json::from_str(r#"{"type":"pong"}"#).unwrap();
        assert!(matches!(pong, ClientMessage::Pong));
    }

    #[test]
    fn update_settings_wire_format() {
        let msg: ClientMessage = serde_json::from_str(
            r#"{"type":"update_settings","mode":"farkle","idle_timeout_secs":null}"#,
        )
        .unwrap();
        assert!(matches!(
            msg,
            ClientMessage::UpdateSettings {
                mode: GameMode::Farkle,
                idle_timeout_secs: None,
            }
        ));
        let msg: ClientMessage = serde_json::from_str(
            r#"{"type":"update_settings","mode":"bones","idle_timeout_secs":120}"#,
        )
        .unwrap();
        assert!(matches!(
            msg,
            ClientMessage::UpdateSettings {
                mode: GameMode::Bones,
                idle_timeout_secs: Some(120),
            }
        ));
    }
}
