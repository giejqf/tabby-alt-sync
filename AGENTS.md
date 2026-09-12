# AGENTS.md — tabby-alt-sync

## What this project is

`tabby-alt-sync` is a minimal, single-user **config sync host** for [Tabby](https://github.com/Eugeny/tabby), written in Rust.

Tabby's *Settings → Config sync* feature expects a [tabby-web](https://github.com/Eugeny/tabby-web) instance (Django + DRF) at the configured "Sync host", authenticated with a "Secret sync token". tabby-web also ships a browser terminal, OAuth logins, a connection gateway, an app-distribution service and a full UI. We want **none of that** — only the sync API, for one user, with one static token.

Unmodified Tabby clients must work against this server with nothing but a host URL and a token.

In ./tabby you have a copy of tabby's source code FOR REFERENCE. Don't compile it or commit it.

### Scope

In scope:
- `/api/1/configs` CRUD and `/api/1/user`, byte-compatible with tabby-web's DRF output
- One static bearer token, one implicit user
- SQLite persistence
- Optional built-in TLS

Explicitly out of scope (do not add):
- Any UI, HTML, or static file serving
- OAuth / social auth / sessions / cookies / CSRF
- Multiple users, accounts, registration, per-device tokens
- The connection gateway (`/api/1/gateways/choose`), app versions (`/api/1/versions`), demo/terminal views
- Parsing, validating, or transforming the synced config content

## The golden rule

**Wire compatibility beats internal elegance.** The client is a fixed, already-shipped binary. If a change makes our JSON body, status code, header, or datetime format differ from what DRF emits, that is a bug — even when the new behaviour is "more correct".

The contract below is derived from real upstream source. When changing anything in it, cite the upstream file you checked in the commit message. Do not relax a compatibility test to make code pass.

Upstream sources of truth:

| What | Where |
| --- | --- |
| Routes | `Eugeny/tabby-web` → `backend/tabby/app/api/__init__.py` |
| Config serializer/viewset | `backend/tabby/app/api/config.py` |
| User serializer/viewset | `backend/tabby/app/api/user.py` |
| Models & defaults | `backend/tabby/app/models.py` |
| Token auth | `backend/tabby/middleware.py` (`TokenMiddleware`) |
| DRF settings (no pagination!) | `backend/tabby/settings.py` |
| **The client** | `Eugeny/tabby` → `tabby-settings/src/services/configSync.service.ts` |

## Stack and conventions

- Rust stable, 2021 edition. MSRV: whatever current stable is; don't add MSRV gymnastics.
- `axum` + `tokio` for HTTP, `tower-http` for tracing/CORS/body limits.
- `rusqlite` with the `bundled` feature — no system SQLite, trivially cross-compiles, and the workload is one user doing a few writes a minute. A single `Arc<Mutex<Connection>>` is fine; do not introduce an async ORM.
- `serde` / `serde_json` for representations, `time` or `chrono` for timestamps, `subtle` for constant-time comparison, `rustls` + `axum-server` for optional TLS, `clap` for CLI, `tracing` for logs.
- `thiserror` for error types. No `anyhow` in library code; `anyhow` is acceptable in `main.rs` startup only.
- **No `unwrap`/`expect`/`panic!` in request handlers.** Startup and tests may use them.
- `cargo fmt` and `cargo clippy -- -D warnings` must be clean.

## Commands

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check

# run locally (plain HTTP — only older clients accept it, see TLS below)
TABBY_ALT_SYNC_TOKEN=$(cargo run -q -- gen-token) cargo run

# tests set their own token; never read the operator's real one
TABBY_ALT_SYNC_TOKEN=test-token cargo test
```

## API contract

Base path is `/api/1`. The client strips a trailing `/` from the configured host before appending paths, so the server is always addressed at the URL root.

### Authentication

Every `/api/1/*` request must present the token, in either form (upstream's `TokenMiddleware` accepts both):

1. `Authorization: Bearer <token>` — what the Tabby client sends.
2. `?auth_token=<token>` query parameter — keep it for parity with tabby-web.

Rules:
- Compare in constant time. Hash both sides (SHA-256) and compare the digests with `subtle::ConstantTimeEq`, so neither the value nor its **length** leaks through timing.
- Missing or wrong token → `401` with `WWW-Authenticate: Bearer` and body `{"detail": "Invalid token."}`.
  - Upstream actually returns `403 {"detail": "Authentication credentials were not provided."}` because DRF's `SessionAuthentication` supplies no auth header. The client only inspects `response.ok`, so this difference is unobservable. We deliberately return the semantically correct `401`. **Do not keep flip-flopping on this** — it is a settled decision.
- The scheme comparison (`Bearer`) is case-sensitive upstream. Accept case-insensitive; it can only help.

### Endpoints

| Method | Path | Called by | Request body | Success |
| --- | --- | --- | --- | --- |
| `GET` | `/api/1/user` | client's *Test connection* | — | `200` user object |
| `PUT` | `/api/1/user` | tabby-web UI only; parity | partial user | `200` user object |
| `GET` | `/api/1/configs` | client config list | — | `200` **bare JSON array** |
| `POST` | `/api/1/configs` | *Upload as new config* | `{"name": "..."}` | `201` config object |
| `GET` | `/api/1/configs/{id}` | download + 60 s autosync poll | — | `200` config object |
| `PUT` | `/api/1/configs/{id}` | parity | config fields | `200` config object |
| `PATCH` | `/api/1/configs/{id}` | every upload | `{"content": "...", "last_used_with_version": "..."}` | `200` config object |
| `DELETE` | `/api/1/configs/{id}` | *Delete remote config* | — | `204`, empty body |

Everything else under `/api/1/` → `404 {"detail": "Not found."}`. That includes `/api/1/versions`, `/api/1/gateways/choose`, `/api/1/auth/*`. Do not stub them "just in case"; the desktop client never calls them.

Accept both `/api/1/configs` and `/api/1/configs/` as the same route. The client never sends the trailing slash — just don't 404 or redirect if something else does.

### Config object

Exact shape. All keys always present, including nulls. Never add, rename, omit, or camelCase a key.

```json
{
  "id": 3,
  "user": 1,
  "name": "New config on darwin",
  "content": "version: 4\nprofiles: []\n",
  "last_used_with_version": "1.0.235",
  "created_at": "2026-09-12T09:41:07.123456Z",
  "modified_at": "2026-09-12T09:58:22.654321Z"
}
```

- `id` — integer. The client stores it in its own config file as `configSync.configID` and compares with `===`. Never a string, never a UUID.
- `user` — always `1`. Vestigial, but upstream's serializer uses `fields = "__all__"`, so it is there.
- `name` — string, shown in the client's config list.
- `content` — **opaque string**, the client's YAML config. Store and return it byte-for-byte. Never parse it, reformat it, validate it, or re-serialize it. It routinely contains credentials (vault, SSH passwords).
- `last_used_with_version` — nullable string. The client writes its app version on every upload.
- `created_at` / `modified_at` — DRF ISO-8601 in UTC: `%Y-%m-%dT%H:%M:%S.%6fZ`. Six fractional digits, literal `Z`, never `+00:00`.

### User object

```json
{
  "id": 1,
  "username": "tabby",
  "active_config": null,
  "custom_connection_gateway": null,
  "custom_connection_gateway_token": null,
  "config_sync_token": "<the configured token>",
  "is_pro": true,
  "is_sponsor": false,
  "github_username": null
}
```

- `is_pro` must be `true` (upstream gates features on it). `is_sponsor` is `false`.
- `config_sync_token` echoes the token back, exactly as upstream does. This is not a leak: the caller had to present that token to get here. Do not "harden" it by redacting.
- `active_config` is a nullable config id, persisted; settable via `PUT /api/1/user`. The desktop client never reads or writes it, but keep it working.
- `PUT /api/1/user` ignores `id` and `username` (read-only upstream) and the derived fields; it may set `active_config`, `custom_connection_gateway`, `custom_connection_gateway_token`.

### Write semantics (these mirror Django model/serializer behaviour)

1. **POST** may carry only `{"name": "..."}`. Defaults: `content` = `"{}"`, `last_used_with_version` = `null`. If `name` is absent or empty, generate `Unnamed config (YYYY-MM-DD)` from the server's local date.
2. **PATCH** is a partial update: keys absent from the body are untouched. An explicit `null` for `last_used_with_version` sets null.
3. **PUT** behaves the same as PATCH here. In DRF every writable field on this serializer is `required=False`, and a non-partial update only assigns what is in `validated_data` — so absent fields are left unchanged rather than reset. Implement PUT as an alias of PATCH.
4. Read-only fields (`id`, `user`, `created_at`, `modified_at`) in a request body are **silently ignored**, not rejected. DRF drops them.
5. Unknown fields in a request body are **silently ignored**.
6. `modified_at` changes on every successful write; `created_at` never changes after creation.
7. **DELETE** returns `204` with a completely empty body. The client does `text ? JSON.parse(text) : undefined`, so a JSON body here is wrong even though it wouldn't crash.
8. Unknown id → `404 {"detail": "Not found."}`.
9. Wrong method on an existing route → `405 {"detail": "Method \"DELETE\" not allowed."}`.
10. Malformed JSON → `400 {"detail": "JSON parse error - ..."}`. Field-level validation errors use DRF's shape: an object mapping field name to an **array** of message strings, e.g. `{"name": ["Not a valid string."]}`.
11. Always respond with `Content-Type: application/json` (except the empty 204). Be lenient about the request's `Content-Type`: older clients use axios, current ones use `fetch` and only set the header when there's a body.

### Concurrency

Last write wins, same as upstream. The client sends no `If-Match`/`ETag` and would ignore a `409`. Do read-modify-write inside a single SQLite transaction so `content` and `modified_at` move together.

The client polls `GET /api/1/configs/{id}` every 60 s and downloads when `new Date(modified_at) > lastRemoteChange`. JS `Date` resolves to milliseconds, so two writes inside the same millisecond can look equal and delay a pull by one poll cycle. Acceptable — do not invent a version counter to work around it.

### CORS

Some client builds issue these requests from an Electron renderer where CORS applies. Answer `OPTIONS` for all `/api/1/*`:

```
Access-Control-Allow-Origin: <echo request Origin, or * when absent>
Access-Control-Allow-Methods: GET, POST, PUT, PATCH, DELETE, OPTIONS
Access-Control-Allow-Headers: authorization, content-type
Access-Control-Max-Age: 86400
```

Do **not** send `Access-Control-Allow-Credentials`. Auth is a bearer token; no cookies are involved.

## TLS and deployment

Current Tabby versions refuse to sync over plaintext — `configSync.service.ts` throws before sending if the host doesn't match `^https://`, because the response is YAML that gets merged into the local config, including profile `command`/`env` that the terminal later executes. Consequences:

- Production: reverse proxy with TLS, or run with built-in rustls via `--tls-cert` / `--tls-key`.
- Developing against a *real* client over `http://localhost` will not work on recent versions. Use a locally trusted cert (mkcert) for manual testing; use the integration tests for everything else.
- The server never constructs absolute URLs (DRF here serializes relations as integer PKs, not hyperlinks), so there is **no** public-base-URL setting and no `X-Forwarded-Proto` handling to get right.

## Configuration

Env vars, all prefixed `TABBY_ALT_SYNC_`; matching CLI flags override them.

| Variable | Default | Notes |
| --- | --- | --- |
| `TOKEN` | — | Required. Refuse to start if empty or shorter than 16 chars. |
| `TOKEN_FILE` | — | Alternative to `TOKEN`; read and trim trailing newline. Exactly one of the two. |
| `BIND` | `127.0.0.1:9600` | Loopback by default. |
| `DB` | `./tabby-alt-sync.db` | Created on first run. |
| `USERNAME` | `tabby` | Cosmetic; appears in the user object. |
| `MAX_BODY_BYTES` | `8388608` | Configs with many profiles get large; axum's 2 MiB default is too small. |
| `TLS_CERT` / `TLS_KEY` | — | Both or neither. |
| `LOG` | `info` | `tracing_subscriber` env filter syntax. |

Subcommand `gen-token` prints 64 random bytes as hex (128 chars — matches upstream's `secrets.token_hex(64)`) and exits. Never auto-generate a token at startup: a token printed once into a log the operator didn't keep is worse than a clear error.

## Storage

```sql
CREATE TABLE configs (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    name                   TEXT NOT NULL,
    content                TEXT NOT NULL DEFAULT '{}',
    last_used_with_version TEXT,
    created_at             TEXT NOT NULL,   -- stored in wire format
    modified_at            TEXT NOT NULL    -- stored in wire format
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);  -- schema_version, active_config
```

- `AUTOINCREMENT` is deliberate: ids must never be reused after a delete. A client still holding a stale `configID` would otherwise silently start overwriting an unrelated config.
- Store timestamps as TEXT already in wire format so reads are byte-identical to what was written.
- `PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;` on every connection.
- Migrations are embedded, forward-only, applied at startup, tracked in `meta.schema_version`.

## Layout

```
src/
  main.rs          # CLI, settings, tracing, bootstrap (plain or TLS)
  config.rs        # Settings struct + validation
  auth.rs          # token extraction, constant-time check, 401 response
  time.rs          # DRF datetime formatting/parsing — the ONLY place that formats timestamps
  db/mod.rs        # connection, pragmas, migrations
  db/configs.rs    # CRUD
  api/mod.rs       # router assembly, CORS, body limit
  api/error.rs     # ApiError -> DRF-shaped JSON responses
  api/configs.rs
  api/user.rs
tests/
  compat.rs        # the contract above, endpoint by endpoint
  lifecycle.rs     # full client flow
```

## Testing

Tests are the contract. Add to them before changing behaviour.

Required coverage:
- **Lifecycle test** replaying exactly what the client does: `GET /user` → `GET /configs` → `POST {name}` → `PATCH {content, last_used_with_version}` → `GET /configs/{id}` (assert `modified_at` advanced and `created_at` did not) → `DELETE` → `GET` returns 404.
- **Auth matrix** per endpoint: no header, wrong token, wrong scheme, token in `?auth_token=`, token with trailing whitespace.
- **Shape tests** asserting the exact key set of both objects — fail on extra keys as well as missing ones.
- **Timestamp format**: match `^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6}Z$`.
- **Content round-trip**: CRLF, non-ASCII, emoji, embedded `\u0000`-free control chars, an empty string, and a 1 MiB payload all return byte-identical.
- **List is a bare array**, not `{"count":…,"results":[…]}`. This is the single easiest thing to get wrong; upstream sets no DRF pagination class.
- **Read-only and unknown fields** in a PATCH body are ignored rather than erroring.
- Each test gets its own temp SQLite file or `:memory:`; no shared global state, tests run in parallel.

If you cannot verify a behaviour against upstream source, write the test `#[ignore]`d with a comment explaining what needs checking. Do not guess and encode the guess as contract.

## Security

- **Never log**: the token, the `Authorization` header, or `content`. Redact at the tracing layer, not at each call site. Log method, path, status, latency, and config id only.
- Constant-time token comparison over hashed values (see Authentication).
- Default bind is loopback. Document that exposing it publicly requires TLS.
- No panics in handlers — a panic is a 500 and, with a shared `Mutex`, a poisoned lock for the rest of the process's life.
- Optional, off by default: a small delay/backoff after repeated auth failures from one peer.

## Things that look like improvements but are not

- Paginating `GET /api/1/configs` — the client indexes the response as an array.
- Emitting timestamps without microseconds, or with `+00:00` instead of `Z`.
- Returning a body on `DELETE`.
- UUID or string config ids.
- camelCase JSON keys.
- Parsing, validating, pretty-printing, or diffing YAML `content`.
- Rejecting unknown request fields with a 400.
- Multi-user support, a login page, an admin UI, per-device tokens.
- Redacting `config_sync_token` from `GET /api/1/user`.

## Manual verification

```bash
TOKEN=... HOST=https://sync.example.com

curl -sS -H "Authorization: Bearer $TOKEN" $HOST/api/1/user | jq
curl -sS -H "Authorization: Bearer $TOKEN" $HOST/api/1/configs | jq
curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"name":"from curl"}' $HOST/api/1/configs | jq
curl -sS -X PATCH -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"content":"version: 4\n","last_used_with_version":"1.0.235"}' \
     $HOST/api/1/configs/1 | jq
curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE -H "Authorization: Bearer $TOKEN" \
     $HOST/api/1/configs/1   # expect 204
```

End-to-end with a real client: Tabby → Settings → Config sync → set *Sync host* to the HTTPS URL and *Secret sync token* to the token. A green check means `GET /api/1/user` succeeded. Then *Upload as new config*, edit a profile, confirm the remote `content` changes, and confirm a second client pulls it within 60 s.

## Commits and PRs

- Conventional commits (`feat:`, `fix:`, `test:`, `docs:`, `chore:`).
- One behavioural change per PR. `cargo test`, `clippy -D warnings`, and `fmt --check` must pass.
- Any change to the wire contract must (a) cite the upstream file that justifies it and (b) update both this document and `tests/compat.rs` in the same commit.
