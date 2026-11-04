use std::env;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode};
use axum::routing::get;
use axum::routing::post;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;
use sqlx::SqlitePool;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions,
};
use tower_http::cors::CorsLayer;
use uuid::Uuid;

mod protocol;


const SERVER_VERSION: &str = "0.5.0";
const DEFAULT_MAX_PLAYERS: i32 = 8;

#[derive(Clone)]
struct AppState {
    db: SqlitePool,
    public_url: String,
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthRequest {
    token: String,
}

#[derive(Debug, Deserialize)]
struct LobbyCreateRequest {
    token: String,
    name: Option<String>,
    max_players: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct LobbyJoinRequest {
    token: String,
}

#[derive(Debug, Deserialize)]
struct OfflineMatchRequest {
    token: String,
    match_id: String,
    mode: String,
    completed: bool,
}

#[derive(Debug, Deserialize)]
struct OfflineStartRequest {
    token: String,
    match_id: String,
    mode: String,
}

#[derive(Debug, Serialize)]
struct LoginResponse {
    success: bool,
    token: String,
    #[serde(rename = "userId")]
    user_id: String,
    #[serde(rename = "displayName")]
    display_name: String,
    status: String,
}

#[tokio::main]
async fn main() {
    let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

    let port = env::var("PORT").unwrap_or_else(|_| "8080".to_string());

    let public_url = env::var("PUBLIC_URL")
        .unwrap_or_else(|_| "https://f13-custom-backend-production.up.railway.app".to_string());

    let database_url =
        env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:///tmp/f13.db".to_string());

    println!("========================================");
    println!("KLAY Friday the 13th Private Server");
    println!("Version: {SERVER_VERSION}");
    println!("========================================");
    println!("Host: {host}");
    println!("Port: {port}");
    println!("Server URL: {public_url}");
    println!("Database: {database_url}");
    println!("========================================");

    let sqlite_options = SqliteConnectOptions::from_str(&database_url)
        .expect("Invalid DATABASE_URL")
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));

    if let Some(parent) = std::path::Path::new(sqlite_options.get_filename())
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .expect("Failed to create database directory");
    }

    let db = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(sqlite_options)
        .await
        .expect("Failed to connect to SQLite database");

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS sessions (
            user_id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            token TEXT NOT NULL UNIQUE,
            created_at TEXT DEFAULT CURRENT_TIMESTAMP,
            last_seen TEXT DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(&db)
    .await
    .expect("Failed to create sessions table");

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS lobbies (
            lobby_id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            host_user_id TEXT NOT NULL,
            max_players INTEGER NOT NULL,
            created_at TEXT DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(&db)
    .await
    .expect("Failed to create lobbies table");

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS lobby_players (
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
    .expect("Failed to create lobby_players table");

    let ready_column: Option<String> = sqlx::query_scalar(
        r#"
        SELECT name
        FROM pragma_table_info('lobby_players')
        WHERE name = 'ready'
        "#,
    )
    .fetch_optional(&db)
    .await
    .expect("Failed to inspect lobby_players table");

    if ready_column.is_none() {
        sqlx::query(
            r#"
            ALTER TABLE lobby_players
            ADD COLUMN ready INTEGER NOT NULL DEFAULT 0
            "#,
        )
        .execute(&db)
        .await
        .expect("Failed to add ready column to lobby_players");

        println!("Lobby players ready column added");
    }

    // Enforce lobby capacity inside SQLite as well as in request handlers.
    // SQLite serializes writers, so this trigger closes the check-then-insert race
    // when multiple clients attempt to occupy the final slot simultaneously.
    sqlx::query(
        r#"
        CREATE TRIGGER IF NOT EXISTS enforce_lobby_capacity
        BEFORE INSERT ON lobby_players
        WHEN NOT EXISTS (
            SELECT 1
            FROM lobby_players
            WHERE lobby_id = NEW.lobby_id
              AND user_id = NEW.user_id
        )
        AND (
            SELECT COUNT(*)
            FROM lobby_players
            WHERE lobby_id = NEW.lobby_id
        ) >= COALESCE(
            (SELECT max_players FROM lobbies WHERE lobby_id = NEW.lobby_id),
            0
        )
        BEGIN
            SELECT RAISE(ABORT, 'lobby_full');
        END
        "#,
    )
    .execute(&db)
    .await
    .expect("Failed to create lobby capacity trigger");

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS offline_matches (
            user_id TEXT NOT NULL,
            match_id TEXT NOT NULL,
            mode TEXT NOT NULL CHECK (mode IN ('counselor', 'jason')),
            completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (user_id, match_id)
        )
        "#,
    )
    .execute(&db)
    .await
    .expect("Failed to create offline_matches table");

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS klay_achievements (
            user_id TEXT NOT NULL,
            achievement_id TEXT NOT NULL,
            unlocked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (user_id, achievement_id)
        )
        "#,
    )
    .execute(&db)
    .await
    .expect("Failed to create klay_achievements table");

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS offline_match_starts (
            user_id TEXT NOT NULL,
            match_id TEXT NOT NULL,
            mode TEXT NOT NULL CHECK (mode IN ('counselor', 'jason')),
            started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (user_id, match_id)
        )
        "#,
    )
    .execute(&db)
    .await
    .expect("Failed to create offline_match_starts table");

    println!("Database connected");
    println!("Sessions table ready");
    println!("Lobbies table ready");
    println!("Lobby players table ready");
    println!("Offline matches table ready");
    println!("KLAY achievements table ready");

    let state = AppState {
        db: db.clone(),
        public_url: public_url.clone(),
    };

    let tcp_host = env::var("TCP_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

    let tcp_port = env::var("TCP_PORT").unwrap_or_else(|_| "9000".to_string());

    let tcp_addr = format!("{tcp_host}:{tcp_port}");
    let tcp_db = db.clone();

    tokio::spawn(async move {
        if let Err(error) = protocol::tcp::run_tcp_server(tcp_addr, tcp_db).await {
            eprintln!("Protocol TCP server stopped: {error}");
        }
    });

    let cors = CorsLayer::new()
        .allow_origin(
            public_url
                .parse::<HeaderValue>()
                .expect("Invalid PUBLIC_URL"),
        )
        .allow_methods(tower_http::cors::Any)
        .allow_headers(tower_http::cors::Any);

    let app = Router::new()
        .route("/", get(home_handler))
        .route("/health", get(health_handler))
        .route("/api/v1/protocol", post(protocol_unavailable_handler))
        .route("/api/v1/login", post(login_handler))
        .route("/api/v1/auth/psn", post(psn_auth_unavailable_handler))
        .route("/api/v1/session/heartbeat", post(heartbeat_handler))
        .route("/api/v1/session/validate", post(validate_session_handler))
        .route("/api/v1/profiles/me", get(profile_handler))
        .route(
            "/api/v1/trophies/offline-start",
            post(record_offline_start_handler),
        )
        .route(
            "/api/v1/trophies/offline-match",
            post(record_offline_match_handler),
        )
        .route("/api/v1/trophies/progress", post(trophy_progress_handler))
        .route("/api/v1/database/status", get(db_check_handler))
        .route("/api/v1/database_check", get(db_check_handler))
        .route("/api/v1/server/info", get(server_info_handler))
        .route("/api/v1/lobbies", get(list_lobbies_handler))
        .route("/api/v1/lobbies/create", post(create_lobby_handler))
        .route("/api/v1/lobbies/:lobby_id", get(get_lobby_handler))
        .route("/api/v1/lobbies/:lobby_id/join", post(join_lobby_handler))
        .route("/api/v1/lobbies/:lobby_id/leave", post(leave_lobby_handler))
        .with_state(state)
        .layer(cors);

    let addr: SocketAddr = format!("{host}:{port}")
        .parse()
        .expect("Invalid HOST or PORT");

    println!("Server listening on {addr}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind server");

    axum::serve(listener, app).await.expect("Server failed");
}

async fn home_handler() -> &'static str {
    "KLAY Friday the 13th Private Server v0.5.0"
}

async fn health_handler(State(state): State<AppState>) -> Json<Value> {
    let database = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();

    Json(json!({
        "status": "ok",
        "service": "f13-custom-backend",
        "version": SERVER_VERSION,
        "database": if database { "connected" } else { "disconnected" },
        "public_url": state.public_url
    }))
}

async fn protocol_unavailable_handler() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "success": false,
            "error": "original_game_protocol_not_implemented",
            "message": "This HTTP endpoint is not a substitute for the original game's protocol."
        })),
    )
}

async fn psn_auth_unavailable_handler() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({
            "success": false,
            "error": "psn_authentication_not_implemented",
            "message": "Use /api/v1/login only for a KLAY test account. This service does not authenticate PlayStation Network accounts."
        })),
    )
}

async fn login_handler(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Json<Value> {
    let display_name = request
        .display_name
        .unwrap_or_else(|| "KLAY_Player".to_string())
        .trim()
        .to_string();

    if display_name.is_empty() || display_name.chars().count() > 32 {
        return Json(json!({
            "success": false,
            "status": "invalid_display_name",
            "message": "Display name must contain between 1 and 32 characters."
        }));
    }

    let user_id = Uuid::new_v4().to_string();
    let token = Uuid::new_v4().to_string();

    let result = sqlx::query(
        r#"
        INSERT INTO sessions (user_id, display_name, token)
        VALUES (?, ?, ?)
        "#,
    )
    .bind(&user_id)
    .bind(&display_name)
    .bind(&token)
    .execute(&state.db)
    .await;

    if let Err(error) = result {
        eprintln!("Failed to create session: {error}");
        return Json(json!({
            "success": false,
            "status": "database_error"
        }));
    }

    println!("New session: user={} name={}", user_id, display_name);

    Json(json!(LoginResponse {
        success: true,
        token,
        user_id,
        display_name,
        status: "success".to_string(),
    }))
}

async fn heartbeat_handler(
    State(state): State<AppState>,
    Json(request): Json<AuthRequest>,
) -> Json<Value> {
    let result = sqlx::query(
        r#"
        UPDATE sessions
        SET last_seen = CURRENT_TIMESTAMP
        WHERE token = ?
        "#,
    )
    .bind(&request.token)
    .execute(&state.db)
    .await;

    match result {
        Ok(result) if result.rows_affected() > 0 => Json(json!({
            "success": true,
            "status": "active"
        })),

        _ => Json(json!({
            "success": false,
            "status": "invalid_session"
        })),
    }
}

async fn validate_session_handler(
    State(state): State<AppState>,
    Json(request): Json<AuthRequest>,
) -> Json<Value> {
    let session = sqlx::query_as::<_, (String, String)>(
        r#"
        SELECT user_id, display_name
        FROM sessions
        WHERE token = ?
        "#,
    )
    .bind(&request.token)
    .fetch_optional(&state.db)
    .await;

    match session {
        Ok(Some((user_id, display_name))) => Json(json!({
            "success": true,
            "valid": true,
            "userId": user_id,
            "displayName": display_name
        })),

        _ => Json(json!({
            "success": true,
            "valid": false
        })),
    }
}

async fn profile_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Json<Value> {
    let token = match headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        Some(token) if !token.trim().is_empty() => token.trim(),
        _ => {
            return Json(json!({
                "success": false,
                "error": "authentication_required"
            }));
        }
    };

    let session = sqlx::query_as::<_, (String, String)>(
        "SELECT user_id, display_name FROM sessions WHERE token = ?",
    )
    .bind(token)
    .fetch_optional(&state.db)
    .await;

    let (user_id, display_name) = match session {
        Ok(Some(session)) => session,
        Ok(None) => {
            return Json(json!({
                "success": false,
                "error": "invalid_session"
            }));
        }
        Err(_) => {
            return Json(json!({
                "success": false,
                "error": "database_error"
            }));
        }
    };

    Json(json!({
        "success": true,
        "profile": {
            "user_id": user_id,
            "display_name": display_name
        },
        "custom_catalog": {
            "assets": [
                "savini_jason",
                "backers_clothing_pack",
                "counselor_clothing_dlc"
            ],
            "customization": {
                "perkSlots": 3,
                "badgeSlots": 3
            }
        },
        "note": "Custom catalog only. Official game and platform entitlements are unchanged."
    }))
}
async fn record_offline_start_handler(
    State(state): State<AppState>,
    Json(request): Json<OfflineStartRequest>,
) -> Json<Value> {
    let token = request.token.trim();
    let match_id = request.match_id.trim();

    if token.is_empty() || match_id.is_empty() {
        return Json(json!({
            "success": false,
            "error": "missing_token_or_match_id"
        }));
    }

    if match_id.len() > 128 {
        return Json(json!({
            "success": false,
            "error": "match_id_too_long"
        }));
    }

    if request.mode != "counselor" && request.mode != "jason" {
        return Json(json!({
            "success": false,
            "error": "invalid_mode",
            "allowed_modes": ["counselor", "jason"]
        }));
    }

    let user_id: Option<String> =
        match sqlx::query_scalar("SELECT user_id FROM sessions WHERE token = ?")
            .bind(token)
            .fetch_optional(&state.db)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                eprintln!("Session lookup failed: {error}");
                return Json(json!({
                    "success": false,
                    "error": "database_error"
                }));
            }
        };

    let Some(user_id) = user_id else {
        return Json(json!({
            "success": false,
            "error": "invalid_session"
        }));
    };

    let inserted = match sqlx::query(
        r#"
        INSERT OR IGNORE INTO offline_match_starts
            (user_id, match_id, mode)
        VALUES (?, ?, ?)
        "#,
    )
    .bind(&user_id)
    .bind(match_id)
    .bind(&request.mode)
    .execute(&state.db)
    .await
    {
        Ok(result) => result.rows_affected() > 0,
        Err(error) => {
            eprintln!("Failed to record match start: {error}");
            return Json(json!({
                "success": false,
                "error": "database_error"
            }));
        }
    };

    let unlock_result = sqlx::query(
        r#"
        WITH RECURSIVE seq(x) AS (
            SELECT 1
            UNION ALL
            SELECT x + 1 FROM seq WHERE x < 999
        )
        INSERT OR IGNORE INTO klay_achievements
            (user_id, achievement_id)
        SELECT ?, 'offline_trophy_' || printf('%03d', x)
        FROM seq
        "#,
    )
    .bind(&user_id)
    .execute(&state.db)
    .await;

    if let Err(error) = unlock_result {
        eprintln!("Failed to save achievements: {error}");
        return Json(json!({
            "success": false,
            "error": "achievement_save_failed"
        }));
    }

    let unlocked: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM klay_achievements
        WHERE user_id = ?
          AND achievement_id LIKE 'offline_trophy_%'
        "#,
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Json(json!({
        "success": true,
        "event": "offline_match_started",
        "recorded": inserted,
        "duplicate": !inserted,
        "match_id": match_id,
        "mode": request.mode,
        "custom_achievements_unlocked": unlocked,
        "custom_achievements_total": 999,
        "note": "Custom KLAY backend records only, not official PSN trophies"
    }))
}

async fn record_offline_match_handler(
    State(state): State<AppState>,
    Json(request): Json<OfflineMatchRequest>,
) -> Json<Value> {
    let token = request.token.trim();
    let match_id = request.match_id.trim();

    if token.is_empty() || match_id.is_empty() {
        return Json(json!({
            "success": false,
            "error": "missing_token_or_match_id"
        }));
    }

    if match_id.len() > 128 {
        return Json(json!({
            "success": false,
            "error": "match_id_too_long"
        }));
    }

    if !request.completed {
        return Json(json!({
            "success": false,
            "error": "match_not_completed"
        }));
    }

    if request.mode != "counselor" && request.mode != "jason" {
        return Json(json!({
            "success": false,
            "error": "invalid_mode",
            "allowed_modes": ["counselor", "jason"]
        }));
    }

    let user_id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM sessions WHERE token = ?")
            .bind(token)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let Some(user_id) = user_id else {
        return Json(json!({
            "success": false,
            "error": "invalid_session"
        }));
    };

    let inserted = sqlx::query(
        r#"
        INSERT OR IGNORE INTO offline_matches (user_id, match_id, mode)
        VALUES (?, ?, ?)
        "#,
    )
    .bind(&user_id)
    .bind(match_id)
    .bind(&request.mode)
    .execute(&state.db)
    .await;

    let inserted = match inserted {
        Ok(result) => result.rows_affected() > 0,
        Err(error) => {
            eprintln!("Failed to record offline match: {error}");
            return Json(json!({
                "success": false,
                "error": "database_error"
            }));
        }
    };

    let achievement_id = match request.mode.as_str() {
        "counselor" => "offline_counselor_first_match",
        "jason" => "offline_jason_first_match",
        _ => unreachable!(),
    };

    if inserted
        && let Err(error) = sqlx::query(
            r#"
            INSERT OR IGNORE INTO klay_achievements (user_id, achievement_id)
            VALUES (?, ?)
            "#,
        )
        .bind(&user_id)
        .bind(achievement_id)
        .execute(&state.db)
        .await
    {
        eprintln!("Failed to record KLAY achievement: {error}");
        return Json(json!({
            "success": false,
            "error": "achievement_save_failed"
        }));
    }

    let counselor_matches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM offline_matches WHERE user_id = ? AND mode = 'counselor'",
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let jason_matches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM offline_matches WHERE user_id = ? AND mode = 'jason'",
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Json(json!({
        "success": true,
        "recorded": inserted,
        "duplicate": !inserted,
        "mode": request.mode,
        "match_id": match_id,
        "achievement": achievement_id,
        "progress": {
            "offline_counselor_matches": counselor_matches,
            "offline_jason_matches": jason_matches,
            "total_offline_matches": counselor_matches + jason_matches
        },
        "note": "Backend tracking only; not official PSN trophies"
    }))
}

async fn trophy_progress_handler(
    State(state): State<AppState>,
    Json(request): Json<AuthRequest>,
) -> Json<Value> {
    let token = request.token.trim();

    if token.is_empty() {
        return Json(json!({
            "success": false,
            "error": "missing_token"
        }));
    }

    let user_id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM sessions WHERE token = ?")
            .bind(token)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let Some(user_id) = user_id else {
        return Json(json!({
            "success": false,
            "error": "invalid_session"
        }));
    };

    let counselor_matches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM offline_matches WHERE user_id = ? AND mode = 'counselor'",
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let jason_matches: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM offline_matches WHERE user_id = ? AND mode = 'jason'",
    )
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    let achievements: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT achievement_id, unlocked_at
        FROM klay_achievements
        WHERE user_id = ?
        ORDER BY unlocked_at ASC
        "#,
    )
    .bind(&user_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let achievement_list: Vec<Value> = achievements
        .into_iter()
        .map(|(id, unlocked_at)| {
            json!({
                "id": id,
                "unlocked_at": unlocked_at
            })
        })
        .collect();

    Json(json!({
        "success": true,
        "user_id": user_id,
        "progress": {
            "offline_counselor_matches": counselor_matches,
            "offline_jason_matches": jason_matches,
            "total_offline_matches": counselor_matches + jason_matches
        },
        "achievements": achievement_list,
        "note": "Backend tracking only; not official PSN trophies"
    }))
}

async fn create_lobby_handler(
    State(state): State<AppState>,
    Json(request): Json<LobbyCreateRequest>,
) -> Json<Value> {
    let session = sqlx::query_as::<_, (String, String)>(
        r#"
        SELECT user_id, display_name
        FROM sessions
        WHERE token = ?
        "#,
    )
    .bind(&request.token)
    .fetch_optional(&state.db)
    .await;

    let Some((user_id, display_name)) = session.ok().flatten() else {
        return Json(json!({
            "success": false,
            "error": "invalid_session"
        }));
    };

    let lobby_id = Uuid::new_v4().to_string();

    let name = request
        .name
        .unwrap_or_else(|| format!("{display_name}'s Lobby"));

    let max_players = request
        .max_players
        .unwrap_or(DEFAULT_MAX_PLAYERS)
        .clamp(1, DEFAULT_MAX_PLAYERS);

    let create_result = sqlx::query(
        r#"
        INSERT INTO lobbies (lobby_id, name, host_user_id, max_players)
        VALUES (?, ?, ?, ?)
        "#,
    )
    .bind(&lobby_id)
    .bind(&name)
    .bind(&user_id)
    .bind(max_players)
    .execute(&state.db)
    .await;

    if let Err(error) = create_result {
        eprintln!("Failed to create lobby: {error}");
        return Json(json!({
            "success": false,
            "error": "lobby_create_failed"
        }));
    }

    let player_result = sqlx::query(
        r#"
        INSERT INTO lobby_players (lobby_id, user_id, ready)
        VALUES (?, ?, 0)
        "#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .execute(&state.db)
    .await;

    if let Err(error) = player_result {
        eprintln!("Failed to add lobby host: {error}");

        let _ = sqlx::query("DELETE FROM lobbies WHERE lobby_id = ?")
            .bind(&lobby_id)
            .execute(&state.db)
            .await;

        return Json(json!({
            "success": false,
            "error": "lobby_host_join_failed"
        }));
    }

    println!("Lobby created: {} host={}", lobby_id, user_id);

    Json(json!({
        "success": true,
        "lobbyId": lobby_id,
        "name": name,
        "hostUserId": user_id,
        "maxPlayers": max_players,
        "players": 1,
        "status": "waiting"
    }))
}

async fn list_lobbies_handler(State(state): State<AppState>) -> Json<Value> {
    let lobbies = sqlx::query_as::<_, (String, String, String, i32)>(
        r#"
        SELECT lobby_id, name, host_user_id, max_players
        FROM lobbies
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let mut result = Vec::new();

    for (lobby_id, name, host_user_id, max_players) in lobbies {
        let players: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM lobby_players WHERE lobby_id = ?")
                .bind(&lobby_id)
                .fetch_one(&state.db)
                .await
                .unwrap_or(0);

        result.push(json!({
            "lobbyId": lobby_id,
            "name": name,
            "hostUserId": host_user_id,
            "maxPlayers": max_players,
            "players": players,
            "status": if players >= max_players as i64 { "full" } else { "waiting" }
        }));
    }

    Json(json!({
        "success": true,
        "lobbies": result
    }))
}

async fn get_lobby_handler(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
) -> Json<Value> {
    let lobby = sqlx::query_as::<_, (String, String, String, i32)>(
        r#"
        SELECT lobby_id, name, host_user_id, max_players
        FROM lobbies
        WHERE lobby_id = ?
        "#,
    )
    .bind(&lobby_id)
    .fetch_optional(&state.db)
    .await;

    let Some((lobby_id, name, host_user_id, max_players)) = lobby.ok().flatten() else {
        return Json(json!({
            "success": false,
            "error": "lobby_not_found"
        }));
    };

    let players = sqlx::query_as::<_, (String, String, String, i64)>(
        r#"
        SELECT s.user_id, s.display_name, lp.joined_at, lp.ready
        FROM lobby_players lp
        INNER JOIN sessions s ON s.user_id = lp.user_id
        WHERE lp.lobby_id = ?
        ORDER BY lp.joined_at ASC
        "#,
    )
    .bind(&lobby_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let player_list: Vec<Value> = players
        .iter()
        .map(|(user_id, display_name, joined_at, ready)| {
            json!({
                "userId": user_id,
                "displayName": display_name,
                "joinedAt": joined_at,
                "ready": *ready != 0
            })
        })
        .collect();

    let player_count = player_list.len() as i64;

    Json(json!({
        "success": true,
        "lobby": {
            "lobbyId": lobby_id,
            "name": name,
            "hostUserId": host_user_id,
            "maxPlayers": max_players,
            "players": player_count,
            "status": if player_count >= max_players as i64 { "full" } else { "waiting" },
            "members": player_list
        }
    }))
}

async fn join_lobby_handler(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    Json(request): Json<LobbyJoinRequest>,
) -> Json<Value> {
    let user_id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM sessions WHERE token = ?")
            .bind(&request.token)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let Some(user_id) = user_id else {
        return Json(json!({
            "success": false,
            "error": "invalid_session"
        }));
    };

    let lobby = sqlx::query_as::<_, (String, String, i32)>(
        r#"
        SELECT name, host_user_id, max_players
        FROM lobbies
        WHERE lobby_id = ?
        "#,
    )
    .bind(&lobby_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let Some((name, host_user_id, max_players)) = lobby else {
        return Json(json!({
            "success": false,
            "error": "lobby_not_found"
        }));
    };

    let already_joined: bool = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT COUNT(*)
        FROM lobby_players
        WHERE lobby_id = ? AND user_id = ?
        "#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0)
        > 0;

    if already_joined {
        let players: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM lobby_players WHERE lobby_id = ?")
                .bind(&lobby_id)
                .fetch_one(&state.db)
                .await
                .unwrap_or(0);

        return Json(json!({
            "success": true,
            "alreadyJoined": true,
            "lobbyId": lobby_id,
            "name": name,
            "hostUserId": host_user_id,
            "players": players,
            "maxPlayers": max_players,
            "status": if players >= max_players as i64 { "full" } else { "waiting" }
        }));
    }

    let players: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lobby_players WHERE lobby_id = ?")
        .bind(&lobby_id)
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    if players >= max_players as i64 {
        return Json(json!({
            "success": false,
            "error": "lobby_full",
            "players": players,
            "maxPlayers": max_players
        }));
    }

    let result = sqlx::query(
        r#"
        INSERT INTO lobby_players (lobby_id, user_id, ready)
        VALUES (?, ?, 0)
        "#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .execute(&state.db)
    .await;

    if let Err(error) = result {
        eprintln!("Failed to join lobby: {error}");
        return Json(json!({
            "success": false,
            "error": "join_failed"
        }));
    }

    let new_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM lobby_players WHERE lobby_id = ?")
            .bind(&lobby_id)
            .fetch_one(&state.db)
            .await
            .unwrap_or(players + 1);

    println!("Lobby join: lobby={} user={}", lobby_id, user_id);

    Json(json!({
        "success": true,
        "alreadyJoined": false,
        "lobbyId": lobby_id,
        "name": name,
        "hostUserId": host_user_id,
        "players": new_count,
        "maxPlayers": max_players,
        "status": if new_count >= max_players as i64 { "full" } else { "waiting" }
    }))
}

async fn leave_lobby_handler(
    State(state): State<AppState>,
    Path(lobby_id): Path<String>,
    Json(request): Json<LobbyJoinRequest>,
) -> Json<Value> {
    let user_id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM sessions WHERE token = ?")
            .bind(&request.token)
            .fetch_optional(&state.db)
            .await
            .unwrap_or(None);

    let Some(user_id) = user_id else {
        return Json(json!({
            "success": false,
            "error": "invalid_session"
        }));
    };

    let result = sqlx::query(
        r#"
        DELETE FROM lobby_players
        WHERE lobby_id = ? AND user_id = ?
        "#,
    )
    .bind(&lobby_id)
    .bind(&user_id)
    .execute(&state.db)
    .await;

    match result {
        Ok(result) => {
            let remaining: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM lobby_players WHERE lobby_id = ?")
                    .bind(&lobby_id)
                    .fetch_one(&state.db)
                    .await
                    .unwrap_or(0);

            if remaining == 0 {
                let _ = sqlx::query("DELETE FROM lobbies WHERE lobby_id = ?")
                    .bind(&lobby_id)
                    .execute(&state.db)
                    .await;
            }

            Json(json!({
                "success": true,
                "removed": result.rows_affected() > 0,
                "lobbyId": lobby_id,
                "players": remaining
            }))
        }

        Err(error) => {
            eprintln!("Failed to leave lobby: {error}");
            Json(json!({
                "success": false,
                "error": "leave_failed"
            }))
        }
    }
}

async fn db_check_handler(State(state): State<AppState>) -> Json<Value> {
    let connected = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();

    Json(json!({
        "status": if connected { "online" } else { "degraded" },
        "database": if connected { "connected" } else { "disconnected" },
        "healthy": connected
    }))
}

async fn server_info_handler(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "name": "KLAY Friday the 13th Private Server",
        "status": "online",
        "backend": "rust-axum",
        "database": "sqlite",
        "public_url": state.public_url,
        "version": SERVER_VERSION,
        "features": [
            "sessions",
            "heartbeat",
            "session_validation",
            "lobbies",
            "lobby_details",
            "lobby_join",
            "lobby_leave",
            "protocol",
            "custom_tcp_protocol",
            "ready_state",
            "session_start",
            "offline_match_tracking",
            "klay_achievements"
        ]
    }))
}


#[cfg(test)]
mod lobby_capacity_tests {
    use super::*;

    #[tokio::test]
    async fn sqlite_trigger_rejects_joining_a_full_lobby() {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();

        sqlx::query(
            "CREATE TABLE lobbies (lobby_id TEXT PRIMARY KEY, max_players INTEGER NOT NULL)",
        )
        .execute(&db)
        .await
        .unwrap();

        sqlx::query(
            "CREATE TABLE lobby_players (lobby_id TEXT NOT NULL, user_id TEXT NOT NULL, PRIMARY KEY (lobby_id, user_id))",
        )
        .execute(&db)
        .await
        .unwrap();

        sqlx::query(
            r#"
            CREATE TRIGGER enforce_lobby_capacity
            BEFORE INSERT ON lobby_players
            WHEN NOT EXISTS (
                SELECT 1 FROM lobby_players
                WHERE lobby_id = NEW.lobby_id AND user_id = NEW.user_id
            )
            AND (
                SELECT COUNT(*) FROM lobby_players WHERE lobby_id = NEW.lobby_id
            ) >= COALESCE(
                (SELECT max_players FROM lobbies WHERE lobby_id = NEW.lobby_id),
                0
            )
            BEGIN
                SELECT RAISE(ABORT, 'lobby_full');
            END
            "#,
        )
        .execute(&db)
        .await
        .unwrap();

        sqlx::query("INSERT INTO lobbies (lobby_id, max_players) VALUES ('lobby-a', 1)")
            .execute(&db)
            .await
            .unwrap();

        sqlx::query("INSERT INTO lobby_players (lobby_id, user_id) VALUES ('lobby-a', 'player-1')")
            .execute(&db)
            .await
            .unwrap();

        let rejected = sqlx::query(
            "INSERT INTO lobby_players (lobby_id, user_id) VALUES ('lobby-a', 'player-2')",
        )
        .execute(&db)
        .await;

        assert!(rejected.is_err(), "a full lobby must reject additional players");
    }
}
