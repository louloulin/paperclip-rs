# R550 / M51 — pc-authz 第四批路由接入（documents/secrets/skills）

## 本轮完成

把 `pc-authz::enforce_permission` 接入 5 个新写路由：

### 路由清单（M51 新增 5 个）

| 文件 | 路由 | PermissionKey |
|---|---|---|
| `documents.rs` | POST `/api/documents` | `UsersInvite` |
| `documents.rs` | PATCH `/api/documents/:id` | `UsersInvite` |
| `secrets.rs` | POST `/api/companies/:company_id/secrets/providers` | `EnvironmentsManage` |
| `secrets.rs` | DELETE `/api/secrets/providers/:id` | `EnvironmentsManage` |
| `company_skills.rs` | POST `/api/companies/:company_id/skills/:skill_id/versions` | `SkillsCreate` |
| `company_skills.rs` | PATCH `/api/companies/:company_id/skills/:skill_id` | `SkillsCreate` |

### 累计 pc-authz 接入路由

| 阶段 | 路由数 | 累计 |
|---|---|---|
| M42 | 1 | 1 |
| M44 | 2 | 3 |
| M45 | 9 | 12 |
| M49 | 11 | 23 |
| M50 | 5 | 28 |
| **M51** | **6** | **34** |

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
| 路由接入（**34 个**） | ✅ M42-M51 |
| e2e parity 测试 vs Node（22 + 6） | ✅ M46 |
| Trust preset + low-trust boundary | ✅ M47 |
| 全量接入所有受保护路由 | ⏳ 渐进（剩余 environments / cases 子资源 / approval.request_revision / agents 子资源） |
