use std::io;

use sqlx::SqlitePool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
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

    loop {
        let (stream, peer) = listener.accept().await?;

        println!("TCP client connected: {peer}");

        let client_db = db.clone();

        tokio::spawn(async move {
            if let Err(error) =
                handle_client(stream, client_db).await
            {
                eprintln!(
                    "TCP client error ({peer}): {error}"
                );
            }

            println!(
                "TCP client disconnected: {peer}"
            );
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
        match sqlx::query(
            "DELETE FROM lobby_players WHERE lobby_id = ? AND user_id = ?",
        )
        .bind(session_id)
        .bind(user_id)
        .execute(db)
        .await
        {
            Ok(result) if result.rows_affected() > 0 => {
                let remaining = sqlx::query_scalar::<_, i64>(
                    "SELECT COUNT(*) FROM lobby_players WHERE lobby_id = ?",
                )
                .bind(session_id)
                .fetch_one(db)
                .await;

                match remaining {
                    Ok(0) => {
                        if let Err(error) = sqlx::query(
                            "DELETE FROM lobbies WHERE lobby_id = ?",
                        )
                        .bind(session_id)
                        .execute(db)
                        .await
                        {
                            eprintln!("TCP cleanup failed to delete empty lobby: {error}");
                        }
                    }
                    Ok(_) => {
                        let next_host = sqlx::query_scalar::<_, String>(
                            r#"
                            SELECT user_id
                            FROM lobby_players
                            WHERE lobby_id = ?
                            ORDER BY joined_at ASC, user_id ASC
                            LIMIT 1
                            "#,
                        )
                        .bind(session_id)
                        .fetch_optional(db)
                        .await;

                        match next_host {
                            Ok(Some(next_host)) => {
                                if let Err(error) = sqlx::query(
                                    "UPDATE lobbies SET host_user_id = ? WHERE lobby_id = ? AND host_user_id = ?",
                                )
                                .bind(next_host)
                                .bind(session_id)
                                .bind(user_id)
                                .execute(db)
                                .await
                                {
                                    eprintln!("TCP cleanup failed to transfer lobby host: {error}");
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                eprintln!("TCP cleanup failed to select next lobby host: {error}");
                            }
                        }
                    }
                    Err(error) => {
                        eprintln!("TCP cleanup failed to count lobby members: {error}");
                    }
                }
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("TCP cleanup failed to remove disconnected player: {error}");
            }
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

            if let Err(error) = sqlx::query(
                "UPDATE sessions SET last_seen = CURRENT_TIMESTAMP WHERE user_id = ?",
            )
            .bind(user_id)
            .execute(db)
            .await
            {
                eprintln!("TCP heartbeat update failed: {error}");
                return ServerMessage::Error {
                    code: 1001,
                    message: "heartbeat_failed".to_string(),
                };
            }

            ServerMessage::Pong
        }

        ClientMessage::CreateSession => {
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

            if state.session_id.is_some() {
                return ServerMessage::Error {
                    code: 1015,
                    message:
                        "already_in_session"
                            .to_string(),
                };
            }

            let session_id =
                uuid::Uuid::new_v4().to_string();

            let lobby_name =
                format!(
                    "KLAY Lobby {}",
                    &session_id[..8]
                );

            let result = sqlx::query(
                r#"
                INSERT INTO lobbies (
                    lobby_id,
                    name,
                    host_user_id,
                    max_players
                )
                VALUES (?, ?, ?, ?)
                "#,
            )
            .bind(&session_id)
            .bind(&lobby_name)
            .bind(&user_id)
            .bind(8i32)
            .execute(db)
            .await;

            if let Err(error) = result {
                eprintln!(
                    "Failed to create session: {error}"
                );

                return ServerMessage::Error {
                    code: 1003,
                    message:
                        "session_create_failed"
                            .to_string(),
                };
            }

            let result = sqlx::query(
                r#"
                INSERT INTO lobby_players (
                    lobby_id,
                    user_id,
                    ready
                )
                VALUES (?, ?, 0)
                "#,
            )
            .bind(&session_id)
            .bind(&user_id)
            .execute(db)
            .await;

            if let Err(error) = result {
                eprintln!(
                    "Failed to add session host: {error}"
                );

                let _ = sqlx::query(
                    "DELETE FROM lobbies WHERE lobby_id = ?",
                )
                .bind(&session_id)
                .execute(db)
                .await;

                return ServerMessage::Error {
                    code: 1004,
                    message:
                        "session_host_join_failed"
                            .to_string(),
                };
            }

            state.session_id =
                Some(session_id.clone());

            state.ready = false;
            state.started = false;

            match load_session(
                db,
                &session_id,
            )
            .await
            {
                Ok(session) => {
                    ServerMessage::SessionCreated {
                        session,
                    }
                }

                Err(_) => {
                    ServerMessage::Error {
                        code: 1005,
                        message:
                            "session_load_failed"
                                .to_string(),
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

            let lobby = sqlx::query_as::<_, (String, i32)>(
                "SELECT name, max_players FROM lobbies WHERE lobby_id = ?",
            )
            .bind(&session_id)
            .fetch_optional(&mut *tx)
            .await;

            let Some((_name, _max_players)) = (match lobby {
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

            if state.started {
                return ServerMessage::Error {
                    code: 1011,
                    message:
                        "session_already_started"
                            .to_string(),
                };
            }

            let result = sqlx::query(
                r#"
                UPDATE lobby_players
                SET ready = ?
                WHERE lobby_id = ?
                AND user_id = ?
                "#,
            )
            .bind(if ready {
                1i64
            } else {
                0i64
            })
            .bind(&session_id)
            .bind(&user_id)
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
                    ServerMessage::Error {
                        code: 1016,
                        message:
                            "player_not_in_session"
                                .to_string(),
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

            let host =
                sqlx::query_scalar::<_, String>(
                    r#"
                    SELECT host_user_id
                    FROM lobbies
                    WHERE lobby_id = ?
                    "#,
                )
                .bind(&session_id)
                .fetch_optional(db)
                .await;

            let Some(host_user_id) =
                host.ok().flatten()
            else {
                return ServerMessage::Error {
                    code: 1006,
                    message:
                        "session_not_found"
                            .to_string(),
                };
            };

            if host_user_id != user_id {
                return ServerMessage::Error {
                    code: 1012,
                    message:
                        "host_required"
                            .to_string(),
                };
            }

            let players: i64 =
                sqlx::query_scalar(
                    r#"
                    SELECT COUNT(*)
                    FROM lobby_players
                    WHERE lobby_id = ?
                    "#,
                )
                .bind(&session_id)
                .fetch_one(db)
                .await
                .unwrap_or(0);

            if players == 0 {
                return ServerMessage::Error {
                    code: 1013,
                    message:
                        "session_empty"
                            .to_string(),
                };
            }

            let not_ready_players: i64 =
                sqlx::query_scalar(
                    r#"
                    SELECT COUNT(*)
                    FROM lobby_players
                    WHERE lobby_id = ?
                    AND ready = 0
                    "#,
                )
                .bind(&session_id)
                .fetch_one(db)
                .await
                .unwrap_or(0);

            if not_ready_players > 0 {
                return ServerMessage::Error {
                    code: 1018,
                    message:
                        "players_not_ready"
                            .to_string(),
                };
            }

            state.started = true;

            ServerMessage::SessionStarted {
                session_id,
            }
        }
    }
}

async fn load_session(
    db: &SqlitePool,
    session_id: &str,
) -> Result<Session, sqlx::Error> {
    let exists: Option<(String,)> =
        sqlx::query_as(
            r#"
            SELECT lobby_id
            FROM lobbies
            WHERE lobby_id = ?
            "#,
        )
        .bind(session_id)
        .fetch_optional(db)
        .await?;

    if exists.is_none() {
        return Err(
            sqlx::Error::RowNotFound
        );
    }

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
        started: false,
    })
}
