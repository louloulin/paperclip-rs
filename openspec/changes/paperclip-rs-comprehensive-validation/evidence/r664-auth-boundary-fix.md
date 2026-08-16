# R664 - Auth Boundary 修复 + workspace-runtime 接入准备

## 目标

修复 R663 发现的 auth boundary 问题：
- `authenticated` 模式下，未认证请求 `/api/companies` 返回 200 而不是 403
- 原因：`auth_layer` 总是注入 `AuthContext::anonymous()`，handler 自行决定是否需要 auth；
  当前多数 list/GET handler 没有调用 `require_user_id`，导致 anonymous 也通过
- 修复：对齐 Node `actorMiddleware` 的 `local_trusted` 自动注入 + `assertBoard` 强制 board 检查

## 实现

### 1. `crates/pc-http/src/middleware/auth.rs` 改动

新增两个 helper + middleware：

#### `is_local_trusted_mode()`
从 `PAPERCLIP_DEPLOYMENT_MODE` 环境变量检测部署模式：
- `local_trusted` / `local-trusted` → true
- 其它（含默认 `authenticated`）→ false

#### `local_board_auth_context()`
构造 `Actor::User { id: "local-board", is_instance_admin: true, ... }` with `ActorSource::LocalImplicit`，
完全对齐 Node `actorMiddleware` 在 `local_trusted` 模式下自动注入的等价值。

#### 修改 `auth_layer`
在 anonymous + local_trusted 时自动注入 local-board：
```rust
if !ctx.actor.is_authenticated() && is_local_trusted_mode() {
    ctx = local_board_auth_context();
}
```

#### 新增 `require_board_layer`
axum middleware，拒绝 anonymous，返回 403 forbidden：
- 镜像 Node `routes/authz.ts::assertBoard`
- 公开路径（`/health`、`/api/health`、`/api`、`/api/`）通过 `is_public_auth_path` 豁免
- 其它路径要求 actor 是 `User` / `Agent` / `System`

### 2. `apps/pc-server/src/main.rs` 改动

调整 middleware 装配顺序：
```rust
.route_layer(from_fn(require_board_layer))   // 源码先加 - 内层执行（晚）
.route_layer(from_fn_with_state(state, auth_layer))  // 源码后加 - 外层执行（早）
.route_layer(from_fn_with_state(state, csrf_layer))
```

执行顺序（外→内）：`board_mutation_guard → auth_layer → require_board_layer → csrf_layer → handler`

**关键**：require_board_layer 必须放在 auth_layer 之"内"，即源码中先 `.route_layer`，
因为 axum `.route_layer` 是后加的在外层。这样 auth_layer 先注入 AuthContext，
require_board_layer 再读取并校验。

### 3. 单元测试

`crates/pc-http/src/middleware/auth.rs::tests`：
- `require_auth_rejects_anonymous` (existing)
- `require_auth_accepts_user` (existing)
- `require_company_access_blocks_cross_tenant_agent` (existing)
- `instance_admin_can_access_any_company` (existing)
- `membership_user_can_access_their_company` (existing)
- `local_trusted_mode_detected_from_env` (NEW) — 环境变量检测
- `require_board_layer_rejects_anonymous` (NEW) — Router+oneshot 测试
- `require_board_layer_accepts_user` (NEW)
- `require_board_layer_accepts_agent` (NEW)
- `public_auth_path_whitelist` (NEW) — 白名单单元测试

测试结果：
```
test middleware::auth::tests::local_trusted_mode_detected_from_env ... ok
test middleware::auth::tests::require_board_layer_accepts_agent ... ok
test middleware::auth::tests::require_board_layer_accepts_user ... ok
test middleware::auth::tests::require_board_layer_rejects_anonymous ... ok
test middleware::auth::tests::public_auth_path_whitelist ... ok

test result: ok. 10 passed; 0 failed; 0 ignored
```

### 4. 全套无 regression 验证

- `pc-http` lib tests：**478 passed; 0 failed**
- `pc-auth` lib tests：80 passed; 0 failed
- `pc-routines` lib tests：41 passed; 0 failed

## 真实 curl 验证

### AUTHENTICATED 模式（`PAPERCLIP_DEPLOYMENT_MODE=authenticated`，默认）

```
GET /api/health       → 200 (公开路径豁免)
GET /api/companies    → 403 {"error":"forbidden: Board access required"}
GET /api/agents       → 403 {"error":"forbidden: Board access required"}
GET /api/projects     → 403 {"error":"forbidden: Board access required"}
```

### LOCAL_TRUSTED 模式（`PAPERCLIP_DEPLOYMENT_MODE=local_trusted`）

```
GET /api/health       → 200 (公开路径豁免)
GET /api/companies    → 200 + 17 条真实 companies 数据（local-board 自动注入）
```

## 关键文件改动

- `crates/pc-http/src/middleware/auth.rs` — 新增 `is_local_trusted_mode` + `local_board_auth_context`
  + 修改 `auth_layer`（local_trusted 注入）+ 新增 `require_board_layer` + `is_public_auth_path`
- `apps/pc-server/src/main.rs` — 调整 require_board_layer 在 auth_layer 之内的装配顺序

## 后续

- R665: workspace-realization / workspace-runtime-read-model route 接入
- R666: issue-* 子服务补齐（approvals / visibility / recovery-actions）
- R667: 集成 e2e + Node 兼容验收（终验）
