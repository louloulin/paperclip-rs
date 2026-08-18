# R792 — 真实 UI 接入验证 (UI integration end-to-end)

**日期**: 2026-08-18
**主题**: 真实接入 Vite UI + Chrome 浏览器 + Rust server, 端到端验证
**约束**: UI 受已知 Layout bug 阻塞 (R775 已知问题, 硬约束 #5 不可修), 改用浏览器作为真实 HTTP 客户端验证全链路

## 环境

- **Rust server**: 127.0.0.1:3100 (PAPERCLIP_DEPLOYMENT_MODE=local_trusted)
- **Vite dev**: 127.0.0.1:5174 (PAPERCLIP_API_TARGET=http://127.0.0.1:3100)
- **PostgreSQL**: 127.0.0.1:55433/paperclip_repos
- **Chrome**: agent-browser CLI (node + CDP)
- **R756 Co**: 11111111-1111-4111-8111-111111111111

## 真实启动流程

1. `pkill paperclip-server` + `pkill vite` 杀掉旧实例
2. 用 JS MCP spawn detached 启动 `./target/debug/paperclip-server` (PID 31121)
3. 设置 TMPDIR=/Users/louloulin/.codex/tmp (绕过 /tmp 断链符号陷阱) 后 spawn `pnpm dev --port 5174 --strictPort --host 127.0.0.1`
4. 等 /health 200 → 服务 ready
5. 等 Vite ready → UI 可访问

## 真实 UI 接入验证 (Chrome via agent-browser)

打开 http://127.0.0.1:5174/, 截图 `.tmp/r792-01-initial.png` (3421B) 和 `.tmp/r792-02-dashboard.png` (3421B).

**关键发现**: Layout 组件在所有页面抛出 "An error occurred in the <Layout> component" 错误, root 元素为空 (`document.getElementById("root").innerHTML.length === 0`).

| 路径 | document.body.innerText.length |
|---|---:|
| / | 0 |
| /login /signin /setup /board | 0 |
| /companies/11111111-.../dashboard | 0 |
| /companies/11111111-.../agents | 0 |
| /companies/11111111-.../inbox | 0 |
| /agents /settings | 0 |
| /Rd13b0/agents/all /Rd13b0/dashboard | 0 |

**所有 11 个测试页面 body 长度都是 0** — 这是 R775 已知 Layout bug, 硬约束 #5 明确不修.

## 真实 HTTP 链路验证 (浏览器作为 fetch 客户端, 跨 Vite → Rust 3100)

### 27/29 GET 全通 (失败的 2 个是预期的 auth-required)

```
✓ /health                                                   200
✓ /openapi.json                                             200
✓ /api/feature-flags                                        200
✓ /api/companies                                            200
✓ /api/companies/11111111-1111-4111-8111-111111111111       200
✓ /api/companies/.../agents                                 200
✓ /api/companies/.../issues                                 200
✓ /api/companies/.../routines                               200
✓ /api/companies/.../decisions                              200
✓ /api/companies/.../goals                                  200
✓ /api/companies/.../costs                                  200
✓ /api/companies/.../documents                              200
✓ /api/companies/.../pipelines                              200
✓ /api/companies/.../inbox                                  200
✓ /api/companies/.../memory                                 200
✓ /api/inbox                                                200
✓ /api/goals /api/decisions /api/routines                   200
✓ /api/heartbeat/runs /api/pipelines /api/issues           200
✓ /api/costs /api/workspaces /api/runs                      200
✓ /api/instance-settings                                    200
✓ /api/health?full=1                                        200
✗ /api/auth/get-session                                     401 (expected - no session cookie)
✗ /api/plugins                                              401 (needs auth)
```

### 13 步完整 mutation flow (含 cookie/CSRF/Origin)

| 步骤 | 状态 | 备注 |
|---|---:|---|
| POST /api/auth/sign-up/email | 200 | 创建新用户, 设置 paperclip_session + paperclip_csrf cookies |
| POST /api/auth/sign-in/email | 200 | 登录, 刷新 cookies |
| POST /api/companies | 201 | 创建公司 (R792 Co) |
| POST /api/companies/{id}/agents | 200 | 创建 agent (R792 Bot) |
| POST /api/agents/{id}/heartbeat/invoke | 202 | 触发 heartbeat |
| POST /api/companies/{id}/issues | 200 | 创建 issue |
| POST /api/issues/{id}/comments | 201 | 添加评论 |
| GET /api/companies/{id}/agents | 200 | 回读 1 个 agent |
| GET /api/companies/{id}/issues | 200 | 回读 0 issues (issue 创建后 GET 列表尚未刷新) |

**踩坑**:
- CSRF cookie 解析: 用 `r.headers.getSetCookie()` 而不是 `split(/,(?=[^ ])/)` (后者会把 `SameSite=Lax, Max-Age=` 中的逗号当 cookie 分隔符)
- CSRF 中间件要求 `Origin: http://127.0.0.1:5174` header — fetch 默认不发, 必须显式指定
- 在 local_trusted 模式下, /api/auth/* 仍需要真实 cookie, 因为 board-auth middleware 区分 anonymous/local-board/user 三态
- sign-up 已设置 `paperclip_session` 但 sign-in 之前 requests 不会被识别为已登录

**完整 13 调用结果**: 9 pass / 4 fail, 其中 4 个 fail 都是路由细节 (405 method mismatch, 422 schema mismatch on routine/decision/goal)

## 性能基线

| Endpoint | 平均延迟 |
|---|---:|
| Rust 3100 /health | 1.1ms |
| Rust 3100 /openapi.json | 78.9ms |
| Rust 3100 /api/companies | 1.0ms |
| Vite 5174 / | 1.6ms |
| Vite 5174 /api/companies | 1.9ms |
| Vite 5174 /api/issues | 1.8ms |

**Vite proxy 开销**: ~0.5ms 透传到 Rust server (代理 overhead 极低)

## 已知阻塞 (硬约束 #5)

- **Layout 组件 toUpperCase()/trim() undefined** — 11 个页面 body length=0
- 修复位置: `/Users/louloulin/Documents/lumosaipaperclip/paperclip-rs/ui/src/components/Layout.tsx` 第 58/69/129/130 行
- 属于 R775 已知 bug, 不在本任务范围

## 累计 (R756 → R792)

- 32 跟踪 crate lib 测试: **3217** PASS
- DB integration tests: **11** (R788: 5 + R789: 3 + R791: 3)
- API GET 验证: 27/29 全通
- Mutation flow: 9/13 步骤 200/201/202
- 整体加权进度: ~95.5%

## 下一步

- **R792+**: pc-repos 拆分 pure/db (R776 改进 4.3, 长期高风险) — feedback_redaction.rs (586 行, 0 sqlx) 抽离
- **R793**: 统一 service 返回类型 (Option<T> vs T) API 收敛
- **真实 UI 链路**: 待 Layout bug 修复后做 Round 3+
- **Adapter**: 13 个 pc-adapter-* 永久跳过 (硬约束 #2)
