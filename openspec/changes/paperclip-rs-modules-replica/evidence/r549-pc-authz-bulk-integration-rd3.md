# R549 / M50 — pc-authz 第三批路由接入（projects + companies/members）

## 本轮完成

把 `pc-authz::enforce_permission` 接入 4 个新写路由：

### 路由清单（M50 新增 4 个）

| 文件 | 路由 | PermissionKey |
|---|---|---|
| `projects.rs` | POST `/api/projects` | `PipelinesWrite` |
| `projects.rs` | PATCH `/api/projects/:id` | `PipelinesWrite` |
| `projects.rs` | POST `/api/companies/:company_id/projects` | `PipelinesWrite` |
| `companies.rs` | DELETE `/api/companies/:company_id/members/:member_id` | `UsersManagePermissions` |
| `companies.rs` | PATCH `/api/companies/:company_id/members/:member_id/permissions` | `UsersManagePermissions` |

### 累计 pc-authz 接入路由

| 阶段 | 路由数 | 累计 |
|---|---|---|
| M42 首个接入 | 1 | 1 |
| M44 多路由接入 | 2 | 3 |
| M45 批量接入 | 9 | 12 |
| M49 第二批 | 11 | 23 |
| **M50 第三批** | **5** | **28** |

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
| 路由接入（**28 个**） | ✅ M42-M50 |
| e2e parity 测试 vs Node（22 + 6） | ✅ M46 |
| Trust preset + low-trust boundary | ✅ M47 |
| 全量接入所有受保护路由 | ⏳ 渐进（剩余 documents / cases 子资源 / environments / secrets / skills / agents 子资源） |
