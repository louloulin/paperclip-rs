# Paperclip-rs 复刻进度审计（2026-08-03，第二次会话）

## 当前三道门禁
- ✅ `cargo fmt --all`
- ✅ `cargo clippy --workspace --all-targets -- -D warnings`
- ✅ `cargo test --workspace` — 239 passed (68 suites)

## Workspace 实际状态
- **36 crates**（vs 计划 ~32 + 计划外 cli）
- 测试 239（pc-storage 12、pc-workflow 18、pc-activity 6、pc-openapi 4、pc-feature-flags 15 + 旧 184）

## 本会话关键突破

### 1. Server 端到端可启动 + 真实路由返回
- ✅ pc-server 二进制可启动并监听 127.0.0.1:3100
- ✅ 109 表 Drizzle SQL 迁移完整运行
- ✅ 21+ 业务路由真实返回 DB 数据（agents / issues / projects / approvals / decisions / pipelines / goals / cases / routines / folders / documents / costs/summary / dashboard 等）

### 2. UI 端到端联通
- ✅ paperclip/ui 源码完整 copy 到 paperclip-rs/ui（17M）
- ✅ paperclip/packages/adapters/* (11 个) + adapter-utils + shared + plugins + skills-catalog + teams-catalog 完整 copy 到 paperclip-rs/packages
- ✅ pnpm-workspace.yaml 配置 packages + adapters + ui
- ✅ Vite dev server 启动，5173 端口监听
- ✅ Vite proxy /api/* → :3100 全联通
- ✅ 21/21 业务路由通过 UI 代理返回 200 + 真实 JSON

### 3. 心跳 → 实时事件全链路
- ✅ HTTP POST /api/agents/{id}/heartbeat/invoke 真实创建 heartbeat_run
- ✅ WebSocket /api/live-events upgrade 成功 + welcome 消息
- ✅ 实时事件推送（heartbeat.run.started）resource_id 匹配

### 4. 插件 bootstrap
- ✅ pc-server main.rs 在 AppState 装配后插入 PluginRepo::list_filtered("ready") → PluginEntry 注册 → WorkerPool::spawn
- ✅ PaperclipPluginManifestV1 解析通过
- ✅ WorkerOptions (command/args/cwd/version) 正确构造

### 5. Lexical 真实根因修正
- ✅ 推翻之前"lexical 未在源码被引用"的错误结论（实际 4 个文件 import）
- ✅ 真实根因：pnpm hoisting child-link 缺失
- ✅ `pnpm install --no-frozen-lockfile --filter @paperclipai/ui` 修复
- ✅ docs/06-LEXICAL-REAL-ROOT-CAUSE.md 详细记录

## 修复的 Bug
| # | 文件 | 问题 |
|---|---|---|
| 1 | crates/pc-server/src/main.rs | 11 个 adapter 重复注册 → 启动崩溃 |
| 2 | crates/pc-http/src/routes/mod.rs | activity::router() 合并两次 → 路由重叠 |
| 3 | crates/pc-http/src/routes/activity.rs | POST /api/activity 与 GET /api/activity/list 路径冲突 |
| 4 | crates/pc-server/Cargo.toml | 缺失 pc-plugin-host / pc-plugin-protocol / uuid 依赖 |
| 5 | crates/pc-server/src/main.rs | plugin bootstrap 缺失 |
| 6 | crates/pc-http/src/routes/secrets.rs | SHA-256 placeholder 已替换为真实 hash |

## 仍未完成（NOT_IMPLEMENTED 端点）
- `crates/pc-http/src/routes/plugins.rs` 中 5 个 501 端点（bridge_data / bridge_action / plugin_data / plugin_action / bridge_stream）需要真实 plugin worker 进程
- `crates/pc-http/src/routes/status_cards.rs` 中 3 个端点（recompile / refresh / summary）
- `crates/pc-http/src/routes/tool_access.rs` 中 3 个 OAuth 流端点

## 真实复刻距离
- Node 端 760 源文件 / 44.4 万行 TS
- UI 端 1168 源文件 / 34.4 万行 TS
- 当前 Rust 5 万行 + UI 全部 copy，约 60% 真实复刻（核心后端骨架 + 完整 UI 已就位）

## 下一步建议
1. 实现 plugin worker 实际 stdio JSON-RPC 协议（bridge_data 等 5 个 501 端点）
2. 真实化 status_cards summary 写入
3. OAuth 流在 tool_access.rs 补全
4. 用原 Node server 与 Rust server 并行 A/B 字节级对比
5. 移除 paperclip/ 目录运行依赖，归档为只读快照
