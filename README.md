# Seal

A fully end-to-end encrypted chat application with web and desktop clients, built with a Rust (axum) server, LanceDB storage, libsodium, and Tauri.

## Quick Start

```bash
# Build the Rust server
make dev

# Start the server (http://localhost:8000)
make server

# Run all 75 native Rust integration tests
make test
```

Or without Make:

```bash
cargo run --release
```

The server is a single static binary. `templates/`, `static/`, and `config.yaml`
are bundled into the binary at build time (via `include_dir!` / `include_str!`),
so the release binary at `target/release/seal-server` is self-contained — copy
it anywhere, set `JWT_SECRET` and `DATABASE_PATH`, and run.

## Pre-built Binaries

Every push to `main` triggers a GitHub Actions workflow that cross-compiles the
server for two platforms and publishes the artifacts as a **pre-release** on
the [GitHub Releases page](../../releases):

| Platform | Triple | Asset |
|----------|--------|-------|
| Linux x86_64 | `x86_64-unknown-linux-gnu` | `seal-server-linux-x86_64.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `seal-server-macos-aarch64.tar.gz` |

Each release is tagged `build-YYYYMMDD-<short-sha>` and marked as a pre-release
(no stable releases are cut yet). To run a pre-built binary:

```bash
# Download the asset for your platform, then:
tar -xzf seal-server-<platform>.tar.gz
export JWT_SECRET="your-strong-secret"
export DATABASE_PATH="./data/chat.lance"
./seal-server
```

The workflow definition lives at [`.github/workflows/release.yml`](.github/workflows/release.yml)
and can also be invoked manually via the **Actions → Build & Pre-release → Run
workflow** button on GitHub.

## Deploy to DigitalOcean

[`scripts/deploy-digitalocean.sh`](scripts/deploy-digitalocean.sh) is an
interactive deploy script that ships the server to [DigitalOcean App
Platform](https://www.digitalocean.com/products/app-platform) using `doctl`.

Prerequisites:

- [`doctl`](https://docs.digitalocean.com/reference/doctl/how-to/install/) installed and authenticated (`doctl auth init`)
- The repo pushed to GitHub (App Platform builds from a GitHub branch)
- A [`Dockerfile`](Dockerfile) at the repo root (shipped with the script —
  multi-stage Rust build, no Rust buildpack exists on App Platform)

Run it:

```bash
./scripts/deploy-digitalocean.sh
```

The script prompts for app name, GitHub repo, branch, region, instance size,
`APP_TITLE`, `DATABASE_PATH`, and `JWT_SECRET` (hidden input, stored as an
encrypted env var). Defaults for branch and repo are auto-filled from your
local git config.

It then:

1. Generates a DigitalOcean App Platform spec
2. Previews it with the JWT secret redacted and asks for confirmation
3. Runs `doctl apps create --wait` for a new app, or `doctl apps update` if an
   app with the chosen name already exists
4. Prints the app ID and live URL

With `deploy_on_push: true` in the generated spec, subsequent pushes to the
deployed branch automatically trigger a new build on App Platform.

**Persistence note:** App Platform containers have an ephemeral filesystem by
default, so the LanceDB directory resets on each deploy. For durable data,
attach a persistent volume via **Apps → \<name\> → Settings → Resources → Add
Storage** in the DigitalOcean console.

## Security Model

- **Client-side encryption**: All messages are encrypted/decrypted in the browser using [libsodium](https://libsodium.gitbook.io/) (X25519 key exchange + XSalsa20-Poly1305 authenticated encryption). The server never sees plaintext.
- **Forward secrecy**: Each message uses a fresh ephemeral key pair. Compromising one message key does not compromise past or future messages.
- **Zero-knowledge server**: The server stores only opaque ciphertext. Private keys never leave the client device.
- **Fan-out encryption**: Channel messages are encrypted separately for each member, so every recipient gets a uniquely encrypted copy.
- **Password-protected key backup**: Keys can be exported with Argon2id password derivation + XSalsa20-Poly1305 symmetric encryption.
- **Rate limiting**: Auth endpoints are rate-limited (20 requests/minute per IP) to prevent brute-force attacks.
- **Input validation**: All user inputs are validated against strict regex patterns to prevent injection into LanceDB queries.

## How It Works

1. **Register** — The client generates an X25519 key pair. The public key is uploaded to the server; the private key stays in the browser's IndexedDB.
2. **Login** — Authenticates with username/password (bcrypt-hashed, JWT token returned). The client loads the private key from IndexedDB.
3. **Direct Messages** — Select a user via the search box. Messages are encrypted with the recipient's public key using an ephemeral key pair. A self-encrypted copy is also stored so the sender can read their own message history.
4. **Channels** — Create channels and invite members. Each message is encrypted separately for every member (fan-out). Any user can browse and join public channels.
5. **Key Backup** — Export keys (password-protected with Argon2id) to a JSON file. Import on another device to restore access. Downloaded to your browser's default Downloads folder.
6. **Real-time** — WebSocket connection delivers messages instantly. REST polling (every 5 seconds) provides fallback for missed messages.

## Architecture

```
Browser (libsodium)           Server (Rust / axum)          Storage (LanceDB)
┌──────────────────┐    ┌─────────────────────┐    ┌──────────────────┐
│  Generate keys   │    │  JWT auth + bcrypt   │    │  users           │
│  Encrypt/decrypt │◄──►│  WebSocket relay     │◄──►│  messages        │
│  IndexedDB keys  │    │  REST API            │    │  channels        │
│  Argon2id export │    │  Rate limiting       │    │  channel_members │
└──────────────────┘    └─────────────────────┘    └──────────────────┘
```

The LanceDB dataset (all tables plus encrypted attachments) can live on the
local filesystem *or* on object storage — see [Object Storage](#object-storage).
`GET /health` (liveness) and `GET /readyz` (readiness; pings the database) are
exposed for orchestrators and load balancers.

### Encryption Flow (DM)

```
Sender                          Server                      Recipient
  │                               │                            │
  ├─ Generate ephemeral keypair   │                            │
  ├─ crypto_box(msg, recip_pub)  ─┤                            │
  ├─ crypto_box(msg, self_pub)   ─┤ Store both copies          │
  │                               ├─ Relay to recipient ──────►│
  │                               │                    decrypt ┤
  │                               │          crypto_box_open() ┤
```

### Encryption Flow (Channel)

```
Sender                          Server                     Members
  │                               │                          │
  ├─ For each member:             │                          │
  │   crypto_box(msg, member_pub) │                          │
  ├─ Send N envelopes ───────────►│                          │
  │                               ├─ Store N rows            │
  │                               ├─ Relay via WebSocket ───►│
  │                               │                  decrypt ┤
```

## API Reference

### Authentication

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/register` | Create account (username, password, public_key_jwk) |
| POST | `/api/login` | Sign in (returns JWT token) |

### Users

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/users?token=` | List DM contacts (users you've messaged) |
| GET | `/api/users/search?q=&token=` | Search users by prefix |
| GET | `/api/users/{username}/public_key?token=` | Get a user's public key |

### Channels

| Method | Endpoint | Description |
|--------|----------|-------------|
| POST | `/api/channels?token=` | Create a channel |
| GET | `/api/channels?token=` | List my channels |
| GET | `/api/channels/browse?token=` | Browse joinable channels |
| POST | `/api/channels/{id}/join?token=` | Join a channel |
| GET | `/api/channels/{id}?token=` | Get channel info |
| POST | `/api/channels/{id}/members?token=` | Invite a member |
| GET | `/api/channels/{id}/members/public_keys?token=` | Get member public keys |

### Messages

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/messages/{peer}?token=&after=` | Get DM history with a peer |
| GET | `/api/channels/{id}/messages?token=&after=` | Get channel message history |
| POST | `/api/channels/{id}/messages?token=` | Send channel message (REST) |

### WebSocket

Connect to `ws://host/ws/chat?token=` for real-time messaging. Send JSON:

```json
// DM
{"type": "dm", "recipient": "bob", "ciphertext": "...", "iv": "...", "sender_public_key_jwk": "...",
 "self_ciphertext": "...", "self_iv": "...", "self_sender_public_key_jwk": "..."}

// Channel
{"type": "channel", "channel_id": "uuid", "envelopes": [
  {"target_user": "alice", "ciphertext": "...", "iv": "...", "sender_public_key_jwk": "..."},
  {"target_user": "bob", "ciphertext": "...", "iv": "...", "sender_public_key_jwk": "..."}
]}
```

## Configuration

Configuration is loaded from `config.yaml` with environment variable overrides. Create a `.env` file from the example:

```bash
cp .env.example .env
# Edit .env and set a strong JWT_SECRET
```

| Variable | Default | Description |
|----------|---------|-------------|
| `JWT_SECRET` | `change-me` | Secret key for JWT signing (required for production) |
| `APP_TITLE` | `Seal` | Application title |
| `APP_HOST` | `0.0.0.0` | Server bind address |
| `APP_PORT` | `8000` | Server port |
| `DATABASE_PATH` | `data/chat.lance` | LanceDB path — a local path or an object-store URI (see below) |
| `AUTH_JWT_ALGORITHM` | `HS256` | JWT signing algorithm |
| `AUTH_TOKEN_EXPIRE_MINUTES` | `1440` | Token expiry (24 hours) |

### Object Storage

`DATABASE_PATH` can point at object storage instead of a local path, which keeps
the server stateless for containerized deploys. The whole LanceDB dataset —
users, messages, **and** encrypted attachments — lives in the bucket.

| Scheme | Backend |
|--------|---------|
| `s3://`, `s3a://` | AWS S3 / S3-compatible (MinIO, Cloudflare R2, LocalStack) |
| `s3+ddb://` | S3 with DynamoDB commit locking (safe for concurrent writers) |
| `gs://`, `gcs://` | Google Cloud Storage |
| `az://`, `azure://`, `abfs://`, `abfss://` | Azure Blob Storage |

Credentials come from the standard cloud env vars (`AWS_ACCESS_KEY_ID`,
`GOOGLE_APPLICATION_CREDENTIALS`, `AZURE_STORAGE_ACCOUNT_NAME`, …) or from a
`storage:` block in `config.yaml` using the object_store-native key names
(`aws_endpoint`, `aws_region`, `allow_http`, …). See `config.yaml` and
`.env.example` for full examples.

> **Concurrency:** plain `s3://` has no safe concurrent-write story — concurrent
> commits can clobber each other, and Seal writes messages concurrently. For
> production S3 with multiple writers, use `s3+ddb://` with a DynamoDB lock table
> (or keep a single writer). The server logs a warning at startup when it detects
> a plain `s3://` path.

On object storage the connect **retry budget defaults to 30s** so an unreachable
bucket fails fast (lance-io's own default is ~180s); override with
`client_retry_timeout` / `client_max_retries` in the `storage:` block. Attachment
uploads are capped at `attachments.max_image_size_mb` (default 5 MB) and rejected
with `413 Payload Too Large` before anything is written to the bucket.

Building the server with the object-store backends requires `protoc` (the
Protocol Buffers compiler) at build time — `apt-get install protobuf-compiler`
on Debian/Ubuntu, `brew install protobuf` on macOS.

## Testing

```bash
# Run all 75 native Rust integration tests
make test
```

Each test spawns the axum router in-process against a fresh LanceDB temp dir,
using `reqwest` for REST and `tokio-tungstenite` for WebSockets.

Tests cover:
- **Authentication** — Registration, login, token validation, rate limiting, bcrypt cross-compat with PyNaCl/Python-bcrypt hashes
- **Users** — DM contacts, search, public key retrieval
- **Channels** — CRUD, join, invite, browse, member keys, duplicate name prevention
- **Messages** — DM history, channel messages, REST sending, timestamp filtering
- **Attachments** — Image attachment storage and access control
- **WebSocket** — Connection, DM relay, channel relay with fan-out, non-member rejection
- **Health** — `/health` liveness and `/readyz` readiness probes
- **Object storage** — remote-URI detection, fail-fast on an unreachable bucket
- **Schema migration** — Legacy messages-table column auto-upgrade

## Bot Simulation

Generate test data with 100 bots sending encrypted messages across 10 channels:

```bash
make bots
# or
uv run python scripts/bots.py [--base-url http://localhost:8000]
```

The bot script:
- Creates 100 accounts with deterministic X25519 key pairs
- Creates 10 channels (or finds existing ones — names are unique)
- Sends 1-5 random encrypted messages per bot across random channels
- Uses real PyNaCl encryption compatible with the JS client
- Safe to re-run (idempotent accounts/channels, randomized messages each run)

## Desktop App (Tauri)

The desktop client is a native app built with Tauri v2 (Rust + WebView). It connects to the same FastAPI server.

```bash
# Start the server first
make server

# Development mode (hot reload)
cd desktop && cargo tauri dev

# Production build
cd desktop && cargo tauri build
```

### Prerequisites (Linux)

```bash
apt install libgtk-3-dev libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf
```

## Project Structure

```
├── src/
│   ├── main.rs              # tokio entrypoint
│   ├── lib.rs               # axum Router + bundled assets (include_dir!)
│   ├── config.rs            # YAML + .env loader (config.yaml is bundled too)
│   ├── auth.rs              # bcrypt password hashing, HS256 JWT tokens
│   ├── db.rs                # LanceDB schemas + init + legacy column migration
│   ├── db_ops.rs            # Query/insert helpers around LanceDB
│   ├── models.rs            # serde request/response structs
│   ├── validate.rs          # Username/ID/timestamp regex guards
│   ├── rate_limit.rs        # In-memory IP-keyed rate limiter (20 req / 60s)
│   ├── error.rs             # AppError → IntoResponse
│   ├── ws.rs                # WebSocket connection registry
│   └── routes/
│       ├── auth.rs          # /api/register, /api/login
│       ├── users.rs         # /api/users, /api/users/search, /api/users/{u}/public_key
│       ├── channels.rs      # /api/channels/* REST
│       ├── messages.rs      # /api/messages/{peer}, /api/channels/{id}/messages
│       ├── attachments.rs   # /api/attachments/{id}
│       └── ws.rs            # /ws/chat upgrade handler + DM/channel dispatch
├── tests/
│   ├── auth.rs              # Register, login, JWT, bcrypt, rate limit
│   ├── users.rs             # DM contacts, search, public key
│   ├── channels.rs          # Channel REST CRUD + membership
│   ├── messages.rs          # DM/channel history + attachments
│   ├── websocket.rs         # WS connect, DM relay, channel fan-out
│   ├── migration.rs         # Legacy messages-table column migration
│   └── common/mod.rs        # Shared in-process TestServer harness
├── static/                  # JS/CSS/sodium.js — bundled into the binary at build
├── templates/index.html     # Bundled into the binary at build
├── scripts/
│   ├── bots.py              # Bot simulation (Python REST client)
│   └── bundle-sodium.sh     # Rebuild vendored libsodium
├── desktop/                 # Tauri desktop app
├── config.yaml              # Default configuration (also bundled into the binary)
├── .env.example             # Environment variable template
├── Cargo.toml               # Rust dependencies
├── pyproject.toml           # Python deps for scripts/bots.py only
└── Makefile                 # Development commands
```

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Backend | Rust / axum 0.8 / tokio |
| Database | LanceDB 0.30 (embedded columnar, native Rust) |
| Crypto | libsodium (X25519 + XSalsa20-Poly1305) on the client; bcrypt + HS256 JWT on the server |
| Auth | `jsonwebtoken` 10 + `bcrypt` 0.19 (wire-compatible with python-jose / Python bcrypt) |
| Web Frontend | Vanilla HTML/CSS/JS (no build step) |
| Desktop | Tauri v2 (Rust + WebView) |
| Key Backup | Argon2id + XSalsa20-Poly1305 (secretbox) |
| Testing | `cargo test` integration suite (reqwest + tokio-tungstenite, in-process server) |
| Asset bundling | `include_dir!` / `include_str!` — single-binary deploy |

## Make Commands

```
make help             Show all available commands
make dev              Build the Rust server (debug)
make release          Build the Rust server (release)
make server           Run the Rust server (cargo run)
make test             Run all native Rust integration tests
make test-quiet       Run tests with minimal output
make bots             Run bot simulation against a running server
make reset-db         Delete the LanceDB directory
make bundle-sodium    Rebuild the vendored libsodium bundle
make clean            Remove cargo + Python build artifacts
```
