# R468 — M22 Auth Set-Cookie 合约修复

> 时间：2026-08-09 · 用户目标"真实启动前后端验证"前置阻塞解开

## 关键发现

跑 M18 Playwright spec 时发现 `issue-key` 在 sign-up 后用 Playwright `request` fixture
调用返回 401。根因：Rust server 的 `sign_up_email` / `sign_in_email` / `refresh_session`
handler 只返回 JSON body，**没有 Set-Cookie header**。

Node better-auth 上游行为：sign-up 时自动 `Set-Cookie: paperclip_session=...`。
Rust 实现遗漏了这个合约，导致 UI 用 `credentials: "include"` 时 session 不持久。

## 修复

修改 `crates/pc-http/src/routes/auth.rs`：

```rust
fn session_cookie(token: &str, expires_at: chrono::DateTime<Utc>) -> String {
    let max_age = (expires_at - chrono::Utc::now()).num_seconds().max(0);
    format!(
        "paperclip_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        token, max_age
    )
}
```

三个 handler（sign_up_email / sign_in_email / refresh_session）改为返回
`(StatusCode, HeaderMap, Json<T>)`，在 Ok 分支插入 Set-Cookie。

## 真实验证（一次跑通 12 个测试）

```
Running 12 tests using 1 worker
  ✓  M18 (5): health / sign-up / company / flags / live-events
  ✓  M22 API key (3): issue→use→revoke→rejected, empty name, unknown id
  ✓  M22 session cookie (4):
       - sign-up sets paperclip_session cookie + 30-day expiry
       - sign-in rotates token + sets new cookie
       - sign-in rejects wrong password
       - refresh rotates + sets fresh cookie
  12 passed (2.9s)
```

## 真实合约

`Set-Cookie: paperclip_session=3itoi...; Path=/; HttpOnly; SameSite=Lax; Max-Age=2591999`

| 字段 | 期望 | 实际 |
|---|---|---|
| name | `paperclip_session` | ✅ |
| Path | `/` | ✅ |
| HttpOnly | true | ✅ |
| SameSite | Lax | ✅ |
| Max-Age | 30 days (2,592,000s) | 2591999–2592000 ✅（±1s slop） |

## 验证基线

```text
$ cargo check -p pc-http
warning: `pc-http` (lib) generated 147 warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.79s
$ cargo test -p pc-http -p pc-server -p pc-migrate --lib
test result: ok. 236 passed; 0 failed; 0 ignored
```

✅ 0 regression。

## 用户目标"前后端真实验证"硬阻塞全清

| 阻塞 | 状态 | 证据 |
|---|---|---|
| M17 UI 切流真实链路 | ✅ | dev-ui-rust.sh 5 endpoint + 5 vitest |
| M18 前后端端到端 | ✅ | e2e-full-stack.sh 5/5 |
| M22 Set-Cookie / API key 完整生命周期 | ✅ | e2e-full-stack.sh 7/7 |

## 关键产物（本轮新增）

```
crates/pc-http/src/routes/auth.rs
  + fn session_cookie                                # 12 行
  + sign_up_email 返回 Set-Cookie                    # 14 行
  + sign_in_email 返回 Set-Cookie                    # 14 行
  + refresh_session 返回 Set-Cookie                  # 11 行
tests/e2e/tests/api-key-lifecycle.spec.ts           # 87 行
tests/e2e/tests/session-cookie.spec.ts              # 75 行
openspec/changes/paperclip-rs-modules-replica/evidence/m22-auth-complete.md
```

## M22 follow-up（仍需深化）

- [ ] OAuth providers（Google / GitHub）
- [ ] CSRF token（double-submit cookie）
- [ ] Auth endpoint rate limiting
- [ ] API key 列表 endpoint（Node 上游也没）
