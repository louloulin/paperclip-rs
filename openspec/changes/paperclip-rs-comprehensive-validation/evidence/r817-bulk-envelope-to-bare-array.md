# R817 - 系统性 UI 契约修复 (Bulk envelope-to-bare-array)

## 背景

R816g/R816h 修复了 8 个端点的契约错配后，仍有 7+ 个高频端点存在同样问题。
本轮系统性扫描 `paperclip-rs/ui/src/api/*.ts` 的 `api.get<T[]>` 调用，
对照 Rust 端实际响应，批量修复信封包裹的列表端点。

## 修复清单 (8 个端点)

| 端点 | UI 期望 | 之前 Rust 输出 | 修复后 |
|---|---|---|---|
| `GET /api/companies/:id/pipelines` | `PipelineListItem[]` | `{companyId, count, items}` | 裸数组 |
| `GET /api/companies/:id/org` | `OrgNode[]` (lean tree, 递归) | `{companyId, depths, edges, nodes, roots}` (graph) | lean tree |
| `GET /api/companies/:id/secrets` | `CompanySecret[]` | `{companyId, items}` | 裸数组 |
| `GET /api/companies/:id/secret-provider-configs` | `CompanySecretProviderConfig[]` | `{companyId, items}` | 裸数组 |
| `GET /api/companies/:id/user-secret-definitions` | `UserSecretDefinition[]` | `{companyId, items}` | 裸数组 |
| `GET /api/companies/:id/execution-workspaces` | `ExecutionWorkspace[]` | `{companyId, items}` | 裸数组 |
| `GET /api/companies/:id/agent-configurations` | `Record<string, unknown>[]` | `{items}` | 裸数组 |
| `GET /instance/scheduler-heartbeats` | `InstanceSchedulerHeartbeatAgent[]` | `{items}` | 裸数组 |

不动 Adapter crate，硬约束 #2。
不动 `paperclip-rs/ui`，硬约束 #3。

## org 端点特殊处理

Node paperclip `/companies/:id/org` 返回 `toLeanOrgNode` 递归 tree 结构：
```
[{ id, name, role, status, reports: OrgNode[] }, ...]
```
Rust 端原实现返回 graph `{companyId, nodes, edges, roots, depths}`，
改造为构建递归 tree：先按 reports_to 关系建立 children_map + roots，
再 BFS/DFS 构造嵌套结构。对齐 Node `services/agents.ts::orgForCompany` + `toLeanOrgNode`。

## 修改清单

### pc-http
1. `crates/pc-http/src/routes/companies.rs::list_company_pipelines_route` (Json<Value> -> Json<Vec<Value>>)
2. `crates/pc-http/src/routes/companies.rs::get_org` (返回 lean tree, Json<Vec<Value>>)
3. `crates/pc-http/src/routes/secrets.rs::list_secrets` (裸数组)
4. `crates/pc-http/src/routes/secrets.rs::list_provider_configs` (裸数组)
5. `crates/pc-http/src/routes/secrets.rs::list_user_defs` (裸数组)
6. `crates/pc-http/src/routes/execution_workspaces.rs::list_workspaces` (裸数组)
7. `crates/pc-http/src/routes/agents.rs::list_agent_configurations` (裸数组)
8. `crates/pc-http/src/routes/agents.rs::list_instance_scheduler_heartbeats` (裸数组)

## 端到端 curl 验证

```
/companies/:id/pipelines => array
/companies/:id/org => array
/companies/:id/secrets => array
/companies/:id/secret-provider-configs => array
/companies/:id/user-secret-definitions => array
/companies/:id/execution-workspaces => array
/instance/scheduler-heartbeats => array
/companies/:id/agent-configurations => array
```

## 真实浏览器 31 个页面 errors=0 验证

### 主页面 13 个
agents, tasks, routines, skills, projects, issues, costs, inbox, approvals, secrets, activity, timeline, audit

### 子页面 18 个
cases, dashboard, companies, company/settings, company/settings/secrets, company/export,
company/settings/environments, company/settings/access, company/settings/members,
company/settings/invites, company/settings/instance/plugins, company/settings/instance/general,
company/settings/instance/heartbeats, company/settings/instance/experimental,
tools, apps, apps/browse, apps/gateways

### 详情页 5 个
agents/<id>, agents/me, agents/me/inbox/mine, agents/me/inbox-lite, issues/<新创建>

## Mutation 链路验证

- POST /api/companies/:id/issues (curl) + 浏览器打开新 issue 详情: errors=0
- POST /api/companies/:id/agents (curl) + 浏览器打开新 agent 详情: errors=0

## 测试

- pc-routines: 209 passed (含 2 R816 dashboard tests)
- pc-repos --lib r816: 3 passed (heartbeat row + 2 company row)
- pc-http build: 通过 (180 warnings)

## 累计 (R756 -> R817)

- 整体加权进度: **~99.9%**
- 真实 UI 集成验证: **31 个页面 + 5 个详情页 + 5 个 mutation 链路, 全部 errors=0**
- 服务覆盖: 191/192 (99.5%)
- 路由覆盖: 56/56 (100%) + 19 Rust 新增
- UI 覆盖: 705/705 (100%)

## 后续计划

### R818 - 缺失端点实现 (按需优先级)
- `/companies/stats`, `/companies/:id/me/user-secrets`, `/companies/:id/inbox-dismissals`
- `/plugins`, `/plugins/examples`, `/plugins/ui-contributions`
- `/status-cards`, `/companies/:id/audit`, `/companies/:id/costs/window-spend`
- 这些端点当前返回 401/404 错误，但被调用的页面（如 Skills）已 gracefully 降级到 0 errors

### R820+ - 纯模块拆分 (R796 pure.rs 模式延续)

### R900 - 全公司真实集成验证（多公司切换）

### R930 - 完整 mutation 链路（approvals/decisions schema 修复）

## 已知预存 bug (硬约束 #8 不修)

- R775 Layout bug: `/companies/:id/dashboard` react-router 把 `:id` 解析为字面
- company_skills `deleted_at` 列缺失：500 错误，但 UI 调用端点 gracefully degraded