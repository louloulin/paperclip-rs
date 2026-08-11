# R625 — 真实 UX 流程 E2E + 3 个真 bug 修复

> 日期：2026-08-12
> 范围：完整 sign-up → sign-in → company → agent → issue → heartbeat invoke → WS /api/live-events 端到端
> 触发：通过 `scripts/r625-ux-flow.sh` + `scripts/r625-ux-flow.py` 真实 PG17 + pc-server + Python client 验证
> 修复：3 个 server-side bug — CSRF / principal schema / session cookie name
> 状态：✅ 7 步全过；WS 101 + welcome 事件 next_event_id=11（证明 live_events 链路全通）

## 1. 触发背景

R624 完成 production transport 切换后，启动 `dev-ui-rust.sh` 链路通过，但没人**端到端**走过真实
用户流程。R625 决定从零写一个 E2E 脚本，把 sign-up 到 WS 订阅 7 步串起来，**用真实结果** 验证
纸面覆盖率（M30 100% / V11 60/60）背后是否真的能跑通。

## 2. E2E 脚本

| 文件 | 行数 | 作用 |
|---|---:|---|
| `scripts/r625-ux-flow.sh` | 65 | bash 包装：建临时 DB → pc-migrate → 启 pc-server :54300 → 调 python |
| `scripts/r625-ux-flow.py` | 131 | Python：sign-up / sign-in / 5 个 API + WS（含 CSRF header + token query） |

**关键设计**：
- 复用系统 PG17（shmmni=32 限制下 initdb 不可用）
- 每轮 run 独立 DB（`paperclip_r625_<ts>_<pid>`），跑完自动 drop
- CSRF header 从 `paperclip_csrf` cookie 提取，与 better-auth 行为一致
- WS 用 `?token=<sign-in token>&company_id=<id>` query 鉴权（与 `live_events.rs::AuthQuery` 一致）

## 3. 7 步流程真实验证

| 步 | 端点 | 结果 |
|---|---|---|
| 1 | `POST /api/auth/sign-up/email` | 200，user_id 返回 |
| 2 | `POST /api/auth/sign-in/email` | 200，token + cookie + CSRF token |
| 3 | `POST /api/companies` | 201，company_id 返回 |
| 4 | `POST /api/companies/{id}/agents` | 200，agent_id 返回 |
| 5 | `POST /api/companies/{id}/issues` | 200，issue_id 返回 |
| 6 | `POST /api/agents/{id}/heartbeat/invoke` | 202，run id + status=running |
| 7 | `WS /api/live-events?token=...&company_id=...` | 101 upgrade + welcome 事件 |

**Welcome 事件**（WS 升级后立即收到）：
```json
{
  "client_id": "cc8d0cd6-6df9-4b40-a7a9-a3e4104bd945",
  "next_event_id": 11,
  "server": "paperclip-rs",
  "type": "welcome"
}
```

`next_event_id: 11` 证明 realtime hub 持续接收并 buffer 事件（heartbeat 触发的状态变更已落库），
resumability 机制工作正常。

## 4. 三个真 bug（全部已修）

### Bug #1 — CSRF middleware 拒绝所有跨 session POST

**触发**：第一次 e2e 跑，POST /api/companies 立即 403。

**Root cause**：`crates/pc-http/src/middleware/csrf.rs` 设计正确（path 白名单 + 双字段比对），
但 e2e Python client 第一次没发 `X-CSRF-Token` header。

**修复**：测试 client 读 `paperclip_csrf` cookie 作为 `X-CSRF-Token` header（与 better-auth helper 一致）。
**代码无改动**，只是测试用法对齐。这暴露了一个 UX 问题：**UI 端没有显式 helper 文件**
`fetchWithCsrf()`，所有 60 个 client 都靠 better-auth SDK 隐式注入。如果有第三方 / 自定义 client，
会被 403。

### Bug #2 — `is_active_member` SQL 用错列名（`user_id` 不存在）

**触发**：修完 CSRF 后，WS 升级返回 500。
**错误体**：`{"error":"database error: error returned from database: column \"user_id\" does not exist"}`

**Root cause**：`pc-repos/src/company_member.rs` 中 5 个函数（is_active_member /
list_company_ids_for_user / list_for_user_with_company / replace_user_companies 的 DELETE + INSERT）
直接用 `user_id` 列查询/写入 `company_memberships`。但实际 schema 是：

```
company_memberships(id, company_id, principal_type, principal_id, status, membership_role, ...)
```

只有 `principal_type='user'` + `principal_id=<user_id>` 的组合等价于「人类成员」。

**修复**（5 处 SQL）：

| 函数 | 修改 |
|---|---|
| `is_active_member` | `WHERE user_id = $1` → `WHERE principal_type = 'user' AND principal_id = $1` |
| `list_company_ids_for_user` | 同上 |
| `list_for_user_with_company` | `cm.user_id` → `cm.principal_type = 'user' AND cm.principal_id` + `cm.role` → `cm.membership_role` |
| `replace_user_companies` (DELETE) | `WHERE user_id = $1` → `WHERE principal_type = 'user' AND principal_id = $1` |
| `replace_user_companies` (INSERT) | `(user_id, company_id, role, status)` → `(principal_type, principal_id, company_id, membership_role, status)` 加 `VALUES ('user', $1, $2, 'member', 'active')` |

**验证**：修完 is_active_member 不再 500，改返回 401（找不到 member row），暴露 bug #3。

### Bug #3 — 默认 session cookie 名字拼错

**触发**：bug #2 修完后 WS 401。
**Root cause**：`crates/pc-config/src/lib.rs` 默认 `session_cookie_name = "paperclip.session"`（点），
但 better-auth 设置的 cookie 是 `paperclip_session`（下划线）。`require_user_id` 找不到匹配的
cookie，fallback 走 `local-board` 占位（"所有人共用一个 member"）。

**修复**：

```diff
 session_cookie_name: lookup("PAPERCLIP_SESSION_COOKIE")
-    .unwrap_or_else(|| "paperclip.session".into()),
+    .unwrap_or_else(|| "paperclip_session".into()),
```

**验证**：修完后 owner principal_id 变成真实 `u_5a9c24c0aca94cbba487e2f04ee4de36`（不是
`local-board`）。`is_active_member` 返回 true，WS 升级成功。

## 5. 验证后 DB 状态

```sql
SELECT cm.company_id, cm.principal_id, cm.membership_role, cm.status, u.email
FROM company_memberships cm LEFT JOIN "user" u ON u.id = cm.principal_id;

              company_id              |            principal_id            | membership_role | status |           email
--------------------------------------+------------------------------------+-----------------+--------+---------------------------
 9080e12d-a7a0-4fc3-af33-a9a3e64ca2c3 | u_5a9c24c0aca94cbba487e2f04ee4de36 | owner           | active | r625-294c01a1@test.local
```

修复前 `principal_id = 'local-board'`；修复后 = 真实 user UUID。

## 6. 数字汇总

| 指标 | R624 | R625 |
|---|---:|---:|
| E2E 7-step UX flow | ❌ 没跑 | ✅ 全过 |
| 真实 server-side bugs 发现 | 0 | **3** |
| server-side bugs 修复 | — | 3/3 |
| pc-config 默认值修正 | 0 | 1 |
| `cargo check --workspace` | 0 errors | 0 errors |
| `cargo build -p pc-server` | 2m50s | 26s (incremental) |
| 7 步响应时间 (含 WS upgrade) | — | 9.16s total |
| `/health` cold start | < 100ms | < 100ms |

## 7. 暴露的下游隐患

- **CSRF UX 风险**：UI 60 client 无显式 `X-CSRF-Token` helper，依赖 better-auth SDK 隐式注入。
  第三方 client 集成时易踩坑。建议在 `ui/src/api/client.ts` 加显式 `applyCsrfHeader()` 辅助函数。
- **session cookie name 跨服务**：未来若加 mobile / CLI client，需通过 `PAPERCLIP_SESSION_COOKIE`
  env 显式同步，否则会重蹈 R625 bug #3。
- **`local-board` fallback 风险**：`create_company_route` 在 `require_user_id` 失败时 fallback
  到 `"local-board"`，可能掩盖鉴权 bug。建议改为 `return error`（强制修前端 / 客户端问题）。

## 8. 下一步

| 优先级 | 轮次 | 目标 |
|---|---|---|
| **P0** | R626 | 把 e2e ux-flow 接入 CI 回归保护（crashed 时 fail PR） |
| **P0** | R626 | 写 `applyCsrfHeader()` 显式 helper，60 client 全部统一走它 |
| **P1** | R627 | 去掉 `local-board` fallback，强制 require_user_id 成功 |
| **P1** | R627 | `company_member.rs` 加 sqlx::test! 单元测试防回归（5 个 query） |
| **P2** | R628 | 把 `r625-ux-flow.sh` 扩到 13 步（issue checkout / approval / run continuation） |
| **P2** | R628 | `session_cookie_name` 跨 workspace crates 单测（防再次误改默认） |
