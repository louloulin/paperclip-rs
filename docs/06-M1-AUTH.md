# M1-AUTH: Authentication Flow

> **Sub-issue**: LUM-1345 (M1-B)
> **Branch**: `feat/multica-rs-m1b-auth` → merge target `feat/multica-rs-initial`
> **Status**: implementation complete; awaiting master (LUM-1342) review.

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
- `crates/mc-repos/src/verification_code.rs` — DB-backed `verification_code` table
- `crates/mc-repos/src/pat.rs` — DB-backed `personal_access_token` table
- `crates/mc-config/src/lib.rs` — `AuthConfig` TTL + rate-limit knobs
- `crates/mc-http/src/state.rs` — `ConfigSnapshot` extensions for auth fields
- `apps/mc-server/src/main.rs` — passes the new fields into `ConfigSnapshot`

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
| `code` not 6 digits | 400 |
| code expired / already consumed / wrong | 400 (`verification_code_invalid`) |
| email missing | 400 (`validation_error`) |

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

## SMTP / mailer

`send_code` currently emits a `tracing::info!` line with the email (and the code
in dev mode). The actual email send is **TODO for M9** (`mc-mailer` will pick
up the verification code and dispatch via Resend / SES / SMTP). The placeholder
is intentional — sub-issue M9 will replace the call site without touching the
repo or the route contract.

## Tests

End-to-end (skip without `DATABASE_URL`):

- `send_then_verify_full_flow` — send-code → verify-code → 200 + session cookie
- `verify_with_wrong_code_returns_400`
- `verify_with_consumed_code_returns_400` (double-submit)
- `verify_with_expired_code_returns_400`
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