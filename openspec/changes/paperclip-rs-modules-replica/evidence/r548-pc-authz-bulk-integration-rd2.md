# R548 / M49 — pc-authz 第二批路由接入（issues / approvals 全套 / goals / folders）

## 本轮完成

把 `pc-authz::enforce_permission` 接入 11 个新写路由：

### 路由清单（M49 新增 11 个）

| 文件 | 路由 | PermissionKey |
|---|---|---|
| `issues.rs` | POST `/api/issues` | `TasksAssign` |
| `issues.rs` | PATCH `/api/issues/:id` | `TasksAssign` |
| `approvals.rs` | POST `/api/approvals` | `UsersInvite` |
| `approvals.rs` | POST `/api/approvals/:id/resubmit` | `UsersInvite` |
| `approvals.rs` | POST `/api/approvals/:id/comments` | membership check |
| `goals.rs` | POST `/api/goals` | `UsersInvite` |
| `goals.rs` | POST `/api/companies/:company_id/goals` | `UsersInvite` |
| `goals.rs` | PATCH `/api/goals/:id` | `UsersInvite` |
| `goals.rs` | DELETE `/api/goals/:id` | `UsersInvite` |
| `folders.rs` | POST `/api/companies/:company_id/folders` | `UsersInvite` |
| `folders.rs` | DELETE `/api/companies/:company_id/folders/:folder_id` | `UsersInvite` |

### 累计 pc-authz 接入路由

| 阶段 | 路由数 | 累计 |
|---|---|---|
| M42 首个接入 | 1 | 1 |
| M44 多路由接入 | 2 | 3 |
| M45 批量接入 | 9 | 12 |
| **M49 第二批** | **11** | **23** |

### 验证

- `cargo check -p pc-http`：0 errors
- `cargo test --workspace --lib -- --test-threads=1`：**4976 passed**（40 suites，无回归）
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
| 路由接入（**23 个**） | ✅ M42-M49 |
| e2e parity 测试 vs Node（22 + 6） | ✅ M46 |
| Trust preset + low-trust boundary | ✅ M47 |
| 全量接入所有受保护路由 | ⏳ 渐进（剩余 issues 子资源 / documents / projects / cases） |
