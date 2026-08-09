# R507 — UI happy path 完整闭环 + 前后端 UI 对齐硬阻塞消除

> 时间：2026-08-09 · 用户硬阻塞「前后端 UI 对齐」全打通
> + e2e-full-stack.sh 现在跑完整 17 个 Playwright 测试（含 UI 真实浏览器）

## 1. 背景

R506 实现 CSRF middleware 后，api-flow / company-invites 6 个 Playwright 测试
从 403 失败变成通过，但 **UI happy path（真实 Chromium 浏览器）仍然失败**，
原因是"前后端 UI 对齐"问题：

- UI 用相对路径 `/api/auth/...` / `/api/health`，vite proxy 转发到 hardcoded `localhost:3100`
- UI 默认路由 `/` → CompanyRootRedirect → `/onboarding`（无 session）
- Rust server 的 health response 不包含 `deploymentMode` / `bootstrapStatus`
- `e2e-full-stack.sh` 不启动 vite，缺 CORS 配置

## 2. 修复 1：vite proxy 支持任意目标端口

`ui/vite.config.ts` proxy target 由 hardcoded `localhost:3100` 改为
`process.env.PAPERCLIP_API_TARGET ?? "http://localhost:3100"`。

`scripts/e2e-full-stack.sh` / `scripts/ui-happy-path.sh` 启 vite 时传：
```bash
PAPERCLIP_API_TARGET="http://localhost:$SRV_PORT" \
VITE_API_BASE="http://localhost:$SRV_PORT/api" \
  pnpm dev --port "$UI_PORT" --strictPort
```

## 3. 修复 2：UI API client 用 VITE_API_BASE（不绕过 proxy）

`ui/src/api/health.ts` 和 `ui/src/api/auth.ts` 直接用 `fetch("/api/...")`，
不走 `client.ts` 的 `BASE`，导致 vite proxy 仍然尝试连接 3100。

修改两文件用 `import { BASE } from "./client"`，所有 fetch 改为 `${BASE}/...`。
替换了 6 处硬编码路径：
- `health.ts::healthApi.get` → `${BASE}/health`
- `health.ts::requestDevServerRestart` → `${BASE}/health/dev-server/restart`
- `auth.ts::resolveAuthUrl` → `${BASE}/auth${path}`
- `auth.ts::authPost` → `${BASE}/auth${path}`
- `auth.ts::authPatch` → `${BASE}/auth${path}`
- `auth.ts::getSession` → `${BASE}/auth/get-session`
- `auth.ts::getProfile` → `${BASE}/auth/profile`

## 4. 修复 3：health response 包含 deploymentMode

`crates/pc-http/src/routes/health.rs` 现在返回：
```json
{
  "status": "ok",
  "version": "0.1.0",
  "deploymentMode": "authenticated",
  "bootstrapStatus": "ready",
  "authReady": true,
  "db": { "ok": true, "latency_ms": 0, "error": null }
}
```

`deploymentMode` 可由 `PAPERCLIP_DEPLOYMENT_MODE` 环境变量覆盖（`local_trusted` / `authenticated`）。
默认 `authenticated` — UI 的 `CloudAccessGate` 看到该字段后才会重定向到 `/auth`。

## 5. 修复 4：e2e-full-stack.sh 启动 vite + CORS

- 增加 `UI_PORT` 变量（51800 + RANDOM%200）
- 增加 vite dev server 启动 + 健康检查
- pc-server 启动时设 `PAPERCLIP_CORS_ALLOWED_ORIGINS="http://localhost:$UI_PORT,..."`
- playwright 传 `E2E_UI_URL=http://localhost:$UI_PORT`

## 6. 真实 e2e 验证

```
$ bash scripts/e2e-full-stack.sh
...
Running 17 tests using 1 worker
  ✓  /health is reachable
  ✓  sign up fresh email → session cookie + me
  ✓  create company + issue + heartbeat trigger
  ✓  feature-flags returns default flags
  ✓  /live-events endpoint exists (handshake probe)
  ✓  api-key-lifecycle 3/3
  ✓  company-invites 4/4
  ✓  session-cookie 4/4
  ✓  sign-up form → dashboard          ← UI 真实浏览器 happy path
  
17 passed (5.7s)
[m18] ALL CHECKS PASSED — M18 前后端端到端 ✅
```

## 7. 关键 bug 诊断历程

| 阶段 | 现象 | 根因 |
|---|---|---|
| 1 | UI 显示 "Failed to load health (500)" | health.ts 走 vite proxy → 3100 ECONNREFUSED |
| 2 | UI 显示 onboarding wizard（不是 Auth） | Rust health 没 `deploymentMode` 字段 |
| 3 | UI 显示 "Failed to fetch" | auth.ts 直接 fetch `/api/auth/...`，不走 BASE |
| 4 | UI CORS 错误 | e2e-full-stack.sh 缺 `PAPERCLIP_CORS_ALLOWED_ORIGINS` |
| 5 | vite proxy 还是 3100 | proxy target hardcoded，没读 env var |

每个阶段修复后用 curl + playwright debug spec 逐步定位，最终全部闭环。

## 8. 测试覆盖更新

- **pc-http**：csrf 18 + auth/health/e2e **259 测试** ✅
- **pc-plugin-host**：127 unit + 3 integration ✅
- **e2e (Playwright)**：**17/17 全过**（api-flow + api-key + company-invites + session-cookie + ui-happy-path）
