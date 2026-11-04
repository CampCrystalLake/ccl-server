# KLAY Friday the 13th Backend — Node.js

Node.js 22+, Express, better-sqlite3, and the existing KLAY custom TCP frame protocol.

## Run locally

1. Install Node.js 22 or newer.
2. Run npm install.
3. Run npm start.
4. Open http://localhost:8080.

The server serves the Arabic website from the website directory. It runs HTTP on PORT (default 8080) and the KLAY-specific TCP listener on TCP_PORT (default 9000).

## Environment

HOST=0.0.0.0
PORT=8080
DATABASE_URL=sqlite:///data/f13.db
PUBLIC_URL=https://YOUR-DOMAIN
TCP_HOST=0.0.0.0
TCP_PORT=9000
COOKIE_SECURE=true

Epic OAuth server variables:
EPIC_CLIENT_ID
EPIC_CLIENT_SECRET
EPIC_DEPLOYMENT_ID
EPIC_REDIRECT_URI=https://YOUR-DOMAIN/auth/epic/callback

Register the exact HTTPS callback URL in Epic Developer Portal. Keep EPIC_CLIENT_SECRET only in backend environment variables. Never add it to website JavaScript or commit it.

## Epic OAuth routes

GET /auth/epic/login
GET /auth/epic/callback
GET /auth/epic/me
POST /auth/logout

OAuth uses a cryptographically random state value, a short-lived HttpOnly cookie, a ten-minute database record, one-time state consumption, and a server-side authorization-code exchange. Production requires HTTPS.

## HTTP API

GET / and GET /health
POST /api/v1/login (KLAY-local test account)
POST /api/v1/session/heartbeat
POST /api/v1/session/validate
GET /api/v1/profiles/me
POST /api/v1/auth/psn (501; PSN auth is not implemented)
POST /api/v1/protocol (501; original game protocol is not implemented)
GET /api/v1/database/status and /api/v1/database_check
GET /api/v1/server/info
GET /api/v1/lobbies
POST /api/v1/lobbies/create
GET /api/v1/lobbies/:id
POST /api/v1/lobbies/:id/join
POST /api/v1/lobbies/:id/leave
POST /api/v1/trophies/offline-start
POST /api/v1/trophies/offline-match
POST /api/v1/trophies/progress

The TCP listener retains the KLAY-specific 15-byte header (KLAY magic, version 1, big-endian message ID, request ID, payload length) and JSON message types Authenticate, Heartbeat, CreateSession, JoinSession, LeaveSession, SetReady, and StartSession.

## Compatibility boundary

Epic OAuth signs in an Epic account to this website only. It does not authenticate PlayStation Network, grant PSN entitlements, or establish PS4/PS5 game-client compatibility. The TCP framing implemented here is KLAY-specific and has not been verified as the original Friday the 13th protocol. KLAY achievement records are not official PSN trophies.

## Hosting

Use npm install as the build command and npm start as the start command. Mount persistent storage for the SQLite database. The hosting provider must support inbound TCP if you need the separate TCP listener; exposing only HTTPS does not expose TCP.


## Source layout

The Node.js service in server.js is the active runtime used by npm start. The earlier Rust implementation under src/ is retained as historical/reference source and is not launched by the Node.js start command.
