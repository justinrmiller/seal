---
name: seal-best-practices
description: Conventions and best practices for the seal codebase — a Rust/Axum/LanceDB end-to-end-encrypted chat server. Use when writing, reviewing, or extending code in this project (config, database, routes, WebSocket, or tests).
---

# Seal — Best Practices

Seal is an end-to-end-encrypted chat server: **Rust 2021**, **Axum** (tokio async),
with **LanceDB** as the single embedded datastore. It ships as one self-contained
binary — templates, static assets, and `config.yaml` are embedded at compile time.

This file captures project conventions so changes stay consistent with the
existing code. Keep it updated as patterns evolve.

## Architecture at a glance

- `src/main.rs` — binary entrypoint: load config, connect DB, `init_db`, build router, serve.
- `src/lib.rs` — `AppState` (shared `cfg`, `conn`, `rate_limiter`, `ws_connections`) and `build_router`.
- `src/config.rs` — layered configuration (see below).
- `src/db.rs` — Arrow schemas, `connect`, `init_db`, and lightweight column migrations.
- `src/db_ops.rs` — low-level LanceDB helpers (row building, `append`, `scan_where`, column extraction).
- `src/routes/` — HTTP handlers (auth, users, messages, attachments, channels).
- `src/ws.rs` — WebSocket connection registry and message relay.
- `tests/` — integration tests over a real (temp-dir) LanceDB via `TestServer`.

## Configuration

- **Layering (lowest → highest precedence):** embedded `config.yaml` →
  on-disk `config.yaml` at the project root → environment variables.
  Use the `env_or` / `env_or_parse` helpers in `config.rs` for new fields.
- Add new config as: a typed `*Section` struct in `YamlConfig`, a field on
  `Config`, and (if overridable) an env var read in `load()`. Mirror the field
  in **both** `Config::load()` and `Config::for_test()`.
- Prefer `#[serde(default)]` on new YAML sections/fields so older config files
  keep parsing. Document new keys with a commented example in `config.yaml` and,
  for env overrides, in `.env.example`.
- **Path/URI resolution:** relative `database.path` values are anchored to the
  project root; object-store URIs (`s3://`, `gs://`, `az://`, …) are passed to
  LanceDB verbatim — see `is_object_store_uri`. `SEAL_PROJECT_ROOT` overrides the
  project root, otherwise it's the process CWD.
- **Secrets:** `JWT_SECRET` must come from the environment; the `change-me`
  fallback is dev-only and logs a warning. Never commit real secrets.

## Database (LanceDB)

- **All** persistence goes through LanceDB — there is no separate blob store.
  Encrypted attachment bytes live in-table as `LargeUtf8` (`attachments.encrypted_data`).
- Table schemas are defined as `*_schema()` functions in `db.rs`. `init_db` is
  idempotent: it creates missing tables and never drops data.
- **Schema evolution:** add columns via a guarded migration like
  `migrate_messages_table` (check existing field names, then `add_columns` with
  SQL default expressions). Never assume a column exists; older datasets predate it.
- Go through `db_ops` helpers rather than hand-rolling Arrow `RecordBatch`
  plumbing in route handlers. Query with `scan_where` using validated predicates.

## Routes & security

- This is an **E2E-encrypted** app: the server stores/relays ciphertext and IVs.
  Do not add code that would need plaintext message or attachment contents.
- **Authorization:** verify channel membership before returning messages or
  attachments (see the attachment handlers as the reference pattern). Return
  `403` for non-members, `404` for missing resources.
- **Validate all identifiers** against the configured `safe_name_re` / `safe_id_re`
  regexes before using them in queries. Enforce `max_image_size_bytes` on uploads.
- Keep handlers thin: validate → authorize → call `db_ops`/`ws` → map errors to
  status codes. Use the existing `thiserror`/`anyhow` error types.

### HTTP / API conventions (match these in new endpoints and tests)

- **Auth token is a query parameter**, not a header: `?token=<jwt>` on HTTP
  routes and on the WebSocket URL (`/ws/chat?token=...`). `register`/`login`
  return `{ "username", "token" }`.
- **Status codes:** `200` success · `400` validation/domain error (e.g. duplicate
  username, bad identifier) · `401` auth failure (wrong password, unknown user)
  · `403` authenticated-but-forbidden (non-member) · `404` not found · `422`
  missing/malformed JSON fields (Axum's extractor rejection) · `429` rate limited.
- **Error body shape** is `{ "detail": "<message>" }`. Assert on `body["detail"]`
  (often a case-insensitive substring) rather than exact strings.
- **Rate limiting:** the limiter allows 20 requests per window per key; the 21st
  returns `429`. Account for setup calls when counting (see the auth rate-limit tests).

## Testing

Tests are the primary safety net — favor thorough integration coverage. Run the
full suite with `cargo test` (unit tests live in `src/`, integration tests in
`tests/`, doc-tests are compiled too).

### The `TestServer` harness

- `TestServer::spawn()` ([tests/common/mod.rs](tests/common/mod.rs)) starts the
  **real** router via `build_router` on `127.0.0.1:0` (random free port), backed
  by a fresh `tempfile::tempdir()` LanceDB and `Config::for_test(...)`. It returns
  a `base_url`, a `reqwest` client factory (`.client()`), a URL builder
  (`.url(path)`), and exposes `AppState` as `.state` for white-box setup.
- The server shuts down gracefully (oneshot channel) and the tempdir is dropped
  when `TestServer` drops — no cleanup needed, no shared global state, so tests
  run concurrently and in isolation. Each `#[tokio::test]` spawns its own server.

### Conventions

- **Per-file helpers, not a shared kitchen sink.** Each integration file defines
  its own small async helpers (`register`, `create_channel`, `post_json`,
  `insert_dm_row`, `open_ws`/`recv_json`/`send_json`, `now`). Duplication across
  files is accepted and intentional — keep helpers local and readable. Only
  `TestServer` itself lives in `tests/common/mod.rs` (add `mod common;` at the top
  of a new test file to use it).
- **Seed via internals, assert via the API.** For setup, write rows directly with
  `db_ops::open(&server.state.conn, "table")` + a hand-built `RecordBatch` (using
  the real `db::*_schema()`) + `db_ops::append`, bypassing HTTP. Then exercise the
  behavior under test through the public API and assert on the response. See
  `insert_dm_row` in [tests/messages.rs](tests/messages.rs). This keeps setup fast
  and lets you construct states the API wouldn't easily produce.
- **Drive setup calls with `.error_for_status()`** to fail loudly if a
  precondition (register/login) unexpectedly breaks.
- **Cover the full matrix** for every new endpoint: happy path, `400` validation,
  `401` bad/missing token, `403` non-member, `404` not found, and `422` malformed
  body. The existing auth/messages/channels tests are the template.
- **WebSocket tests:** derive the ws URL by `replacen("http://", "ws://", 1)`,
  connect with `tokio_tungstenite::connect_async`, exchange JSON text frames, and
  **always wrap `ws.next()` in a `tokio::time::timeout`** (5s) so a hang fails the
  test instead of blocking forever. See the helpers in
  [tests/websocket.rs](tests/websocket.rs).
- **Schema migrations:** test against a legacy-schema fixture (write the old
  schema, run `init_db`, assert the new columns exist with correct defaults) and
  assert **idempotency** by calling `init_db` twice and checking the field count is
  unchanged — see [tests/migration.rs](tests/migration.rs).
- **Python compatibility:** seal is a port of a FastAPI app and must read data the
  Python server wrote. Preserve this — e.g. verify bcrypt `$2b$` hashes and mirror
  Python-era migrations (`login_uses_bcrypt_python_compatible_hashes` in
  [tests/auth.rs](tests/auth.rs)).

### Unit tests and process-global env

- `Config::for_test` deliberately ignores env vars so concurrent integration tests
  don't race on process-global state — always use it in tests, never `Config::load`
  for the server under test.
- Unit tests that *must* mutate env (`DATABASE_PATH`, `SEAL_PROJECT_ROOT`, etc.)
  acquire the `ENV_LOCK` mutex in [src/config.rs](src/config.rs), snapshot the prior
  value, and **restore it at the end** (`std::env::set_var`/`remove_var`). Use
  `.lock().unwrap_or_else(|e| e.into_inner())` so a poisoned lock from an unrelated
  panic doesn't cascade. `remote_database_path_is_stored_verbatim` is the model.
- Keep pure logic (URI/scheme detection, validation regexes, parsing) unit-tested
  in-module under `#[cfg(test)]`, separate from the HTTP-level integration tests.

## Dependencies

- `arrow-array` is **coupled to LanceDB** — it must track the Arrow major version
  LanceDB re-exports (currently 58). Do not bump it independently of `lancedb`.
- `lancedb` object-store backends are feature-gated (`aws`, `gcs`, `azure`);
  its `default = []`. Enable features explicitly for the backends you need.
- Routine refresh: `cargo update` for in-range bumps; bump manifest majors only
  after checking the changelog. `serde_yml` is an unmaintained shim — flag before
  relying on new behavior.

## Verify before you finish

Any observable change should be verified, not assumed:

```
cargo build            # compiles (watch for new feature-gated deps)
cargo test             # unit + integration suites must pass
```

For object-storage changes, exercise a real S3-compatible round-trip
(LocalStack/MinIO) — see the commands in `.env.example`.
