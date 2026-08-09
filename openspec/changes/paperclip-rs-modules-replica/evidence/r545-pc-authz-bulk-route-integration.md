# R545 / M45 — pc-authz 批量路由接入（agents + pipelines + routines）

## 本轮完成

把 `pc-authz::enforce_permission` 批量接入 9 个真实写路由：

### 路由清单

| 路由文件 | 路由 | PermissionKey |
|---|---|---|
| `agents.rs` | `POST /api/agents` | `AgentsCreate` |
| `agents.rs` | `POST /api/companies/:company_id/agents` | `AgentsCreate` |
| `agents.rs` | `PATCH /api/agents/:id` | `AgentsConfigure` |
| `agents.rs` | `DELETE /api/agents/:id` | `AgentsConfigure` |
| `pipelines.rs` | `POST /api/pipelines` | `PipelinesWrite` |
| `pipelines.rs` | `PATCH /api/pipelines/:id` | `PipelinesWrite` |
| `pipelines.rs` | `POST /api/pipelines/:id/archive` | `PipelinesWrite` |
| `pipelines.rs` | `DELETE /api/pipelines/:id` | `PipelinesWrite` |
| `routines.rs` | `POST /api/routines` + `POST /api/companies/:company_id/routines` | `PipelinesWrite` |

### 模式

每个被接入的路由都需要：
1. 注入 `AxumExtension<AuthContext>`
2. 用 `enforce_permission(db, actor, company_id, PermissionKey)` 检查
3. 对 path 参数路由 (`PATCH/DELETE`)，先 SQL 取 `company_id` 再 enforce
4. 失败映射到 `ApiError::Forbidden(explanation)`

### 验证

- `cargo check -p pc-http`：0 errors
- `cargo test --workspace --lib -- --test-threads=1`：**4962 passed**（40 suites，无回归）
- `bash scripts/diff-routes.sh`：**100.0%**（node=581 rust=883 missing=0）

### 累计 pc-authz 路由接入

| 阶段 | 路由数 |
|---|---|
| M42 首个接入 | 1（create_label） |
| M44 多路由接入 | 2（approvals approve/reject） |
| **M45 批量接入** | **9** |
| **合计** | **12** |

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| HTTP 便捷 API（enforce_*） | ✅ M42 |
| Mention / consent / parent-report / responsible_user | ✅ M43 |
| 路由接入（12 个） | ✅ M42-M45 |
| 全量接入所有受保护路由 | ⏳ 渐进（剩余 ~30+ 路由） |
| e2e parity 测试 vs Node | ⏳ M46 |
| Low-trust preset 解析 / DB-backed boundary | ⏳ M47 |
