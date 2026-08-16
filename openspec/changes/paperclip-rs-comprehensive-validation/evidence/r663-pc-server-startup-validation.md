# R663 - pc-server 二进制 build + 真实启动验证

## 目标
- 完成 `paperclip-server` 二进制在隔离 target 下的可编译
- 启动验证：DB 连接 + migrations + adapter 注册 + heartbeat 恢复 + HTTP 服务可用

## 实现

### 1. 二进制 build
- target: `target/debug/paperclip-server`（131 MB，dev profile）
- 编译时间：1m30s（含增量缓存命中）
- 启动命令（已验证）：
  ```bash
  PAPERCLIP_DATABASE_URL="postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos"
  PAPERCLIP_SERVER_PORT=3100
  RUST_LOG=info
  nohup ./target/debug/paperclip-server > .tmp/pc-server.log 2>&1 &
  ```

### 2. 启动序列实测（来自 .tmp/pc-server.log）
1. telemetry init — 162ms 完成所有 phase
2. DB connect — 18ms（attempt=1, pool max=16 min=1）
3. Migrations — 16ms（`drizzle` schema + `__drizzle_migrations` 已存在，跳过）
4. Adapter 注册 — 13ms（13 个 adapter 注册成功）
5. Heartbeat recovery — 15ms（recovered=11 deferred=0）
6. Storage provider — `local_disk` 注册到 `$HOME/.paperclip/storage`
7. Feature flags — 默认 2 个注册（`pc.ui.dense-mode` 等）
8. Plugin workers bootstrap — count=0
9. UI bundle 挂载 — `ui/dist/index.html` 存在，SPA fallback
10. Bind + listen — 0ms，host=127.0.0.1 port=3100
11. **总启动时间**：159ms

### 3. 真实 HTTP 验证
- `GET /api/health` → 200，`{"status":"ok","db.ok":true}`
- `GET /api/companies` → 200 + 17 条真实 companies 数据
- access_log 写入：`request_id=01a008a0-... client_ip=127.0.0.1 method=GET path=/api/companies status=200 duration_ms=2`
- Graceful shutdown：SIGTERM 触发后 7ms 完成所有 actor shutdown

### 4. 关键文件
- `apps/pc-server/src/main.rs`（869 行，启动序列完整）
- `apps/pc-server/Cargo.toml`（workspace member）
- 注：**不要新建 `crates/pc-server`**，会与 `apps/pc-server` 冲突

## 关键发现

### Auth Boundary 问题（R664 任务）
未认证请求 `/api/companies` 返回 200 而不是 401：
- `auth_layer` 注入 `AuthContext::anonymous()` 到 extensions
- handler 内部调用 `require_user_id` 时才校验
- 当前 `/api/companies` 路由缺少 `require_user_id` 前置检查
- 修复方向：在 routes::mod.rs 装配 router 时按 `/api/*` 子路径强制 require_auth

### 其它
1. UI dist 复用 Node bundle（1168 文件未改造），fallback_service 正确处理 SPA 路由
2. Hermes Gateway 默认走 stub transport（env 缺失 → 真实 HTTP/SSE 客户端未启用）
3. Cursor Cloud / OpenClaw 同样：env 缺失走 fake client

## 下一步
- R664: 修复 auth boundary + workspace-runtime 路由层接入
- 真实 curl 验证未认证返回 401
