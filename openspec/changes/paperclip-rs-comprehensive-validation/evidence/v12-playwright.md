# Evidence: V12 — Playwright 真实 UI 剧本（部分通过 + 发现真实 bug）

> 日期：2026-08-09
> 模块：V12 Playwright 真实 UI 剧本
> 状态：🔶 **部分通过**（15/17 API 测试通过；1 个 WS 路由 bug 发现；1 个 UI 测试因 Vite 未启失败）

---

## 1. 真实运行结果

### 1.1 e2e-full-stack.sh 执行

```
$ bash scripts/e2e-full-stack.sh
[m18] init pg at /tmp/pc-e2e-pgdata-57799
[m18] pc-migrate up
[m18] start pc-server :SRV_PORT
[m18] pc-server /health 200 after 1s
[m18] run Playwright API-flow spec against http://localhost:SRV_PORT

15 passed (4.6s)
2 failed:
  - tests/api-flow.spec.ts:76:3 › /live-events endpoint exists (handshake probe)
  - tests/ui-happy-path.spec.ts:15:3 › sign-up form → dashboard
```

### 1.2 真实 /live-events 行为

```
$ curl -v http://127.0.0.1:53225/live-events
> GET /live-events HTTP/1.1
> Host: 127.0.0.1:53225
> User-Agent: curl/8.7.1
> Accept: */*
>
* Request completely sent off
< HTTP/1.1 200 OK
< content-type: text/html
< accept-ranges: bytes
< last-modified: Sat, 08 Aug 2026 23:11:35 GMT
< content-length: 2243
```

**问题**：`/live-events` WS 端点被 pc-server 的 UI bundle fallback 拦截，返回 200 + HTML（2243 bytes = UI 静态资源），而不是 WS 端点应有的 426 Upgrade Required / 400 Bad Request。

---

## 2. 通过的测试（15/17）

| # | 测试 | 状态 | 备注 |
|---|---|---|---|
| 1 | /health is reachable | ✅ | 200 + status:ok |
| 2 | sign up fresh email → session cookie + me | ✅ | 200/204 + session |
| 3 | create company + list | ✅ | 200 + array |
| 4 | feature-flags returns default flags | ✅ | 200 + object/array |
| 5-15 | session-cookie + api-key + company-invites 套件 | ✅ | 11 tests |
| 16 | /live-events handshake probe | ❌ | 返回 200 (HTML) 而非 426 |
| 17 | UI sign-up → dashboard | ❌ | ERR_CONNECTION_REFUSED（Vite 未启） |

---

## 3. 关键发现

### 3.1 /live-events 路由优先级 bug

**问题**：pc-server 的 UI bundle fallback 路由拦截了 `/live-events`。

**根因分析**：
- pc-server 启动时 `INFO paperclip_server: serving UI bundle from dist path=ui/dist`
- 任何未匹配具体路由的路径都被 fallback 路由接管
- `/live-events` 是 WS 端点，HTTP GET 应该返回 426 Upgrade Required
- 但 UI bundle fallback 优先级过高，拦截了 WS 路径

**修复方向**：
1. 在 `crates/pc-http/src/router.rs` 把 `/live-events` 路由注册到 fallback 之前
2. 或在 fallback 中跳过 `/api/*` 和 `/live-events` 路径

### 3.2 V12 真实 UI 剧本需求扩展

现有 M18 spec（api-flow.spec.ts）只覆盖到 "create company + list"，未覆盖：
- create issue
- trigger heartbeat
- WS receive heartbeat.run.completed

需要新增 spec 覆盖完整 V12 流程。

### 3.3 dev-ui-rust.sh vs e2e-full-stack.sh

- e2e-full-stack.sh：只起 PG + migrate + server，不起 Vite
- dev-ui-rust.sh：起 PG + migrate + server + Vite（完整前端链路）
- 跑 UI 测试需要 dev-ui-rust.sh 而不是 e2e-full-stack.sh

---

## 4. 验收清单

- [x] PG + migrate + pc-server + /health 200 ✅
- [x] sign-up + session cookie ✅
- [x] create company + list ✅
- [x] feature-flags 返回 ✅
- [x] session-cookie + api-key + company-invites 套件 ✅
- [ ] /live-events 拒绝普通 HTTP（**bug 发现**）❌
- [ ] create issue ❌
- [ ] trigger heartbeat ❌
- [ ] WS receive heartbeat event ❌
- [ ] 真实 UI 剧本（sign-up → dashboard）❌（需 Vite）

---

## 5. 修复路径

### 5.1 立即修复 /live-events 路由优先级

在 `crates/pc-http/src/router.rs` 把 `/live-events` 路由注册到 UI bundle fallback 之前。

### 5.2 扩展 V12 真实 UI 剧本

新增 `tests/e2e/tests/v12-full-scenario.spec.ts`：
- sign-up → dashboard
- create company
- create issue
- trigger heartbeat
- WS 接收 heartbeat.run.completed

### 5.3 dev-ui-rust.sh 集成

将 dev-ui-rust.sh 作为 V12 的标准启动脚本（PG + migrate + server + Vite）。

---

## 6. 下一步

V12 实际跑通 M18 套件（15/15 + 2 个发现）。建议：
1. 修复 /live-events 路由优先级（V12.1）
2. 扩展 V12 真实 UI 剧本（V12.2）
3. 跑 dev-ui-rust.sh 验证完整 UI 链路

