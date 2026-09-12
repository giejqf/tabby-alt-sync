# tabby-alt-sync

A minimal, single-user **config sync host** for [Tabby](https://github.com/Eugeny/tabby), written in Rust.

Tabby's *Settings → Config sync* feature expects a [tabby-web](https://github.com/Eugeny/tabby-web) instance. tabby-web is a full Django application that also ships a browser terminal, OAuth logins, a connection gateway, an app-distribution service and a UI. This project implements **only** the sync API that the desktop client talks to — one user, one static token, SQLite persistence, optional TLS — and nothing else.

Unmodified Tabby clients work against it with nothing but a host URL and a token.

## What it provides

- `GET/PUT/PATCH /api/1/user` and full CRUD on `/api/1/configs`, byte-compatible with tabby-web's DRF output.
- A single static bearer token and one implicit user.
- Config content stored and returned **byte-for-byte** (it is opaque YAML and may contain credentials).
- SQLite storage with embedded, forward-only migrations.
- Optional built-in TLS via rustls.
- CORS preflight handling for Electron-renderer client builds.

Deliberately **not** included: any UI or static files, OAuth/sessions/cookies, multiple users, the connection gateway, app versions, or config parsing/validation.

## Requirements

- **Rust stable** (2021 edition) to build from source. `rusqlite` is bundled, so no system SQLite is needed.
- For a **real client**, an **HTTPS** endpoint. Current Tabby versions refuse to sync over plaintext `http://` because the downloaded YAML is merged into the local config (including `command`/`env` that the terminal later executes). Use the built-in TLS flags or a TLS-terminating reverse proxy.

## Quick start

Build the binary and generate a token:

```bash
cargo build --release
./target/release/tabby-alt-sync gen-token
```

`gen-token` prints 64 random bytes as hex (128 chars). Start the server with that token:

```bash
export TABBY_ALT_SYNC_TOKEN=<paste the token>
./target/release/tabby-alt-sync
```

The server listens on `127.0.0.1:9600` by default and stores its SQLite database at `./tabby-alt-sync.db`.

Then in Tabby: **Settings → Config sync**, set *Sync host* to your HTTPS URL and *Secret sync token* to the token. A green check means `GET /api/1/user` succeeded. Use *Upload as new config* to create a config, or pick an existing one; clients poll every 60 s and pull remote changes.

> Not sure it is working? A `curl` to `http://localhost:9600/api/1/user` with the bearer token is a quick check. Real clients still need `https://`.

## Running with TLS

Built-in TLS uses PEM certificate and key files; both flags are required together:

```bash
./target/release/tabby-alt-sync \
  --tls-cert /path/to/cert.pem \
  --tls-key  /path/to/key.pem
```

For local testing a self-signed certificate is fine for `curl`, but a real client needs a certificate it trusts. [mkcert](https://github.com/FiloSottile/mkcert) is the easy way to get a locally trusted one. Alternatively, keep the server on loopback and put a reverse proxy (Caddy, nginx, …) in front for TLS.

## Configuration

Every setting has an environment variable prefixed `TABBY_ALT_SYNC_`; a matching CLI flag overrides it.

| Environment variable | CLI flag | Default | Notes |
| --- | --- | --- | --- |
| `TOKEN` | `--token` | — | Required. Exactly one of `TOKEN`/`TOKEN_FILE`. Refuses to start if empty or shorter than 16 chars. |
| `TOKEN_FILE` | `--token-file` | — | Read the token from a file (trailing newline is trimmed). |
| `BIND` | `--bind` | `127.0.0.1:9600` | Address to bind. Loopback by default; expose publicly only behind TLS. |
| `DB` | `--db` | `./tabby-alt-sync.db` | SQLite database file; created on first run. |
| `USERNAME` | `--username` | `tabby` | Cosmetic; reported as `username` by `GET /api/1/user`. |
| `MAX_BODY_BYTES` | `--max-body-bytes` | `8388608` | Request body size limit (configs with many profiles get large). |
| `TLS_CERT` | `--tls-cert` | — | PEM certificate. Requires `TLS_KEY`. |
| `TLS_KEY` | `--tls-key` | — | PEM private key. Requires `TLS_CERT`. |
| `LOG` | `--log` | `info` | `tracing_subscriber` env filter, e.g. `info`, `tabby_alt_sync=debug`. |

There is also a `gen-token` subcommand, which prints a fresh token and exits.

Example using flags instead of the environment:

```bash
./target/release/tabby-alt-sync \
  --token "$(./target/release/tabby-alt-sync gen-token)" \
  --bind 0.0.0.0:9600 \
  --db /var/lib/tabby-alt-sync/sync.db \
  --tls-cert /etc/ssl/tabby/fullchain.pem \
  --tls-key  /etc/ssl/tabby/privkey.pem
```

## API

All routes are under `/api/1` and require the token, either as `Authorization: Bearer <token>` or `?auth_token=<token>`.

| Method(s) | Path | Purpose |
| --- | --- | --- |
| `GET`, `PUT`, `PATCH` | `/api/1/user` | User object; the client uses `GET` for *Test connection*. |
| `GET`, `POST` | `/api/1/configs` | List configs (a bare JSON array) or create one. |
| `GET`, `PUT`, `PATCH`, `DELETE` | `/api/1/configs/{id}` | Fetch, update or delete a config. |

Anything else under `/api/1/` returns `404`. See [AGENTS.md](AGENTS.md) for the exact wire contract, including the config/user JSON shapes and write semantics.

Manual check with `curl`:

```bash
TOKEN=... HOST=https://sync.example.com

curl -sS -H "Authorization: Bearer $TOKEN" "$HOST/api/1/user" | jq
curl -sS -H "Authorization: Bearer $TOKEN" "$HOST/api/1/configs" | jq

curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"name":"from curl"}' "$HOST/api/1/configs" | jq

curl -sS -X PATCH -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
     -d '{"content":"version: 4\n","last_used_with_version":"1.0.235"}' \
     "$HOST/api/1/configs/1" | jq

curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE \
     -H "Authorization: Bearer $TOKEN" "$HOST/api/1/configs/1"   # expect 204
```

## Development

```bash
cargo build --release
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Tests are the contract: `tests/compat.rs` covers the wire format endpoint by endpoint and `tests/lifecycle.rs` replays the client's exact request sequence. Each test uses a private in-memory database, so they run in parallel. Tests set their own token and never read the operator's.

## Security notes

- The token is compared in constant time over SHA-256 digests, so neither its value nor its length leaks through timing.
- The token, the `Authorization` header and config `content` are never logged.
- The server never panics in a request handler.
- The default bind is loopback. If you expose it, terminate TLS in front of it.

## License

[Apache-2.0](LICENSE).
