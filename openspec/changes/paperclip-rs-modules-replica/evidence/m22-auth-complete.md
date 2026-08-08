# Evidence: M22 — Auth/AuthZ 关键缺口修复（Set-Cookie）

> 用户真实使用前置。**真实验证通过**。

## 真实差距发现

通过 Playwright 跑 M18 spec 时发现：sign-up 后用 Playwright `request` fixture 调用
需要 cookie auth 的 endpoint（如 issue-key）返回 401。深入排查发现：

**根因**：`crates/pc-http/src/routes/auth.rs` 的 `sign_up_email` / `sign_in_email` / `refresh_session`
三个 handler 都只返回 JSON body，**没有设置 Set-Cookie header**。React UI 用 `credentials: "include"`
走 cookie session，但没有 cookie 就只能手动从响应 JSON 拿 token 自己塞进 storage。

这是 Rust server 与 Node better-auth 上游的契约差异：
- Node better-auth：sign-up 时自动 `Set-Cookie: paperclip_session=...`
- Rust server（修复前）：只返回 `{token: "..."}`，靠调用方自己管理

没有 cookie session，UI 无法做"登录后持续保持"——这阻塞了真实登录使用。

## 修复

修改 `crates/pc-http/src/routes/auth.rs`：

1. 新增 `session_cookie(token, expires_at) -> String` helper：
   ```
   paperclip_session=<token>; Path=/; HttpOnly; SameSite=Lax; Max-Age=<n>
   ```
2. `sign_up_email` / `sign_in_email` / `refresh_session` 三个 handler 的返回类型改为
   `(StatusCode, HeaderMap, Json<T>)`，在 Ok 分支用 helper 构造 Set-Cookie 写入 HeaderMap。

## 真实验证（Playwright）

`scripts/e2e-full-stack.sh` 一次跑通：

```
Running 12 tests using 1 worker
  ✓  1-5  M18 API flow (health, sign-up, company, flags, live-events)
  ✓  6-8  M22 API key lifecycle (issue→use→revoke→rejected, empty name, unknown id)
  ✓  9    sign-up sets paperclip_session cookie + 30-day expiry
  ✓  10   sign-in rotates token + sets new cookie
  ✓  11   sign-in rejects wrong password
  ✓  12   refresh rotates + sets fresh cookie
  12 passed (2.9s)
```

### session cookie 合约验证

```
Set-Cookie: paperclip_session=3itoiIKlVjqF1-PZxdYBYdLAb_IbRACO8bwTpBKeY_Y; Path=/; HttpOnly; SameSite=Lax; Max-Age=2591999
              ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^                          ^^^^^^^^ ^^^^^^^^^^ ^^^^^^^^
              token (URL-safe base64, ≥32 chars)                          Path   HttpOnly    SameSite   30天 ±1s
```

| 项 | 期望 | 实际 |
|---|---|---|
| Cookie 名 | `paperclip_session` | ✅ |
| Path | `/` | ✅ |
| HttpOnly | true | ✅ |
| SameSite | Lax | ✅ |
| Max-Age | 30 days (≈ 2,592,000s) | 2591999–2592000 ✅ |

### API key 完整生命周期验证

```
1. POST /api/auth/sign-up/email (新用户)
2. POST /api/auth/issue-key {name}    → 200 + {id, raw_token: "pcak_..."}
3. GET  /api/auth/get-session        + Bearer pcak_... → 200 (auth OK)
4. POST /api/auth/revoke-key {id}    → 204
5. GET  /api/auth/get-session        + 同一个 Bearer → 401 (吊销生效)
```

### Refresh token rotation 验证

```
1. POST /api/auth/sign-up/email → token A
2. POST /api/auth/refresh {token: A} → 200 + Set-Cookie + token B (B != A)
3. 旧 token A 仍可在 DB 查到但已被吊销，新 token B 立即生效
```

## 代码改动清单

| 操作 | 文件 | LOC |
|---|---|---|
| 新增 `session_cookie` helper | `crates/pc-http/src/routes/auth.rs` | +12 |
| `sign_up_email` 返回 Set-Cookie | `crates/pc-http/src/routes/auth.rs` | +14 |
| `sign_in_email` 返回 Set-Cookie | `crates/pc-http/src/routes/auth.rs` | +14 |
| `refresh_session` 返回 Set-Cookie | `crates/pc-http/src/routes/auth.rs` | +11 |
| Playwright spec — API key 完整生命周期 | `tests/e2e/tests/api-key-lifecycle.spec.ts` | +87 (新增) |
| Playwright spec — session cookie | `tests/e2e/tests/session-cookie.spec.ts` | +75 (新增) |

## 验证基线（cargo）

```text
$ cargo check -p pc-http
warning: `pc-http` (lib) generated 147 warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.79s
```

✅ 0 errors。

## 后续 M22 follow-up（仍需深化）

- [ ] **OAuth providers**（Google / GitHub）—— 当前完全未实现
- [ ] **CSRF token**（double-submit cookie）—— 当前仅靠 SameSite=Lax 防护
- [ ] **API key 列表**（GET /api/auth/keys）—— Node 上游也没，Rust 同样没有
- [ ] **rate limiting on auth endpoints** —— 当前无限流

## 结论

**M22 部分通过**（核心子项完成）：
- ✅ Set-Cookie contract（sign-up / sign-in / refresh 三处 Set-Cookie）
- ✅ API key 完整生命周期
- ✅ Refresh rotation
- ⏳ OAuth / CSRF / rate limit → follow-up
