use std::io;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

use super::{
    codec::{decode_frame, encode_frame, HEADER_SIZE, MAX_PAYLOAD_SIZE},
    types::{Player, Session},
    ClientMessage, ServerMessage,
};

pub const MSG_AUTHENTICATE: u16 = 0x0001;
pub const MSG_HEARTBEAT: u16 = 0x0002;
pub const MSG_CREATE_SESSION: u16 = 0x0003;
pub const MSG_JOIN_SESSION: u16 = 0x0004;
pub const MSG_LEAVE_SESSION: u16 = 0x0005;
pub const MSG_SET_READY: u16 = 0x0006;
pub const MSG_START_SESSION: u16 = 0x0007;
const MAX_TCP_CONNECTIONS: usize = 128;

#[derive(Debug, Clone)]
struct ConnectionState {
    user_id: Option<String>,
    display_name: Option<String>,
    session_id: Option<String>,
    ready: bool,
    started: bool,
}

pub async fn run_tcp_server(
    addr: String,
    db: SqlitePool,
) -> io::Result<()> {
    let listener = TcpListener::bind(&addr).await?;

    println!("Protocol TCP listening on {addr}");

    let connection_slots = Arc::new(Semaphore::new(MAX_TCP_CONNECTIONS));

    loop {
        let (stream, peer) = listener.accept().await?;

        let permit = match connection_slots.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                eprintln!("TCP connection limit reached; rejecting {peer}");
                drop(stream);
                continue;
            }
        };

        println!("TCP client connected: {peer}");

        let client_db = db.clone();

        tokio::spawn(async move {
            let _permit = permit;

            if let Err(error) = handle_client(stream, client_db).await {
                eprintln!("TCP client error ({peer}): {error}");
            }

            println!("TCP client disconnected: {peer}");
        });
    }
}

async fn handle_client(
    mut stream: TcpStream,
    db: SqlitePool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut state = ConnectionState {
        user_id: None,
        display_name: None,
        session_id: None,
        ready: false,
        started: false,
    };

    loop {
        let mut header = [0u8; HEADER_SIZE];

        println!("TCP waiting for packet header...");

        match stream.read_exact(&mut header).await {
            Ok(_) => {
                println!(
                    "TCP header received: {:?}",
                    header
                );
            }

            Err(error)
                if error.kind()
                    == io::ErrorKind::UnexpectedEof =>
            {
                println!(
                    "TCP connection closed before full header was received."
                );

                cleanup_connection(
                    &db,
                    &mut state,
                )
                .await;

                return Ok(());
            }

            Err(error) => {
                eprintln!(
                    "TCP header read error: {error}"
                );

                cleanup_connection(
                    &db,
                    &mut state,
                )
                .await;

                return Err(error.into());
            }
        }

        if &header[0..4] != b"KLAY" {
            eprintln!("TCP invalid magic: {:?}", &header[0..4]);
            cleanup_connection(&db, &mut state).await;
            return Err(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid packet magic",
                )
                .into(),
            );
        }

        if header[4] != 1 {
            cleanup_connection(&db, &mut state).await;
            return Err(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsupported protocol version",
                )
                .into(),
            );
        }

        let payload_len = u32::from_be_bytes([
            header[11],
            header[12],
            header[13],
            header[14],
        ]) as usize;

        if payload_len > MAX_PAYLOAD_SIZE {
            cleanup_connection(
                &db,
                &mut state,
            )
            .await;

            return Err(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "payload exceeds maximum size",
                )
                .into(),
            );
        }

        let mut frame =
            Vec::with_capacity(
                HEADER_SIZE + payload_len,
            );

        frame.extend_from_slice(&header);

        let mut payload =
            vec![0u8; payload_len];

        if let Err(error) =
            stream.read_exact(&mut payload).await
        {
            eprintln!(
                "TCP payload read error: {error}"
            );

            cleanup_connection(
                &db,
                &mut state,
            )
            .await;

            return Err(error.into());
        }

        frame.extend_from_slice(&payload);

        // Never log client payloads: authentication frames contain bearer tokens.

        let decoded =
            decode_frame::<ClientMessage>(&frame);

        let (message_id, request_id, message) =
            match decoded {
                Ok(value) => value,

                Err(error) => {
                    eprintln!(
                        "TCP frame decode error: {error}"
                    );

                    cleanup_connection(
                        &db,
                        &mut state,
                    )
                    .await;

                    return Err(error);
                }
            };

        let expected_message_id =
            client_message_id(&message);

        if message_id != expected_message_id {
            let response =
                ServerMessage::Error {
                    code: 1099,
                    message:
                        "message_id_mismatch"
                            .to_string(),
                };

            let response_frame =
                encode_frame(
                    message_id | 0x8000,
                    request_id,
                    &response,
                )?;

            stream
                .write_all(&response_frame)
                .await?;

            stream.flush().await?;

            continue;
        }

        let response = handle_message(
            message,
            &db,
            &mut state,
        )
        .await;

        let response_message_id =
            message_id | 0x8000;

        let response_frame =
            encode_frame(
                response_message_id,
                request_id,
                &response,
            )?;

        println!(
            "TCP sending response: message_id={}, request_id={}",
            response_message_id,
            request_id
        );

        if let Err(error) =
            stream.write_all(&response_frame).await
        {
            eprintln!(
                "TCP response write error: {error}"
            );

            cleanup_connection(
                &db,
                &mut state,
            )
            .await;

            return Err(error.into());
        }

        if let Err(error) =
            stream.flush().await
        {
            eprintln!(
                "TCP response flush error: {error}"
            );

            cleanup_connection(
                &db,
                &mut state,
            )
            .await;

            return Err(error.into());
        }

        println!(
            "TCP response sent successfully."
        );
    }
}

fn client_message_id(
    message: &ClientMessage,
) -> u16 {
    match message {
        ClientMessage::Authenticate { .. } =>
            MSG_AUTHENTICATE,

        ClientMessage::Heartbeat =>
            MSG_HEARTBEAT,

        ClientMessage::CreateSession =>
            MSG_CREATE_SESSION,

        ClientMessage::JoinSession { .. } =>
            MSG_JOIN_SESSION,

        ClientMessage::LeaveSession { .. } =>
            MSG_LEAVE_SESSION,

        ClientMessage::SetReady { .. } =>
            MSG_SET_READY,

        ClientMessage::StartSession =>
            MSG_START_SESSION,
    }
}

async fn cleanup_connection(
    db: &SqlitePool,
    state: &mut ConnectionState,
) {
    if let (Some(user_id), Some(session_id)) =
        (state.user_id.as_ref(), state.session_id.as_ref())
    {
        let cleanup = async {
            let mut tx = db.begin().await?;

            let host_user_id: Option<String> = sqlx::query_scalar(
                "SELECT host_user_id FROM lobbies WHERE lobby_id = ?",
            )
            .bind(session_id)
            .fetch_optional(&mut *tx)
            .await?;

            let Some(host_user_id) = host_user_id else {
                tx.commit().await?;
                return Ok::<(), sqlx::Error>(());
            };

            sqlx::query(
                "DELETE FROM lobby_players WHERE lobby_id = ? AND user_id = ?",
            )
            .bind(session_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            let remaining: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM lobby_players WHERE lobby_id = ?",
            )
            .bind(session_id)
            .fetch_one(&mut *tx)
            .await?;

            if remaining == 0 {
                sqlx::query("DELETE FROM lobbies WHERE lobby_id = ?")
                    .bind(session_id)
                    .execute(&mut *tx)
                    .await?;
            } else if host_user_id == user_id.as_str() {
                let next_host: Option<String> = sqlx::query_scalar(
                    r#"
                    SELECT user_id
                    FROM lobby_players
                    WHERE lobby_id = ?
                    ORDER BY joined_at ASC, user_id ASC
                    LIMIT 1
                    "#,
                )
                .bind(session_id)
                .fetch_optional(&mut *tx)
                .await?;

                if let Some(next_host) = next_host {
                    sqlx::query("UPDATE lobbies SET host_user_id = ? WHERE lobby_id = ?")
                        .bind(next_host)
                        .bind(session_id)
                        .execute(&mut *tx)
                        .await?;
                }
            }

            tx.commit().await?;
            Ok::<(), sqlx::Error>(())
        }
        .await;

        if let Err(error) = cleanup {
            eprintln!("TCP disconnect cleanup failed: {error}");
        }
    }

    state.session_id = None;
    state.ready = false;
    state.started = false;
}
async fn handle_message(
    message: ClientMessage,
    db: &SqlitePool,
    state: &mut ConnectionState,
) -> ServerMessage {
    match message {
        ClientMessage::Authenticate {
            player_id,
            token,
        } => {
            if state.user_id.is_some() {
                return ServerMessage::Error {
                    code: 1021,
                    message: "already_authenticated".to_string(),
                };
            }

            if player_id.trim().is_empty() || token.trim().is_empty() {
                return ServerMessage::Error {
                    code: 1014,
                    message: "invalid_credentials".to_string(),
                };
            }

            // The token must have been issued by POST /api/v1/login.
            // The client cannot choose or overwrite another user's identity.
            let identity = sqlx::query_as::<_, (String, String)>(
                "SELECT user_id, display_name FROM sessions WHERE token = ?",
            )
            .bind(token.trim())
            .fetch_optional(db)
            .await;

            match identity {
                Ok(Some((stored_id, stored_name))) if stored_id == player_id => {
                    state.user_id = Some(stored_id.clone());
                    state.display_name = Some(stored_name.clone());
                    state.ready = false;
                    state.started = false;

                    ServerMessage::Authenticated {
                        player: Player {
                            id: stored_id,
                            name: stored_name,
                            ready: false,
                        },
                    }
                }
                Ok(Some(_)) => ServerMessage::Error {
                    code: 1019,
                    message: "identity_token_mismatch".to_string(),
                },
                Ok(None) => ServerMessage::Error {
                    code: 1020,
                    message: "invalid_session_token".to_string(),
                },
                Err(error) => {
                    eprintln!("TCP authentication lookup failed: {error}");
                    ServerMessage::Error {
                        code: 1001,
                        message: "authentication_failed".to_string(),
                    }
                }
            }
        }

        ClientMessage::Heartbeat => {
            let Some(user_id) = state.user_id.as_ref() else {
                return ServerMessage::Error {
                    code: 1002,
                    message: "authentication_required".to_string(),
                };
            };

            match sqlx::query(
                "UPDATE sessions SET last_seen = CURRENT_TIMESTAMP WHERE user_id = ?",
            )
            .bind(user_id)
            .execute(db)
            .await
            {
                Ok(result) if result.rows_affected() > 0 => ServerMessage::Pong,
                Ok(_) => ServerMessage::Error {
                    code: 1020,
                    message: "invalid_session_token".to_string(),
                },
                Err(error) => {
                    eprintln!("TCP heartbeat update failed: {error}");
                    ServerMessage::Error {
                        code: 1001,
                        message: "heartbeat_failed".to_string(),
                    }
                }
            }
        }

        ClientMessage::CreateSession => {
            let Some(user_id) = state.user_id.clone() else {
                return ServerMessage::Error {
                    code: 1002,
                    message: "authentication_required".to_string(),
                };
            };

            if state.session_id.is_some() {
                return ServerMessage::Error {
                    code: 1015,
                    message: "already_in_session".to_string(),
                };
            }

            let session_id = uuid::Uuid::new_v4().to_string();
            let lobby_name = format!("KLAY Lobby {}", &session_id[..8]);

            let mut tx = match db.begin().await {
                Ok(tx) => tx,
                Err(error) => {
                    eprintln!("Failed to begin TCP session creation: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_create_failed".to_string(),
                    };
                }
            };

            if let Err(error) = sqlx::query(
                r#"
                INSERT INTO lobbies (lobby_id, name, host_user_id, max_players)
                VALUES (?, ?, ?, ?)
                "#,
            )
            .bind(&session_id)
            .bind(&lobby_name)
            .bind(&user_id)
            .bind(8i32)
            .execute(&mut *tx)
            .await
            {
                eprintln!("Failed to create TCP lobby: {error}");
                return ServerMessage::Error {
                    code: 1003,
                    message: "session_create_failed".to_string(),
                };
            }

            if let Err(error) = sqlx::query(
                "INSERT INTO lobby_players (lobby_id, user_id, ready) VALUES (?, ?, 0)",
            )
            .bind(&session_id)
            .bind(&user_id)
            .execute(&mut *tx)
            .await
            {
                eprintln!("Failed to add TCP lobby host: {error}");
                return ServerMessage::Error {
                    code: 1004,
                    message: "session_host_join_failed".to_string(),
                };
            }

            if let Err(error) = tx.commit().await {
                eprintln!("Failed to commit TCP lobby creation: {error}");
                return ServerMessage::Error {
                    code: 1009,
                    message: "session_create_failed".to_string(),
                };
            }

            match load_session(db, &session_id).await {
                Ok(session) => {
                    state.session_id = Some(session_id);
                    state.ready = false;
                    state.started = session.started;
                    ServerMessage::SessionCreated { session }
                }
                Err(error) => {
                    eprintln!("Failed to load newly created TCP lobby: {error}");
                    ServerMessage::Error {
                        code: 1005,
                        message: "session_load_failed".to_string(),
                    }
                }
            }
        }

        ClientMessage::JoinSession { session_id } => {
            if state.session_id.is_some() {
                return ServerMessage::Error {
                    code: 1022,
                    message: "leave_current_session_first".to_string(),
                };
            }

            let Some(user_id) = state.user_id.clone() else {
                return ServerMessage::Error {
                    code: 1002,
                    message: "authentication_required".to_string(),
                };
            };

            let mut tx = match db.begin().await {
                Ok(tx) => tx,
                Err(error) => {
                    eprintln!("Failed to begin TCP lobby join: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_join_failed".to_string(),
                    };
                }
            };

            let lobby = sqlx::query_as::<_, (String, i32, i64)>(
                "SELECT name, max_players, started FROM lobbies WHERE lobby_id = ?",
            )
            .bind(&session_id)
            .fetch_optional(&mut *tx)
            .await;

            let Some((_name, _max_players, started)) = (match lobby {
                Ok(lobby) => lobby,
                Err(error) => {
                    eprintln!("Failed to load TCP lobby: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_join_failed".to_string(),
                    };
                }
            }) else {
                return ServerMessage::Error {
                    code: 1006,
                    message: "session_not_found".to_string(),
                };
            };

            if started != 0 {
                return ServerMessage::Error {
                    code: 1011,
                    message: "session_already_started".to_string(),
                };
            }

            if let Err(error) = sqlx::query(
                "INSERT OR IGNORE INTO lobby_players (lobby_id, user_id, ready) VALUES (?, ?, 0)",
            )
            .bind(&session_id)
            .bind(&user_id)
            .execute(&mut *tx)
            .await
            {
                eprintln!("Failed to join TCP lobby: {error}");
                let message = if error.to_string().contains("lobby_full") {
                    "session_full"
                } else {
                    "session_join_failed"
                };
                return ServerMessage::Error {
                    code: if message == "session_full" { 1007 } else { 1008 },
                    message: message.to_string(),
                };
            }

            if let Err(error) = tx.commit().await {
                eprintln!("Failed to commit TCP lobby join: {error}");
                return ServerMessage::Error {
                    code: 1009,
                    message: "session_join_failed".to_string(),
                };
            }

            match load_session(db, &session_id).await {
                Ok(session) => {
                    state.session_id = Some(session_id);
                    state.ready = session.players.iter()
                        .find(|player| player.id == user_id)
                        .is_some_and(|player| player.ready);
                    state.started = session.started;
                    ServerMessage::SessionJoined { session }
                }
                Err(error) => {
                    eprintln!("Failed to load joined TCP session: {error}");
                    ServerMessage::Error {
                        code: 1005,
                        message: "session_load_failed".to_string(),
                    }
                }
            }
        }

        ClientMessage::LeaveSession {
            session_id,
        } => {
            let Some(user_id) = state.user_id.clone() else {
                return ServerMessage::Error {
                    code: 1002,
                    message: "authentication_required".to_string(),
                };
            };

            let mut tx = match db.begin().await {
                Ok(tx) => tx,
                Err(error) => {
                    eprintln!("Failed to begin TCP leave transaction: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_leave_failed".to_string(),
                    };
                }
            };

            let host_user_id: Option<String> = match sqlx::query_scalar(
                "SELECT host_user_id FROM lobbies WHERE lobby_id = ?",
            )
            .bind(&session_id)
            .fetch_optional(&mut *tx)
            .await
            {
                Ok(host_user_id) => host_user_id,
                Err(error) => {
                    eprintln!("Failed to load lobby before TCP leave: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_leave_failed".to_string(),
                    };
                }
            };

            let Some(host_user_id) = host_user_id else {
                return ServerMessage::Error {
                    code: 1006,
                    message: "session_not_found".to_string(),
                };
            };

            let _removed = match sqlx::query(
                "DELETE FROM lobby_players WHERE lobby_id = ? AND user_id = ?",
            )
            .bind(&session_id)
            .bind(&user_id)
            .execute(&mut *tx)
            .await
            {
                Ok(result) => result.rows_affected() > 0,
                Err(error) => {
                    eprintln!("Failed to remove TCP lobby member: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_leave_failed".to_string(),
                    };
                }
            };

            let remaining: i64 = match sqlx::query_scalar(
                "SELECT COUNT(*) FROM lobby_players WHERE lobby_id = ?",
            )
            .bind(&session_id)
            .fetch_one(&mut *tx)
            .await
            {
                Ok(count) => count,
                Err(error) => {
                    eprintln!("Failed to count TCP lobby members: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_leave_failed".to_string(),
                    };
                }
            };

            if remaining == 0 {
                if let Err(error) = sqlx::query("DELETE FROM lobbies WHERE lobby_id = ?")
                    .bind(&session_id)
                    .execute(&mut *tx)
                    .await
                {
                    eprintln!("Failed to delete empty TCP lobby: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_leave_failed".to_string(),
                    };
                }
            } else if host_user_id == user_id {
                let next_host: Option<String> = match sqlx::query_scalar(
                    r#"
                    SELECT user_id
                    FROM lobby_players
                    WHERE lobby_id = ?
                    ORDER BY joined_at ASC, user_id ASC
                    LIMIT 1
                    "#,
                )
                .bind(&session_id)
                .fetch_optional(&mut *tx)
                .await
                {
                    Ok(next_host) => next_host,
                    Err(error) => {
                        eprintln!("Failed to select TCP replacement host: {error}");
                        return ServerMessage::Error {
                            code: 1009,
                            message: "session_leave_failed".to_string(),
                        };
                    }
                };

                if let Some(next_host) = next_host
                    && let Err(error) = sqlx::query(
                        "UPDATE lobbies SET host_user_id = ? WHERE lobby_id = ?",
                    )
                    .bind(next_host)
                    .bind(&session_id)
                    .execute(&mut *tx)
                    .await
                {
                    eprintln!("Failed to transfer TCP lobby host: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_leave_failed".to_string(),
                    };
                }
            }

            if let Err(error) = tx.commit().await {
                eprintln!("Failed to commit TCP leave transaction: {error}");
                return ServerMessage::Error {
                    code: 1009,
                    message: "session_leave_failed".to_string(),
                };
            }

            if state.session_id.as_deref() == Some(session_id.as_str()) {
                state.session_id = None;
                state.ready = false;
                state.started = false;
            }

            ServerMessage::SessionLeft { session_id }
        }

        ClientMessage::SetReady { ready } => {
            let Some(user_id) =
                state.user_id.clone()
            else {
                return ServerMessage::Error {
                    code: 1002,
                    message:
                        "authentication_required"
                            .to_string(),
                };
            };

            let Some(session_id) =
                state.session_id.clone()
            else {
                return ServerMessage::Error {
                    code: 1010,
                    message:
                        "session_required"
                            .to_string(),
                };
            };

            let result = sqlx::query(
                r#"
                UPDATE lobby_players
                SET ready = ?
                WHERE lobby_id = ?
                AND user_id = ?
                AND EXISTS (
                    SELECT 1 FROM lobbies
                    WHERE lobby_id = ? AND started = 0
                )
                "#,
            )
            .bind(if ready {
                1i64
            } else {
                0i64
            })
            .bind(&session_id)
            .bind(&user_id)
            .bind(&session_id)
            .execute(db)
            .await;

            match result {
                Ok(result)
                    if result.rows_affected() > 0 =>
                {
                    state.ready = ready;

                    ServerMessage::ReadyChanged {
                        player_id: user_id,
                        ready,
                    }
                }

                Ok(_) => {
                    match sqlx::query_scalar::<_, i64>(
                        "SELECT started FROM lobbies WHERE lobby_id = ?",
                    )
                    .bind(&session_id)
                    .fetch_optional(db)
                    .await
                    {
                        Ok(Some(1)) => ServerMessage::Error {
                            code: 1011,
                            message: "session_already_started".to_string(),
                        },
                        Ok(_) => ServerMessage::Error {
                            code: 1016,
                            message: "player_not_in_session".to_string(),
                        },
                        Err(error) => {
                            eprintln!("Failed to inspect lobby state after ready update: {error}");
                            ServerMessage::Error {
                                code: 1017,
                                message: "ready_update_failed".to_string(),
                            }
                        }
                    }
                }

                Err(error) => {
                    eprintln!(
                        "Failed to update ready state: {error}"
                    );

                    ServerMessage::Error {
                        code: 1017,
                        message:
                            "ready_update_failed"
                                .to_string(),
                    }
                }
            }
        }

        ClientMessage::StartSession => {
            let Some(user_id) = state.user_id.clone() else {
                return ServerMessage::Error {
                    code: 1002,
                    message: "authentication_required".to_string(),
                };
            };
            let Some(session_id) = state.session_id.clone() else {
                return ServerMessage::Error {
                    code: 1010,
                    message: "session_required".to_string(),
                };
            };

            let mut tx = match db.begin().await {
                Ok(tx) => tx,
                Err(error) => {
                    eprintln!("Failed to begin session start transaction: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_start_failed".to_string(),
                    };
                }
            };

            let lobby = sqlx::query_as::<_, (String, i64)>(
                "SELECT host_user_id, started FROM lobbies WHERE lobby_id = ?",
            )
            .bind(&session_id)
            .fetch_optional(&mut *tx)
            .await;

            let Some((host_user_id, started)) = (match lobby {
                Ok(lobby) => lobby,
                Err(error) => {
                    eprintln!("Failed to load lobby for start: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_start_failed".to_string(),
                    };
                }
            }) else {
                return ServerMessage::Error {
                    code: 1006,
                    message: "session_not_found".to_string(),
                };
            };

            if host_user_id != user_id {
                return ServerMessage::Error {
                    code: 1012,
                    message: "host_required".to_string(),
                };
            }
            if started != 0 {
                return ServerMessage::Error {
                    code: 1011,
                    message: "session_already_started".to_string(),
                };
            }

            let (players, not_ready): (i64, i64) = match sqlx::query_as(
                r#"
                SELECT COUNT(*), COALESCE(SUM(CASE WHEN ready = 0 THEN 1 ELSE 0 END), 0)
                FROM lobby_players
                WHERE lobby_id = ?
                "#,
            )
            .bind(&session_id)
            .fetch_one(&mut *tx)
            .await
            {
                Ok(counts) => counts,
                Err(error) => {
                    eprintln!("Failed to validate players before session start: {error}");
                    return ServerMessage::Error {
                        code: 1009,
                        message: "session_start_failed".to_string(),
                    };
                }
            };

            if players == 0 {
                return ServerMessage::Error {
                    code: 1013,
                    message: "session_empty".to_string(),
                };
            }
            if not_ready > 0 {
                return ServerMessage::Error {
                    code: 1018,
                    message: "players_not_ready".to_string(),
                };
            }

            if let Err(error) = sqlx::query(
                "UPDATE lobbies SET started = 1 WHERE lobby_id = ? AND started = 0",
            )
            .bind(&session_id)
            .execute(&mut *tx)
            .await
            {
                eprintln!("Failed to persist session start: {error}");
                return ServerMessage::Error {
                    code: 1009,
                    message: "session_start_failed".to_string(),
                };
            }

            if let Err(error) = tx.commit().await {
                eprintln!("Failed to commit session start: {error}");
                return ServerMessage::Error {
                    code: 1009,
                    message: "session_start_failed".to_string(),
                };
            }

            state.started = true;
            ServerMessage::SessionStarted { session_id }
        }
    }
}

async fn load_session(
    db: &SqlitePool,
    session_id: &str,
) -> Result<Session, sqlx::Error> {
    let started: Option<i64> = sqlx::query_scalar(
        "SELECT started FROM lobbies WHERE lobby_id = ?",
    )
    .bind(session_id)
    .fetch_optional(db)
    .await?;

    let Some(started) = started else {
        return Err(sqlx::Error::RowNotFound);
    };

    let rows =
        sqlx::query_as::<_, (String, String, i64)>(
            r#"
            SELECT
                s.user_id,
                s.display_name,
                lp.ready
            FROM lobby_players lp
            INNER JOIN sessions s
                ON s.user_id = lp.user_id
            WHERE lp.lobby_id = ?
            ORDER BY lp.joined_at ASC
            "#,
        )
        .bind(session_id)
        .fetch_all(db)
        .await?;

    let players = rows
        .into_iter()
        .map(|(id, name, ready)| Player {
            id,
            name,
            ready: ready != 0,
        })
        .collect();

    Ok(Session {
        id: session_id.to_string(),
        players,
        started: started != 0,
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[tokio::test]
    async fn load_session_reports_persisted_started_state() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory SQLite database should open");

        sqlx::query(
            r#"
            CREATE TABLE sessions (
                user_id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL
            )
            "#,
        )
        .execute(&db)
        .await
        .expect("sessions table should be created");

        sqlx::query(
            r#"
            CREATE TABLE lobbies (
                lobby_id TEXT PRIMARY KEY,
                started INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&db)
        .await
        .expect("lobbies table should be created");

        sqlx::query(
            r#"
            CREATE TABLE lobby_players (
                lobby_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                joined_at TEXT DEFAULT CURRENT_TIMESTAMP,
                ready INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (lobby_id, user_id)
            )
            "#,
        )
        .execute(&db)
        .await
        .expect("lobby_players table should be created");

        sqlx::query("INSERT INTO sessions (user_id, display_name) VALUES ('p1', 'Player 1')")
            .execute(&db)
            .await
            .expect("player should be inserted");
        sqlx::query("INSERT INTO lobbies (lobby_id, started) VALUES ('l1', 0)")
            .execute(&db)
            .await
            .expect("lobby should be inserted");
        sqlx::query(
            "INSERT INTO lobby_players (lobby_id, user_id, ready) VALUES ('l1', 'p1', 1)",
        )
        .execute(&db)
        .await
        .expect("lobby member should be inserted");

        let waiting = load_session(&db, "l1")
            .await
            .expect("waiting lobby should load");
        assert!(!waiting.started);
        assert_eq!(waiting.players.len(), 1);
        assert!(waiting.players[0].ready);

        sqlx::query("UPDATE lobbies SET started = 1 WHERE lobby_id = 'l1'")
            .execute(&db)
            .await
            .expect("lobby should be marked started");

        let started = load_session(&db, "l1")
            .await
            .expect("started lobby should load");
        assert!(started.started);
    }
}
