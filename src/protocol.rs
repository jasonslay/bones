use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WIN_SCORE: u32 = 10_000;
pub const BOARD_THRESHOLD: u32 = 1_000;
pub const DICE_COUNT: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    CreateGame { name: String },
    JoinGame { code: String, name: String },
    StartGame,
    Roll,
    Keep { indices: Vec<usize> },
    Bank,
    Steal,
    DeclineSteal,
    Rematch,
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
    },
    Joined {
        code: String,
        player_id: Uuid,
    },
    State(GameView),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameView {
    pub code: String,
    pub phase: GamePhase,
    pub players: Vec<PlayerView>,
    pub current_player_id: Option<Uuid>,
    pub you_are: Uuid,
    pub host_id: Uuid,
    pub dice: Vec<u8>,
    pub selected: Vec<usize>,
    pub turn_points: u32,
    pub awaiting_keep: bool,
    pub pending_bank: Option<PendingBankView>,
    pub steal_available: bool,
    pub you_can_act: bool,
    pub message: String,
    pub winner_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerView {
    pub id: Uuid,
    pub name: String,
    pub score: u32,
    pub on_board: bool,
    pub connected: bool,
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
