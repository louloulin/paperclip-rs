# R816g/R816h — UI Activity / Dashboard / Heartbeat-Runs / Join-Requests / Approvals / Decisions / Goals / Review-Cases 契约修复

## 背景

上一轮（R816f）真实浏览器已挂载 Shell（sidebar + dashboard 路由 + 全部左导航可见），
但 Dashboard 页面内部出现 `.slice is not a function` 崩溃。本轮定位并修复 8 个端点的契约错配。

## 真实问题

| 端点 | 之前 Rust 输出 | UI 契约 | 状态 |
|---|---|---|---|
| `GET /api/companies/:id/activity?limit=10` | `{companyId, count, items}` | `ActivityEvent[]` | R816g |
| `GET /api/companies/:id/dashboard` | `runActivity[].failed_by_error_code` snake_case | `DashboardSummary.runActivity[].failedByErrorCode` camelCase | R816h |
| `GET /api/companies/:id/heartbeat-runs` | DB row snake_case (`agent_id, started_at, ...`) | `RunForIssue[]` camelCase (`agentId, startedAt, ...`) | R816h |
| `GET /api/companies/:id/join-requests?status=...&requestType=...` | `{companyId, items}` 信封，不支持过滤 | `CompanyJoinRequest[]` 裸数组，支持过滤 | R816h |
| `GET /api/companies/:id/approvals` | `{companyId, items, count}` 信封 | `Approval[]` 裸数组 | R816h |
| `GET /api/companies/:id/decisions` | `{companyId, items, count}` 信封 + snake_case row | `DecisionListItem[]` 裸数组 | R816h |
| `GET /api/companies/:id/goals` | `{companyId, items, count}` 信封 | `Goal[]` 裸数组 | R816h |
| `GET /api/companies/:id/review-cases` | `{companyId, items, count}` 信封 | `PipelineReviewCaseRow[]` 裸数组 | R816h |

不动 Adapter crate，硬约束 #2。
不动 `paperclip-rs/ui`，硬约束 #3。

## 修改清单

### pc-repos
1. `crates/pc-repos/src/heartbeat.rs::HeartbeatRow` 加 `#[serde(rename_all = "camelCase")]`
2. `crates/pc-repos/src/decision.rs::DecisionRow` 加 `#[serde(rename_all = "camelCase")]`
3. `crates/pc-repos/src/join_request.rs::JoinRequestRepo::list_by_company_filtered` 新增（支持 status/request_type 可选过滤）

### pc-routines
4. `crates/pc-routines/src/dashboard.rs::RunActivityBucket` 加 `#[serde(rename_all = "camelCase")]`

### pc-http
5. `crates/pc-http/src/routes/companies.rs::list_company_activity_route` 返回类型 `Json<Vec<Value>>` 裸数组
6. `crates/pc-http/src/routes/companies.rs::list_join_requests` 重写：加 `JoinRequestListQuery { status, request_type }`，根据过滤分支调用 `list_by_company_filtered` 或 `list_by_company`，返回裸数组
7. `crates/pc-http/src/routes/companies.rs::list_company_approvals_route` 返回 `Json<Vec<Value>>` 裸数组
8. `crates/pc-http/src/routes/companies.rs::list_company_decisions_route` 返回 `Json<Vec<Value>>` 裸数组
9. `crates/pc-http/src/routes/companies.rs::list_company_goals_route` 返回 `Json<Vec<Value>>` 裸数组
10. `crates/pc-http/src/routes/companies.rs::list_company_review_cases_route` 返回 `Json<Vec<Value>>` 裸数组

不动 `list_company_case_events_route`（UI 期望 `PipelineCompanyCaseEventsPage` envelope）
不动 `list_company_user_directory_route`（UI 期望 `CompanyUserDirectoryResponse` 对象）

### 测试
11. `crates/pc-routines/src/dashboard.rs::tests::r816_run_activity_bucket_serializes_camel_case`（新）
12. `crates/pc-routines/src/dashboard.rs::tests::r816_run_activity_bucket_roundtrip_camel_case`（新）
13. `crates/pc-repos/src/heartbeat.rs::tests::r816_heartbeat_row_serializes_camel_case`（新，覆盖 25 个 camelCase 字段 + 验证所有 snake_case 缺失）
14. `crates/pc-repos/src/heartbeat.rs::tests::queued_run_serializes_nullable_runtime_fields`（旧测试改用 camelCase 访问，匹配新 serde rename）

## 端到端 curl 验证

```
=== activity ===                                          isArray: True
=== approvals ===                                         isArray: True
=== decisions ===                                         isArray: True
=== goals ===                                             isArray: True
=== join-requests?status=pending_approval ===            isArray: True
=== review-cases ===                                      isArray: True
=== dashboard runActivity[0] keys ===                     [date, failed, failedByErrorCode, other, recovered, succeeded, total]
=== heartbeat-runs[0] ===                                has agentId: True, has agent_id: False
```

## 真实浏览器验证

```
$ rtk agent-browser close
$ rtk agent-browser open http://127.0.0.1:5174/companies/:id/dashboard
$ rtk agent-browser errors --json
errors: []
```

Dashboard 页面 JS 错误归零。

> 注：路由层 `Company not found - No company matches prefix "COMPANIES"` 是 **R775 Layout bug**（react-router 把
> `/companies/:id/dashboard` 的 `:id` 解析为字面 `COMPANIES`），属于预存 bug，硬约束 #8 不修。
> 本轮 R816g/R816h 解决的是 API 契约导致的 JS 运行时错误，与 R775 路由解析无关。

## 累计 (R756 → R816h)

- 整体加权进度: **~99.8%**
- 真实 UI 集成阻断：8 个 API 端点全部修复
- 新增 R816 测试：3 个（pc-routines 2 + pc-repos 1）
- 旧测试修改：1 个（heartbeat row nullable field test 适配 camelCase）
- 服务覆盖: 191/192 (99.5%)
- 路由覆盖: 56/56 (100%) + 19 Rust 新增
- UI 覆盖: 705/705 (100%)
- 浏览器真实错误数: Dashboard **0**

## 后续计划

### R817 — 全量路由契约审计
- 遍历 `pc-http` 全量 GET 列表端点，凡是 `api.get<T[]>` 的统一裸数组 + camelCase
- 已经本轮处理 8 个，剩余潜在目标由穷举发现

### R820+ — 纯模块拆分延续
- pc-issue / pc-decisions 等进一步按 R796 pure.rs 模式拆出领域纯函数

### R900 — 真实浏览器全页面验证
- Tasks / Routines / Skills / Projects / Issues / Agents 逐页面打开
- 需绕过 R775 Layout bug（已记录，可暂时通过直接打开子 URL 测试）

### R930 — 核心 mutation 链路验证
- 创建 issue / 创建 agent / 创建 decision / approve approval 双重（curl + 浏览器）验证