# R589 — V12 完整业务流 Playwright spec（覆盖 issue/agent/dashboard）

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

**tests/e2e/tests/v12-full-flow.spec.ts（113 行）** 新增 6 个 Playwright 测试：

| # | 测试 | 覆盖点 |
|---|---|---|
| 1 | `issue CRUD round-trip` | signup → create company → create issue → list → get detail |
| 2 | `agents list returns array` | signup → company → /api/agents 列表合约 |
| 3 | `dashboard returns expected shape` | /api/dashboard 端点（200/401） |
| 4 | `/api/live-events is reachable and doesn't crash` | R576 bug 回归测试（不能 200 HTML） |
| 5 | `company stats endpoint accessible` | /api/companies/:id/stats 200/404/401 |
| 6 | `issues search returns shape` | /api/search?q=test 合约 |

## 2. 关键测试设计

### 2.1 issue CRUD 全链路

```typescript
signup → csrf → POST /api/companies → POST /api/issues → GET /api/issues?companyId= → GET /api/issues/:id
```

每一步断言：
- 状态码 ∈ {200, 201}
- 响应包含 `id` / `companyId` / `issueId` 字段
- 列表合约是数组

### 2.2 /api/live-events 回归保护

```typescript
const res = await request.get(`${BASE}/api/live-events`, { failOnStatusCode: false });
expect(res.status()).not.toBe(200);  // 关键：不能是 200 HTML
expect(res.status()).toBeLessThan(500);
```

这个断言防止 **V12 之前的 bug 回归**：pc-server 的 UI bundle fallback 不应拦截 WS 端点。R576 已经修复（添加了 `/api/companies/:company_id/events/ws` 路由），但 `/api/live-events` 路径也必须走 WS 升级逻辑。

### 2.3 company stats 用 zero-uuid

```typescript
const fakeCompanyId = "00000000-0000-0000-0000-000000000000";
const res = await request.get(`${BASE}/api/companies/${fakeCompanyId}/stats`);
// 期望 200/404/401
```

避免 fake 数据；合约正确的响应都算 pass。

## 3. 与既有 spec 协同

| 现有 spec | 覆盖 | R589 新增 |
|---|---|---|
| `api-flow.spec.ts` | /health, signup, /live-events probe | (保持) |
| `api-key-lifecycle.spec.ts` | API key 完整流程 | (保持) |
| `company-invites.spec.ts` | company invite 流程 | (保持) |
| `session-cookie.spec.ts` | session cookie 生命周期 | (保持) |
| `ui-happy-path.spec.ts` | UI 浏览器流程（需 Vite）| (保持) |
| **`v12-full-flow.spec.ts`** | (新) **issue CRUD + agent + dashboard + regression** | **6 tests** |

## 4. 使用方法

```bash
# 1. 启动 pc-server（任何方式：scripts/e2e-baseline.sh 或 e2e-full-stack.sh）
# 2. 跑 Playwright
cd tests/e2e
E2E_SERVER_URL=http://localhost:53100 npx playwright test v12-full-flow.spec.ts

# 3. 全套
npx playwright test
```

## 5. 与 V12 原始目标的关系

| V12 目标 | 状态 |
|---|---|
| 打开 UI 登录页 | ✅ ui-happy-path.spec.ts |
| 注册 + 自动登录 | ✅ api-flow.spec.ts (signup) |
| 创建公司 | ✅ api-flow.spec.ts + v12-full-flow.spec.ts |
| 创建 issue | ✅ **v12-full-flow.spec.ts** (新) |
| 启动 heartbeat | 🔶 partial（mock wakeup；真实需要 AI provider） |
| WS 收到 heartbeat.run.completed | 🔶 partial（订阅；触发依赖真实 agent） |
| macOS + Linux glibc/musl 三态均通过 | ⚠️ 依赖 CI runner |

## 6. 关键断言覆盖率

- ✅ 端点合约（200/201/401/404）
- ✅ 数据形状（数组 / 对象 / 必需字段）
- ✅ WS 路径不返回 HTML（回归保护）
- ✅ 嵌套路径参数解析（:company_id, :issue_id）

## 7. 验收清单

- [x] 6 个新测试 ✅
- [x] 113 行 TypeScript spec ✅
- [x] /api/live-events 回归保护 ✅
- [x] issue CRUD 全链路 ✅
- [x] company stats 合约 ✅
- [x] 与既有 spec 协同（不重复）✅
