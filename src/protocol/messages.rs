use serde::{Deserialize, Serialize};

use super::types::{Player, Session};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientMessage {
    Authenticate {
        player_id: String,
        token: String,
    },

    CreateSession,

    JoinSession {
        session_id: String,
    },

    LeaveSession {
        session_id: String,
    },

    SetReady {
        ready: bool,
    },

    StartSession,

    Heartbeat,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerMessage {
    Authenticated {
        player: Player,
    },

    SessionCreated {
        session: Session,
    },

    SessionJoined {
        session: Session,
    },

    SessionLeft {
        session_id: String,
    },

    ReadyChanged {
        player_id: String,
        ready: bool,
    },

    SessionStarted {
        session_id: String,
    },

    Error {
        code: u32,
        message: String,
    },

    Pong,
}
