# M1-INVITATION — workspace invitation 路由 + DB repo

对应 issue：`LUM-1344`（M1 sub-issue C）

## 范围

实现 multica server `internal/handler/invitation.go` 的对外行为：

1. workspace admin 邀请某个 email 加入 workspace
2. 收件人通过 token 接受 / 拒绝
3. admin 撤销未使用的邀请
4. 速率限制（单 workspace 1h 内 N 条）

不在范围内（其他 sub-issue 处理）：
- workspace / member CRUD → M1 sub-issue A
- auth flow（session / cookie）→ M1 sub-issue B
- SMTP 发送邀请邮件 → 用 `tracing::warn!` 占位

## 路由清单

> 实现说明：axum 0.7（matchit 0.7）的路径参数语法是 `:id`，不是 `{id}`（那是
> axum 0.8）。下表按 OpenAPI 惯例写 `{id}`，实际注册代码（`routes/invitations.rs`）
> 用 `:id`——曾因用 `{id}` 导致全部参数化路由 404，e2e 测试抓住了这个问题。

| Method | Path | Handler | 鉴权 | DB 写 |
| --- | --- | --- | --- | --- |
| GET | `/api/workspaces/{id}/invitations` | `list_workspace_invitations` | workspace member | R |
| POST | `/api/workspaces/{id}/invitations` | `create_invitation` | workspace admin / owner | W |
| POST | `/api/workspaces/{id}/members` | `create_invitation`（alias，见兼容决策） | workspace admin / owner | W |
| DELETE | `/api/workspaces/{id}/invitations/{invitationId}` | `revoke_invitation` | workspace admin / owner | W |
| GET | `/api/invitations` | `list_my_invitations` | authenticated | R |
| GET | `/api/invitations/{id}` | `get_my_invitation` | authenticated（仅收件人） | R |
| POST | `/api/invitations/{id}/accept` | `accept_invitation` | authenticated | W (txn) |
| POST | `/api/invitations/{id}/decline` | `decline_invitation` | authenticated（仅收件人） | W |

## 鉴权

M1 阶段用最小化「dev-mode auth」：

- `X-Multica-User-Id` header 携带当前用户 UUID
- 缺失 header → 401 unauthorized
- workspace member / admin 校验走 `member` 表直查（不依赖 MemberRepo，避免与
  sub-issue A 撞车）
- sub-issue B 落地后，会替换为 session / cookie 提取；handler 的 `AuthUser`
  提取器会保留（`auth_user.rs`），但底层改为读 session

## Token 形态

- 32 字节 `rand::thread_rng` → base64url 无 padding → 约 43 字符
- 存 DB 用 `token TEXT UNIQUE` 列（与 multica schema 1:1）
- 接受邀请通过 token 定位；token 仅在创建时通过 DB 返回给调用方，
  不暴露给 API response body（API 走 id / email 定位）

## TTL

- 默认 7 天：`INVITATION_DEFAULT_TTL_DAYS = 7`
- 通过 `NewInvitation::ttl_secs` 覆盖
- 速率限制窗口：1 小时（写死）

## 速率限制

- 触发点：`create_invitation`
- 实现：`InvitationRepo::count_recent_in_workspace(ws, since_1h)`
- 阈值：`mc_config::AuthConfig::invitation_per_workspace_per_hour`（默认 50）
- env：`MULTICA_INVITATION_PER_WORKSPACE_PER_HOUR`
- 超限 → 429 + `retry_after_secs: 3600`

## 接受邀请（accept）的事务语义

```
BEGIN;
  SELECT … FROM workspace_invitation WHERE token = $1 FOR UPDATE;
  -- 检查 revoked_at / expires_at / accepted_at
  UPDATE workspace_invitation SET accepted_at = now() WHERE id = $…;
  INSERT INTO member (…) VALUES (…);   -- UNIQUE 冲突 → 幂等成功
COMMIT;
```

`AcceptOutcome::already_accepted = true` 表示调用方未实际写入 member（已被先前
接受过），客户端可以视为「已经是 member」继续后续逻辑。

## 与 sub-issue A 的兼容决策

`POST /api/workspaces/{id}/members` 的语义冲突（A：直接加已存在 user vs C：按 email
创建邀请），仲裁结论见 `docs/09-M1-INTEGRATION.md` §2.3：**上游 multica 把两者
合并为同一个 `CreateInvitation` handler，以 sub-issue C 的「创建邀请」语义为准**。

本 sub-issue C 的落地：

- `POST /api/workspaces/{id}/invitations` → 创建邀请（主路径，e2e 测试打它）
- `POST /api/workspaces/{id}/members` → 同一个 `create_invitation` handler（alias，
  满足规格路由表；上游兼容矩阵打这条）
- `GET /api/workspaces/{id}/members` 仍归 sub-issue A（当前 mount.rs 占位）；
  GET 与 POST 方法不同，merge 不冲突

⚠️ 如果 sub-issue A 也实现了 `POST /api/workspaces/{id}/members`（直接 add user
语义），合并时 axum 会对同 path+method 重复注册 panic——按 docs/09 决策删除 A 的
该 handler（或迁到非冲突路径），由 LUM-1342 master 在集成 PR 里落实并注明。

## 测试

单元（不依赖 DB）：
- token 格式：base64url，无歧义字符，长度 ≥ 43
- `InvitationRow` 状态机：active / expired / revoked / accepted 互斥
- DTO 转换 + role 解析

集成（需要 PG）：
- `create + get_by_token` 往返
- `accept → member row + accepted_at` + UNIQUE 冲突幂等
- `decline → revoked_at`
- `revoke by admin + already-revoked returns NotFound`
- `count_recent_in_workspace` 速率窗口

集成测试在 `crates/mc-repos/src/invitation.rs::integration_tests`，通过
`MULTICA_TEST_DATABASE_URL` env 触发，无 PG 时静默 skip。

本地验证（2026-09-22，PG 16 + `0001_init.up.sql`）：

- `cargo build --workspace` ✅
- `cargo test --workspace` ✅（exit 0）
- `cargo test -p mc-repos -- --ignored` → 5/5 ✅
- `cargo test -p mc-http --features test-util --test invitations -- --ignored` → 3/3 ✅
  （admin invite → list、accept → member、速率限制 429）
- `cargo test -p mc-http --features test-util --test pats` → 3/3 ✅

## 配置项

```rust
mc_config::AuthConfig {
    invitation_per_workspace_per_hour: u32,  // 默认 50
}
```

env：`MULTICA_INVITATION_PER_WORKSPACE_PER_HOUR=50`
