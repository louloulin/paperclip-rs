# R552 / M53 — pc-authz 第五批路由接入（cases / agents 子资源）

## 本轮完成

把 `pc-authz::enforce_permission` 接入 7 个新写路由：

### 路由清单（M53 新增 7 个）

| 文件 | 路由 | PermissionKey |
|---|---|---|
| `cases.rs` | POST `/api/cases` | `PipelinesWrite` |
| `cases.rs` | PATCH `/api/cases/:case_id` | `PipelinesWrite` |
| `cases.rs` | POST `/api/companies/:company_id/cases` | `PipelinesWrite` |
| `agents.rs` | POST `/api/agents/:id/permissions` | `UsersManagePermissions` |
| `agents.rs` | POST `/api/agents/:id/pause` | `AgentsConfigure` |
| `agents.rs` | POST `/api/agents/:id/resume` | `AgentsConfigure` |
| `agents.rs` | (skip — hire_agent already enforced) | — |

### 累计 pc-authz 接入路由

| 阶段 | 路由数 | 累计 |
|---|---|---|
| M42 | 1 | 1 |
| M44 | 2 | 3 |
| M45 | 9 | 12 |
| M49 | 11 | 23 |
| M50 | 5 | 28 |
| M51 | 6 | 34 |
| **M53** | **6** | **40** |

（一个 patch 失败重做，最终净增 6）

### 验证

- `cargo check -p pc-http`：0 errors
- `cargo test --workspace --lib -- --test-threads=1`：**4993 passed**（40 suites，无回归）
- `bash scripts/diff-routes.sh`：**100.0%**（node=581 rust=883 missing=0）

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---| 
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| HTTP 便捷 API（enforce_*） | ✅ M42 |
| Mention / consent / parent-report / responsible_user | ✅ M43 |
| **路由接入（40 个）** | ✅ M42-M53 |
| e2e parity 测试 vs Node（22 + 6） | ✅ M46 |
| Trust preset + low-trust boundary | ✅ M47 |
| Mention 解析器 | ✅ M52 |
| 全量接入所有受保护路由 | ⏳ 渐进（剩余 environments / hire_agent / approval.request_revision / agents 子资源部分） |
