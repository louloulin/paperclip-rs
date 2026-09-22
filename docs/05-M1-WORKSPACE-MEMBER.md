# M1-A：workspace + member + me HTTP 路由（LUM-1342 / LUM-1343）

本 sub-issue 覆盖 M1-A 的 DB Repo（salvage 已完成部分）与 HTTP 层：`/api/workspaces/*`
member-visible 路由 + `/api/me`，以及占位认证中间件 `require_user` / `require_member` /
`require_role`。

## 本切片覆盖的路由

| Method | Path | Handler（本仓库） | 中间件 | 上游 handler（`server/internal/handler/`） |
| --- | --- | --- | --- | --- |
| GET | `/api/workspaces` | `workspaces::list_my_workspaces` | `require_user` | `workspace.go:159 ListWorkspaces` |
| POST | `/api/workspaces` | `workspaces::create_workspace` | `require_user` | `workspace.go:202 CreateWorkspace` |
| GET | `/api/workspaces/{id}` | `workspaces::get_workspace` | `require_member` | `workspace.go:179 GetWorkspace` |
| PATCH | `/api/workspaces/{id}` | `workspaces::update_workspace` | `require_role(Owner, Admin)` | `workspace.go:373 UpdateWorkspace` |
| PUT | `/api/workspaces/{id}` | `workspaces::update_workspace`（同上，LUM-1362 补） | `require_role(Owner, Admin)` | 上游 `router.go:1699` 与 PATCH 同 handler |
| DELETE | `/api/workspaces/{id}` | `workspaces::delete_workspace` | `require_role(Owner)` | `workspace.go:1059 DeleteWorkspace` |
| POST | `/api/workspaces/{id}/leave` | `workspaces::leave_workspace` | `require_member` | `workspace.go:702 LeaveWorkspace` |
| GET | `/api/workspaces/{id}/members` | `workspaces::list_members` | `require_member` | `workspace.go:499 ListMembersWithUser` |
| PATCH | `/api/workspaces/{id}/members/{memberId}` | `workspaces::update_member`（LUM-1362 补） | `require_role(Owner, Admin)` + handler 内 owner 校验 | `workspace.go:565 UpdateMember` |
| DELETE | `/api/workspaces/{id}/members/{memberId}` | `workspaces::delete_member`（LUM-1362 补） | `require_role(Owner, Admin)` + handler 内 owner 校验 | `workspace.go:641 DeleteMember` |
| GET | `/api/me` | `workspaces::get_me` | `require_user` | `auth.go:473 GetMe` |
| PATCH | `/api/me` | `workspaces::update_me` | `require_user` | `auth.go:768 UpdateMe` |

路由注册：`crates/mc-http/src/routes/workspaces.rs::router`；
接线：`crates/mc-http/src/routes/mount.rs::mount_slice_workspace_member`
（`.merge(workspaces::router(state))`）。

### 与上游路由注册的行号对应（`server/cmd/server/router.go`）

- L1615-L1616：`GET/PATCH /api/me` → `GetMe` / `UpdateMe`
- L1657：`r.Route("/api/workspaces", ...)`
  - L1658-L1659：`GET/POST /` → `ListWorkspaces` / `CreateWorkspace`
  - member 组（`RequireWorkspaceMemberFromURL`，`middleware/workspace.go:173`）：
    `GET /{id}`、`GET /{id}/members`、`POST /{id}/leave`
  - admin 组（`RequireWorkspaceRoleFromURL(..., "owner", "admin")`，`middleware/workspace.go:185`）：
    `PUT/PATCH /{id}`
  - owner 组：`DELETE /{id}`（router.go ~L1745）

状态码与上游 middleware 对齐（`middleware/workspace.go`）：未认证 401（L224/L230）、
workspace 不存在或非成员 404（L200/L243，隐藏资源存在性）、角色不足 403（L256）；
slug 冲突 409（上游 `CreateWorkspace` unique violation 分支）。

## 鉴权中间件（占位实现）

文件：`crates/mc-http/src/middleware/authn.rs`。

- session id 取自 `X-Multica-Session` header，回退 cookie `multica_session`。
- 解析顺序（占位，sub-issue B `/auth/verify-code` 落地后收紧）：
  1. in-memory session store（`mc_auth::SessionStoreContainer`）命中 → `session.user_id`；
  2. 未命中 → 字符串本身是 UUID 时直接当 user id（本地联调：curl/测试直接传 user id）；
  3. 否则 401。
- request extensions：`AuthUser{user_id}`、`WorkspacePathId`（自 URL 解析）、
  `WorkspaceContext{workspace_id, member_id, role}`（membership 校验后）。
- `require_role(roles)` 以 `RoleLayer` 形式返回 layer，角色集合是运行时参数。
- 状态注入：axum 0.7 的 `from_fn` 不支持 `State` 提取器，须用
  `from_fn_with_state`；因此 `router(state: Arc<AppState>)` 的签名从
  `mc_http::router` 贯穿到 `workspaces::router` / `mount_slice_workspace_member`，
  而 handler 同时仍通过 `.with_state(state)` 拿到 `State<Arc<AppState>>`。

## Repo 层（任务清单第 1 项，salvage commit `2c4da9f`）

- `crates/mc-repos/src/workspace.rs`：CRUD + `get_by_slug` + `list_for_user`；
  软删（`archived_at`）。
- `crates/mc-repos/src/member.rs`：CRUD + `list_for_workspace` + `list_with_user`
  （LEFT JOIN user，避免 N+1）+ `get_for_user`。
- `crates/mc-repos/src/user.rs`：CRUD + `get_by_email` + `upsert_by_email`
  （`ON CONFLICT (email)` 幂等 upsert）。
- 错误映射：`RowNotFound → RepoError::NotFound`、unique 约束（23505）→ `RepoError::Conflict`。

## 测试

```bash
# 需要 Postgres；schema 由 mc_db::Migrator 幂等迁移
export MULTICA_TEST_DATABASE_URL='postgres://.../multica_m1a'

# Repo 单测（本 sub-issue 新增的 DB 测试都在其中）
cargo test --package mc-repos --lib
#   workspace::tests::{db_create_and_get_roundtrip, db_unique_slug_conflict,
#                      db_list_for_user_filters_by_membership,
#                      map_sqlx_err_row_not_found, filter_default_is_latest_only,
#                      role_to_str_canonical}
#   member::tests::{db_add_list_remove, db_unique_member_conflict,
#                   db_role_check_admin_can_grant_owner_only_owner_can_change,
#                   parse_role_known_values, filter_default_shape, new_member_constructs}
#   user::tests::{db_upsert_by_email_is_idempotent, db_get_by_email_returns_none_on_missing,
#                 db_update_me_partial_fields, filter_default_shape, new_user_construction,
#                 user_update_timezone_clear_marker}

# HTTP 中间件 + handler 单测
cargo test --package mc-http --lib

# 端到端：POST /api/workspaces → GET → members → /api/me + 401/404/403/leave/delete 矩阵
cargo test --package mc-http --test smoke
#   tests/smoke.rs::workspace_member_http_e2e（无 MULTICA_TEST_DATABASE_URL 时自动跳过）
```

注意：仓库根 `tests/` 属于虚拟 workspace 根（根 `Cargo.toml` 没有 `[package]`），
默认不会被 cargo 编译；本切片在 `crates/mc-http/Cargo.toml` 里以
`[[test]] path = "../../tests/smoke.rs"` 把它挂成 mc-http 的集成测试 target，
使该文件真正可编译可运行（M0 脚手架遗留问题）。

DB 测试的 slug / 邮箱每次运行唯一（workspace 软删后 slug 仍占用唯一约束），
在持久 Postgres 上可重复执行。

## 未覆盖 / 差异（→ sub-issue B/C 或 M2）

1. **DELETE workspace 的重型事务**：上游 `DeleteWorkspace`（`workspace.go:1059+`）
   是单事务 teardown —— workspace 行 `FOR UPDATE`、chat_session 行锁、全局
   advisory lock 4246 + `workspaceDeleteLockTimeout` 10s fence（`workspace.go:750+`）、
   cascade 后的对象 sweep、seat capacity 结算、事件发布与 daemon 失效通知。
   M1-A 仅做软删（`archived_at = now()`），完整 hard-delete 推到 M2。
2. **创建 workspace 不 seed issue statuses**：上游在同事务 `issuestatus.Ensure`
   （MUL-6243，7 个内置状态）；M1-A 的 schema/服务未就绪，推到 M1-D/M2。
   另外 workspace + owner member 两步非事务，member 失败时软删 workspace 作补偿。
3. **响应字段子集**：上游 `WorkspaceResponse` 还有 `context` / `repos` /
   `issue_prefix`，M0 schema 无这三列；`issue_prefix` 默认值
   （`defaultIssuePrefixFromSlug`，MUL-6050）随加列一起补。
4. **PUT 别名**：上游 admin 组同时注册 `PUT /api/workspaces/{id}`；M1-A 仅 PATCH
   （issue 路由表只要求 PATCH）。
5. **leave 与上游的 owner 语义差异**：上游允许"多 owner 时 owner 可以离开"
   （仅 `countOwners <= 1` 时 400）；M1-A 按本 issue 规格 owner 一律 403。
6. **update_me 校验子集**：timezone 做 IANA 校验（chrono-tz）、
   `profile_description` ≤ 2000 字符（MUL-2406）、name 非空；
   language 白名单（`supportedLanguages`：en/zh-Hans/ko/ja/fr）与
   `avatar_url` 校验（`acceptAvatarURL`）未做。
7. **`GET /api/me` 的 memberships 是 multica-rs 扩展**：上游 `UserResponse`
   不含 memberships；本实现按 issue 要求附加 `memberships[]`（user 字段保持上游键名）。
8. **未实现的相邻路由**：`POST /api/workspaces/{id}/members`（邀请创建
   `CreateInvitation`）、`PATCH/DELETE .../members/{memberId}`（角色升降 /
   移除，含"至少一个 owner"护栏）→ sub-issue C；`GET .../invitations` 同。
9. **实时事件 / daemon 通知 / membership cache 失效**（`EventWorkspaceUpdated`、
   `EventMemberRemoved`、`notifyDaemonWorkspacesChanged`）→ M2+。
10. **session 真实颁发**：占位实现，sub-issue B 落地 `/auth/verify-code` 后
    收紧为 store-only 解析（删除 UUID 直通通道）。

## 基线编译修复（超出本 sub-issue 文件清单，已尽量最小化）

M0 合入时从未整体编译过（sandbox 无 cargo），本切片在交付证据要求下修复了以下
阻塞项，全部为最小改动（缺依赖补依赖、缺 import 补 import、测试修 FK）：

- 缺依赖：`mc-errors`（anyhow）、`mc-config`（dirs）、`mc-http`（sqlx /
  parking_lot / chrono / uuid / chrono-tz / mc-repos + dev：mc-config、tower 0.4）、
  `mc-auth`（mc-secrets）、`mc-migrate` / `mc-plugin-protocol`（tokio）、
  `mc-secrets` dev（tempfile）。
- 编译错误：`mc-storage`（缺 `use async_trait::async_trait`）、`mc-auth`
  （`crate::store` 不存在 → 改用 `mc_secrets::{SecretsStore, InMemorySecretsStore}`）、
  `mc-authz`（glob 导入歧义 + `System` 分支缺失）、`mc-realtime`
  （`EventBus: !Default` → 手写 `Default`）、`mc-http::state`
  （`Secrets::new` 双层 Arc）、`mc-server`
  （`middleware::apply_default_middleware` re-export、`AdapterRegistryStub::register(16)`
  类型不匹配、`router()` 签名贯穿）、`mc-http::middleware`
  （`apply_default` 泛型化以接受 `Router<Arc<AppState>>`）。
- 测试错误：`mc-repos` member/user/workspace 测试（trait 未入作用域、
  `WorkspaceMember` 导入路径、`u32→i64`、member FK 需真实 user、slug 不幂等）、
  `mc-db`（纯注释残留被算作语句）、`mc-telemetry`（`redact_str` JSON 风格解析）。
- 路由冲突：axum 0.7 同 path+method 重复注册会 panic，`mount.rs::router()` 中
  `/api/workspaces` 的 M0 占位路由（含 `{id}` 字面量占位）由本切片的真实路由替换
  ——该文件顶部注释本就标注"M1+ 各 sub-issue 用真实 handler 替换"。

`cargo build --workspace`、`cargo test --workspace`、
`cargo clippy --workspace --all-targets`（0 error）、`cargo fmt`（本切片文件）均通过；
`Cargo.lock` 按约定不入库（M1-D 统一提交）。
