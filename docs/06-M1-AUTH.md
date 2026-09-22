# M1-AUTH: Authentication Flow

> **Sub-issue**: LUM-1345 (M1-B)
> **Branch**: `feat/multica-rs-m1b-auth` → merge target `feat/multica-rs-initial`
> **Status**: implementation complete; build + full workspace test suite green
> on 2026-09-22 (rustup stable 1.98.1 + PostgreSQL 16); awaiting master
> (LUM-1342) review.

## Scope

Implements the M1 authentication flow corresponding to upstream
`multica/server/internal/handler/auth.go` and `session.go`:

| Method | Path | Handler | Upstream reference |
| --- | --- | --- | --- |
| `POST` | `/auth/send-code` | `send_code` | `Handler.SendCode` (auth.go) |
| `POST` | `/auth/verify-code` | `verify_code` | `Handler.VerifyCode` (auth.go) |
| `POST` | `/auth/logout` | `logout` | `Handler.Logout` (auth.go) |
| `POST` | `/api/auth/refresh` | `refresh_session` | `Handler.RefreshSession` (session.go) |
| `GET` | `/api/me` | `me_placeholder` | real handler in sub-issue A's `routes/workspaces.rs` |

The implementation lives in:

- `crates/mc-http/src/routes/auth.rs` — handlers + router
- `crates/mc-http/src/routes/mount.rs::mount_slice_auth` — anchor that wires it up
  (now returns `auth::router()`, i.e. the slice is `.merge`d for real)
- `crates/mc-repos/src/verification_code.rs` — DB-backed `verification_code` table
- `crates/mc-repos/src/pat.rs` — DB-backed `personal_access_token` table
- `crates/mc-config/src/lib.rs` — `AuthConfig` TTL + rate-limit knobs
- `crates/mc-http/src/state.rs` — `ConfigSnapshot` extensions for auth fields
- `apps/mc-server/src/main.rs` — passes the new fields into `ConfigSnapshot`
- `migrations/0002_pat_revoked_at.up.sql` — adds `personal_access_token.revoked_at`

`mc-auth/src/{session,cookie,verification,pat}.rs` was already populated at M0 and
is reused unchanged (in-memory session store is wired through `mc-auth::SessionStoreContainer`).

## Wire format

### `POST /auth/send-code`

```json
{ "email": "alice@example.com", "purpose": "email_verification" }
```

`purpose` defaults to `email_verification`. Other accepted values:
`password_reset`, `two_factor`, `workspace_invite`.

Response (200):

```json
{ "message": "Verification code sent", "dev_code": "012345" }
```

`dev_code` is **only** returned when `state.config.dev_mode == true`
(`MULTICA_RUN_MODE ∈ {development, test}` or unspecified); production callers
never receive the code in-band. The code itself is stored as
`hex(sha256(code))` in `verification_code.code_hash`, not in plaintext.

### `POST /auth/verify-code`

```json
{ "email": "alice@example.com", "code": "012345" }
```

Response (200), plus `Set-Cookie: multica_session=<sid>; HttpOnly; SameSite=Lax;
Secure` (Secure only outside dev mode) and `X-Multica-Csrf: <csrf>`:

```json
{
  "user": { "id": "<uuid>", "name": "alice", "email": "alice@example.com", "created_at": "<rfc3339>" },
  "session_id": "<sid>",
  "csrf_token": "<csrf>"
}
```

Failure paths:

| Condition | HTTP status |
| --- | --- |
| `code` not 6 digits | 400 (`validation_error`) |
| code expired / already consumed / wrong | **401** (`verification_code_invalid`) |
| email missing | 400 (`validation_error`) |

401 for the three credential-failure paths is an explicit LUM-1345 acceptance
criterion; `mc-errors::http::status_for` maps `VerificationCodeInvalid` to 401
(it was 422 at M0 — changed in this slice). A wrong code additionally
increments `verification_code.attempts` for that email's pending row
(best-effort), mirroring upstream `IncrementVerificationCodeAttempts`
(`auth.go:388` handler, increment at L415); rows with `attempts >= 5` stop
matching `consume`'s SQL and become unusable until TTL expiry.

In dev mode, the env var `MULTICA_DEV_VERIFICATION_CODE` (any 6-digit string)
acts as a universal code — mirrors upstream `isDevVerificationCode`.

### `POST /auth/logout`

No body required. Clears `multica_session` cookie and removes the session from
the in-memory `SessionStoreContainer`. Idempotent — missing cookie still returns
200.

### `POST /api/auth/refresh`

```json
{ "session_id": "<sid>" }
```

Response (200), with refreshed `Set-Cookie` and `X-Multica-Csrf`:

```json
{
  "session_id": "<sid>",
  "csrf_token": "<csrf>",
  "expires_at": "<rfc3339>"
}
```

Sliding renewal: `last_seen_at = now()`, `expires_at = now() + session_ttl_secs`.
The `session_id` is preserved (MUL-7436 binds CSRF to `sid` rather than the
underlying JWT string).

Failure: 401 if session not found / expired.

### `GET /api/me`

Placeholder that returns:

```json
{ "note": "sub-issue A will provide the real /api/me handler in routes/workspaces.rs" }
```

The real `/api/me` handler is owned by sub-issue A (`routes/workspaces.rs`) and
will be mounted in its slice — we deliberately leave a placeholder route here so
clients have a stable surface during M1 development.

## Rate limiting

Two knobs in `AuthConfig` (env-driven):

| Field | Env | Default | Effect |
| --- | --- | --- | --- |
| `send_code_per_email_per_min` | `MULTICA_AUTH_SEND_CODE_PER_EMAIL_PER_MIN` | `5` | max codes per email per 60 s |
| `send_code_per_min` | `MULTICA_AUTH_SEND_CODE_PER_MIN` | `20` | reserved (global cap) |

The implementation reuses the upstream pattern in
`multica/server/internal/handler/invitation_rate_limiter.go`: the per-email
check is enforced in `send_code` via `VerificationCodeRepo::recent_for`.
When the threshold is exceeded, the response is 429 with `Retry-After: 60`.

(`send_code_per_min` is currently exposed in config but the global cap is not
enforced — left for a follow-up when the request-id / IP-keyed limiter lands;
see `docs/03-CRATE-MAPPING.md`.)

## Cookies, CSRF, sliding session

- Cookie name: `mc_auth::DEFAULT_SESSION_COOKIE` (`"multica_session"`)
- `HttpOnly`, `SameSite=Lax`, `Secure` (production only — dev keeps `Secure=false`)
- `Max-Age` = `session_ttl_secs` (default 30 days)
- CSRF token: returned in both response body (`csrf_token`) and header
  `X-Multica-Csrf`; the token is generated once at session creation and
  **kept stable across refresh** (MUL-7436).
- `/api/auth/refresh` extends `expires_at` and updates `last_seen_at`; the
  session id and CSRF token are preserved so other browser tabs holding the
  CSRF are not invalidated mid-session.

## DB schema

The `verification_code` table (defined in `migrations/0001_init.up.sql`) maps to
the repo as:

| Column | Use |
| --- | --- |
| `id` | primary key |
| `user_id` | set later (M2 will backfill via Google OAuth) |
| `email` | lowercased at write time |
| `purpose` | `email_verification` / `password_reset` / `two_factor` / `workspace_invite` |
| `code_hash` | `hex(sha256(code))`; the raw code is never stored |
| `attempts` | incremented on wrong-code attempts (cap at 5) |
| `expires_at` | TTL enforced in `consume` |
| `consumed_at` | atomic-consume marker; `consume` returns `None` when set |
| `created_at` | for `recent_for` rate window |

The `personal_access_token` table is owned by sub-issue C; this sub-issue ships
its DB-backed repo (`mc-repos::pat::PatRepo`) so the migration does not block.

**Schema addition (this slice)**: `0001_init.up.sql` had no revocation column on
`personal_access_token`, but the issue requires `PatRepo::revoke(id)` to set
`revoked_at`. Added `migrations/0002_pat_revoked_at.up.sql`
(`ALTER TABLE personal_access_token ADD COLUMN revoked_at TIMESTAMPTZ`),
semantically equivalent to upstream's `revoked BOOLEAN`. Applied cleanly on a
fresh PostgreSQL 16 database via `multica-migrate run`.

## SMTP / mailer

`send_code` currently emits a `tracing::info!` line with the email (and the code
in dev mode). The actual email send is **TODO for M9** (`mc-mailer` will pick
up the verification code and dispatch via Resend / SES / SMTP). The placeholder
is intentional — sub-issue M9 will replace the call site without touching the
repo or the route contract.

## Tests

End-to-end (skip without `DATABASE_URL`):

- `send_then_verify_full_flow` — send-code → verify-code → 200 + session cookie
- `verify_with_wrong_code_returns_401`
- `verify_with_consumed_code_returns_401` (double-submit)
- `verify_with_expired_code_returns_401`
- `refresh_renews_session` (in-memory; no DB needed)
- `refresh_unknown_session_returns_401`

`mc-repos` tests (also skip without `DATABASE_URL`):

- `verification_code::create_and_get_round_trip`
- `verification_code::consume_is_idempotent`
- `verification_code::expired_code_cannot_be_consumed`
- `verification_code::recent_for_counts_window`
- `verification_code::prune_expired_removes_old_rows`
- `pat::create_and_get_by_token`
- `pat::revoke_blocks_get_by_token`
- `pat::list_for_user_returns_active_only`

Local verification (2026-09-22, PostgreSQL 16 on 127.0.0.1, migrations
`0001` + `0002` applied via `multica-migrate run`):

```text
cargo build --workspace                        # pass
cargo test  --workspace --no-fail-fast         # pass — 0 failures,
                                               # all 8 mc-repos DB tests and all
                                               # 6 mc-http auth e2e tests executed
                                               # against a real database (not skipped)
```

## Upstream line references (`github.com/louloulin/multica`)

| Our handler | Upstream counterpart |
| --- | --- |
| `send_code` | `server/internal/handler/auth.go:298` `Handler.SendCode` |
| `verify_code` | `server/internal/handler/auth.go:388` `Handler.VerifyCode` |
| `logout` | `server/internal/handler/auth.go:763` `Handler.Logout` |
| `refresh_session` | `server/internal/handler/session.go:50` `Handler.RefreshSession` |
| 6-digit check | `auth.go:150` `isSixDigitCode` |
| dev bypass code | `auth.go:133` `isDevVerificationCode` |
| first-login user upsert | `auth.go:190` `findOrCreateUser` |
| code generation | `auth.go:124` `generateCode` |
| attempts increment | `auth.go:415` (`IncrementVerificationCodeAttempts`) |
| per-email limiter pattern | `handler/invitation_rate_limiter.go:30` `DefaultInvitationRateLimits` |
| global `RATE_LIMIT_AUTH_VERIFY=20/min` | `server/cmd/server/router.go:1470` |

Upstream `SendCode` also enforces "max 1 code / 60 s per email" (auth.go:353);
our per-email default is 5/min because LUM-1345 specifies
`send_code_per_email_per_min = 5` — the knob exists to retune it.

## Baseline repairs required to build (pre-existing on `feat/multica-rs-initial`)

The workspace had **never been compiled** before this slice; `cargo
build --workspace` failed in crates outside this sub-issue's file list. All
fixes are minimal and are called out here so the master (LUM-1342) can review
them separately from the auth feature:

- **Toolchain**: `workspace.package.rust-version = 1.80`, but the machine only
  had `/usr/bin/cargo 1.75` (which also cannot even parse current crates.io
  manifests — several now require `edition2024`). Installed rustup stable
  **1.98.1** (+ rustfmt, clippy) under `~/.cargo/bin`, and pointed the global
  `~/.cargo/config.toml` at the rsproxy.cn sparse index mirror (upstream
  `static.crates.io` was ~50 KB/s here). Use `PATH="$HOME/.cargo/bin:$PATH"`.
  `Cargo.lock` is intentionally **not** committed (owned by M1-D).
- missing crate deps: `mc-errors→anyhow`, `mc-config→dirs`,
  `mc-migrate/mc-plugin-protocol→tokio`, `mc-secrets→tempfile (dev)`,
  `mc-http→{chrono,hex,sha2,uuid,rand,sqlx,parking_lot,mc-repos}`,
  `mc-repos→mc-auth`, `mc-auth→mc-secrets`.
- `mc-auth/src/container.rs` imported a non-existent `crate::store`; the
  in-memory secrets store lives in `mc-secrets` (`DefaultSecretsBackend` is
  kept, its import fixed).
- `mc-realtime`: `EventBus` derived `Default` over `Arc<Bus>` (no `Default`);
  replaced with `with_capacity(DEFAULT_CHANNEL_CAPACITY)`.
- `mc-storage/src/lib.rs`: missing `use async_trait::async_trait;` (the
  attribute error cascaded into 14 bogus trait errors).
- `mc-authz/src/lib.rs::decide`: `use Resource::*` collided with `Action::*` /
  `Principal::*` (E0659/E0408 on `Comment`/`Agent`/`Plugin`/`Channel`) and the
  match missed `Principal::System`; dropped the glob (patterns are already
  `Resource::`-qualified) and added the `System` arm.
- `apps/mc-server/src/main.rs`: `apply_default_middleware` lives at the crate
  root (not `mc_http::middleware`), state must be injected *before* the
  middleware chain (`Router<Arc<AppState>> → Router<()>`), and
  `AdapterRegistryStub::register` takes a name, not a count.
- `mc-errors/http.rs`: `VerificationCodeInvalid` 422 → **401** (issue
  acceptance; see failure-path table above).
- `mc-db/migrate.rs::split_sql_statements`: a trailing `-- comment` after the
  last `;` was emitted as an extra statement (`buf_has_sql` guard now drops
  comment-only remainders).
- `mc-telemetry/redact.rs::redact_str`: rewrote the key/value scanner — the old
  one never matched JSON-style `"password": "..."` (a `"` between key and `:`
  terminated the key before any separator check), so `str_redaction_handles_json_style`
  failed. The rewrite keeps `key=value`, `key: value` and adds quoted-key
  support.
- `mc-core/src/workspace.rs`, `mc-realtime`, `mc-storage/local.rs`,
  `mc-ws`, `mc-plugin-protocol`: pre-existing unused-import warnings left
  untouched (not `-D warnings` clean yet — flagged for LUM-1342/CI policy).

## Boundary notes for the master branch (LUM-1342)

- `mc-http/src/middleware.rs` is **not** touched — session parsing / middleware
  ownership is sub-issue A's. If a shared abstraction becomes necessary (e.g. a
  `resolve_session` helper usable by both A and B), please coordinate.
- `mc-repos/src/lib.rs` is **not** touched — the `pub mod` list and module
  ordering are the agreed concurrent-merge convention.
- `mc-http/src/routes/mod.rs` adds `pub mod auth;` alongside the existing
  `health` and `openapi`. No other change to that file.
- The user upsert in `verify_code` uses direct `INSERT ... ON CONFLICT DO UPDATE`
  against the `user` table — `mc-repos::user::UserRepo` is owned by sub-issue A
  and we deliberately don't preempt its CRUD surface.
- `mc_db::Db::connect_lazy` was added (non-invasive — same shape as `connect`,
  just defers network) to support tests that don't need a live DB. This is a
  small infra addition; safe to keep.