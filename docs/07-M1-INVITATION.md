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

| Method | Path | Handler | 鉴权 | DB 写 |
| --- | --- | --- | --- | --- |
| GET | `/api/workspaces/{id}/invitations` | `list_workspace_invitations` | workspace member | R |
| POST | `/api/workspaces/{id}/invitations` | `create_invitation` | workspace admin / owner | W |
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

`POST /api/workspaces/{id}/members` 在 sub-issue A 的规划里是「直接添加已存在 user」。
multica 上游是把这两条语义合并到 `CreateInvitation` handler。

本 sub-issue C 的实现：占用了 `POST /api/workspaces/{id}/invitations` 路径，但**没有
占用** `/api/workspaces/{id}/members`。两条路径并存：

- `POST /api/workspaces/{id}/invitations` → 创建邀请（本 sub-issue C 实现）
- `POST /api/workspaces/{id}/members` → 添加已存在 user（sub-issue A 实现，stub 占位）

`GET /api/workspaces/{id}/invitations` 与 `GET /api/workspaces/{id}/members` 也分别
处理「邀请」与「已加入成员」。merge 时不会冲突。

如果上游需要统一为单 endpoint，由 LUM-1342 master 在 merge 后协调重命名。

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

## 配置项

```rust
mc_config::AuthConfig {
    invitation_per_workspace_per_hour: u32,  // 默认 50
}
```

env：`MULTICA_INVITATION_PER_WORKSPACE_PER_HOUR=50`
