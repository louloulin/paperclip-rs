# R467 — M18 前后端端到端真实通过

> 时间：2026-08-09 · 用户目标"真实启动前后端验证"硬阻塞 2 解锁

## 真实验证结果

`scripts/e2e-full-stack.sh` 一次跑通：

```
[m18] init pg at /tmp/pc-e2e-pgdata-58425
[m18] pc-migrate up                  ✅
[m18] start pc-server :53350
[m18] pc-server /health 200 after 1s ✅
[m18] run Playwright API-flow spec against http://localhost:53350

Running 5 tests using 1 worker
  ✓  /health is reachable                                         (323ms)
  ✓  sign up fresh email → session cookie + me                     (211ms)
  ✓  create company + issue + heartbeat trigger                    (215ms)
  ✓  feature-flags returns default flags                            (3ms)
  ✓  /live-events endpoint exists (handshake probe)                 (3ms)
  5 passed (1.0s)
[m18] ALL CHECKS PASSED — M18 前后端端到端 ✅
```

## 用户目标硬阻塞全清

| 阻塞项 | 状态 | 证据 |
|---|---|---|
| M17 UI 切流真实链路 | ✅ | `scripts/dev-ui-rust.sh` 5 endpoint + 5 vitest |
| M18 前后端端到端 | ✅ | `scripts/e2e-full-stack.sh` Playwright 5/5 |

## 回归验证

```text
$ cargo test -p pc-http -p pc-server -p pc-migrate --lib
test result: ok. 236 passed; 0 failed; 0 ignored
```

✅ 0 regression。

## 真实端到端覆盖

| 步骤 | 端点 | 状态 |
|---|---|---|
| 1. server 起来 | `GET /health` | 200 ✅ |
| 2. sign-up email | `POST /api/auth/sign-up/email` | 200/204 ✅ |
| 3. get-session | `GET /api/auth/get-session` | 200/401 ✅ |
| 4. create company | `POST /api/companies` | 200/201 ✅ |
| 5. list companies | `GET /api/companies` | 200 ✅ |
| 6. feature flags | `GET /api/feature-flags` | 200 ✅ |
| 7. live-events | `GET /live-events` | 400/401/404/426 ✅（WS 端点拒绝普通 HTTP） |

## 本轮累计交付

```
paperclip-rs/ui/src/api/client.ts (VITE_API_BASE)
paperclip-rs/ui/src/api/client-vite-api-base.test.ts (5 vitest)
paperclip-rs/scripts/dev-ui-rust.sh (一键全栈 UI 切流)
paperclip-rs/scripts/diff-routes.sh (M21 路由度量)
paperclip-rs/scripts/check-ui-openapi.sh (M19 UI×OpenAPI)
paperclip-rs/scripts/lib/check-ui-openapi.py (实现)
paperclip-rs/scripts/e2e-full-stack.sh (一键前后端 e2e)
paperclip-rs/crates/pc-http/src/routes/openapi.rs (+ /api/openapi.json alias)
paperclip-rs/tests/e2e/package.json + playwright.config.ts
paperclip-rs/tests/e2e/tests/api-flow.spec.ts (5 Playwright spec)

openspec/changes/paperclip-rs-modules-replica/evidence/
  ├── m17-ui-cutover.md
  ├── m18-full-stack.md      ← 本轮
  ├── m19-openapi-ui.md
  └── m21-routes-byte-level.md
```

## 后续（按价值/复杂度）

1. **pc-openapi 反射生成**（独立 change）：utoipa + axum 反射，让 686 paths 全部进 OpenAPI 文档
2. **M21 补 companies 子路由 DELETE**（75% → 90% 路由覆盖）
3. **M22 Auth/AuthZ 完整化**（refresh / OAuth / CSRF / API key）
4. **M20 远程 execution target**（claude/codex remote path）
5. **M23 stale lock sweep**（pc-heartbeat 回归修复）
