# KLAY Friday the 13th Backend

Rust/Axum + SQLite service containing a KLAY-local account API and a custom TCP lobby implementation.

## Compatibility status

This repository is **not yet a production-compatible backend for the original Friday the 13th: The Game clients**.

The custom TCP framing uses a KLAY-specific header (magic `KLAY`, version 1, big-endian message/request/payload fields) and JSON messages. There is no verified evidence here that the original PS4 or PS5 game uses this framing, message identifiers, or JSON payloads.

Not implemented or verified:
- PlayStation Network authentication or Sony-issued identity verification.
- Original-game matchmaking, game sessions, or game-server transport.
- The original game's complete API/protocol.
- DNS redirection or PlayStation client configuration.
- Official PSN trophy unlocking or platform entitlement changes.

The `/api/v1/auth/psn` and `/api/v1/protocol` routes return HTTP 501 instead of pretending to authenticate PSN users or returning fabricated protocol responses. `/api/v1/login` creates a KLAY-local account only. TCP authentication requires the matching KLAY account token issued by that endpoint.

## Implemented components

- HTTP health and database checks.
- KLAY-local account creation, session validation, heartbeat, and profile lookup.
- SQLite-backed lobby membership and ready-state operations in the custom TCP service.
- Custom offline-match records and KLAY-local achievement records. These are not official platform trophies.
- TCP frame codec regression tests.

These features operate within this custom protocol; they do not establish compatibility with the original game.

## Environment

- `HOST` (default `0.0.0.0`)
- `PORT` (default `8080`)
- `DATABASE_URL` (SQLite URL; use a persistent mounted volume)
- `TCP_HOST` (default `0.0.0.0`)
- `TCP_PORT` (default `9000`)
- `PUBLIC_URL` (public HTTPS URL)

Do not use an ephemeral filesystem path for the production SQLite database. Configure a persistent volume and point `DATABASE_URL` at its mounted path. Back up the database before deployments or migrations.

## Local checks

```bash
cargo fmt --all -- --check
cargo check --all-targets
cargo test --all-targets
```

The HTTP service and custom TCP listener run as separate listeners in the same process. Hosting providers must support inbound TCP connections and expose the configured TCP port; an HTTPS web domain alone is insufficient.

## API overview

- `GET /`
- `GET /health`
- `GET /api/v1/database/status`
- `GET /api/v1/database_check`
- `POST /api/v1/login` — KLAY-local account only
- `POST /api/v1/session/heartbeat`
- `POST /api/v1/session/validate`
- `GET /api/v1/profiles/me`
- `GET /api/v1/lobbies`
- `POST /api/v1/lobbies/create`
- `GET /api/v1/lobbies/:lobby_id`
- `POST /api/v1/lobbies/:lobby_id/join`
- `POST /api/v1/lobbies/:lobby_id/leave`

A successful build, health check, or TCP socket connection proves only that the service is running. It does not prove that a PS4/PS5 game client can authenticate or join a match. Do not advertise console compatibility until protocol behavior is established from legitimate client/server traces and tested on target game versions.

## License

MIT
