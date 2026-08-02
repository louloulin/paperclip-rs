# paperclip-rs Rust 改造执行计划

> 日期：2026-08-03 · 状态：Phase A 框架已就绪，部分模块真实化进行中

## 一、总体进度评估

| 层次 | Crate 数 | 已实现 | 占比 |
|---|---|---|---|
| 核心层 | 5 | 5 | 100% |
| 领域层 | 5 | 5 | 100% |
| 数据层 | 4 | 4 | 100% |
| 服务层 | 5 | 4 | 80% |
| 适配器 | 12 | 1 | 8% |
| 插件 | 2 | 0 | 0% |
| 辅助 | 5 | 2 | 40% |
| 应用 | 2 | 1 | 50% |
| **总计** | **38** | **22** | **58%** |

## 二、当前已完成（22/38 crates）

| Crate | 状态 | 说明 |
|---|---|---|
| pc-errors | ✅ 完成 | 统一错误类型，ApiError 到 HTTP 状态码映射 |
| pc-telemetry | ✅ 完成 | tracing + JSON 日志 + 启动横幅 |
| pc-config | ✅ 完成 | 环境变量加载，RunMode |
| pc-core | ✅ 完成 | Actor + ActorRegistry + DomainMessage + Id/Timestamp/Money |
| pc-db | ✅ 完成 | sqlx + 109 张表 DDL + 嵌入式迁移 |
| pc-auth | ✅ 完成 | session/cookie/Bearer token 认证 |
| pc-authz | ✅ 完成 | 角色权限矩阵 |
| pc-repos | ✅ 完成 | 29 个子模块，SQL 与 schema 一一对应 |
| pc-realtime | ✅ 完成 | broadcast channel + LiveEvent |
| pc-activity | ✅ 完成 | activity_events 写入 |
| pc-http | ✅ 框架 | 56 个路由模块全部注册，40+ 已真实 SQL |
| pc-ws | ✅ 完成 | WebSocket live-events 推送 |
| pc-heartbeat | ✅ 完成 | HeartbeatActor + 状态机 + kameo actor |
| pc-adapter-api | ✅ 完成 | Adapter trait + Command/Response DTO |
| pc-adapter-process | ✅ 完成 | tokio::process 抽象 |
| pc-adapter-codex-local | ✅ 完成 | Codex CLI 适配器 |
| pc-server | ✅ 完成 | main.rs composition root + graceful shutdown |
| pc-storage | ✅ 完成 | local-disk provider |
| pc-backup | ✅ 完成 | 数据库备份 |
| pc-openapi | ✅ 完成 | OpenAPI 3.1 自动生成 |
| pc-feature-flags | ✅ 完成 | 功能开关 |
| pc-doc-anchors | ✅ 完成 | 文档锚点 |
| pc-migrate-smoke | ✅ 完成 | 迁移烟雾测试 |

## 三、待实现 / 待真实化（16/38 crates）

### Phase B — 适配器补齐（10 crates）

| Crate | 说明 | 难度 | 预估工时 |
|---|---|---|---|
| pc-adapter-claude-local | Claude CLI 适配器 | 中 | 3d |
| pc-adapter-cursor-cloud | Cursor Cloud 适配器 | 中 | 3d |
| pc-adapter-cursor-local | Cursor CLI 适配器 | 中 | 3d |
| pc-adapter-gemini-local | Gemini CLI 适配器 | 中 | 3d |
| pc-adapter-grok-local | Grok CLI 适配器 | 中 | 3d |
| pc-adapter-hermes | Hermes 适配器 | 高 | 5d |
| pc-adapter-hermes-gateway | Hermes Gateway 适配器 | 高 | 5d |
| pc-adapter-openclaw-gateway | OpenClaw Gateway 适配器 | 高 | 5d |
| pc-adapter-opencode-local | OpenCode 适配器 | 中 | 3d |
| pc-adapter-pi-local | Pi 适配器 | 中 | 3d |

### Phase C — 插件系统（2 crates）

| Crate | 说明 | 难度 | 预估工时 |
|---|---|---|---|
| pc-plugin-protocol | JSON-RPC 协议 schema | 中 | 3d |
| pc-plugin-host | Worker 池 + 调度 + 事件桥接 + 数据库桥接 | 高 | 10d |

### Phase D — 路由真实化收尾

| 模块 | 当前状态 | 待实现 | 预估工时 |
|---|---|---|---|
| tool_access.rs | items=[] | 6 个真实 SQL 端点 + OAuth | 5d |
| secrets.rs | 部分真实 | rotate/usage/access-events/patch + pc-secrets crate | 5d |
| access.rs | 5 个 items=[] | 真实 SQL 读取 | 3d |
| board_chat.rs | 501 | 真实聊天房间 + 消息 CRUD | 5d |
| execution_workspaces.rs | 框架 | workspace 生命周期 | 5d |
| built_in_agents.rs | 框架 | 内置 agent 模板 | 2d |
| llms.rs | 框架 | LLM 配置 + 代理 | 2d |

### Phase E — CLI（1 crate）

| Crate | 说明 | 难度 | 预估工时 |
|---|---|---|---|
| paperclip-cli | 20+ 子命令 | 高 | 10d |

### Phase F — 辅助补齐

| Crate | 说明 | 难度 | 预估工时 |
|---|---|---|---|
| pc-secrets | aes-gcm 本地加密 provider | 中 | 5d |

## 四、基于 Actor 的架构图（更新版）

```
┌──────────────────────────────────────────────────────────────────────┐
│                     paperclip-rs (Cargo workspace)                   │
│                                                                      │
│   apps/                                                              │
│   ├── pc-server  ← ActorRegistry 在 composition root 注入            │
│   └── pc-cli     ← 待实现                                            │
│                                                                      │
│   crates/                                                            │
│   ┌──────────────────────────────────────────────────────────────┐   │
│   │  pc-core: Actor + ActorRegistry + DomainMessage              │   │
│   │  底层: kameo 0.22 (ActorRef, Spawn, Message, UnboundedMailbox) │   │
│   └──────────────────────────────────────────────────────────────┘   │
│          │                                                            │
│          ▼                                                            │
│   ┌──────────────────────────────────────────────────────────────┐   │
│   │  ActorRegistry (Mutex<HashMap<ActorKey, RegisteredActor>>)   │   │
│   │                                                              │   │
│   │  ┌──────────────────────┐  ┌──────────────────────┐          │   │
│   │  │ HeartbeatActor       │  │ PluginWorkerActor    │          │   │
│   │  │ (per run)            │  │ (per plugin)         │          │   │
│   │  │ kind="heartbeat_run" │  │ kind="plugin_worker" │          │   │
│   │  └──────────────────────┘  └──────────────────────┘          │   │
│   │  ┌──────────────────────┐  ┌──────────────────────┐          │   │
│   │  │ WsConnectionActor    │  │ AdapterBridgeActor   │          │   │
│   │  │ (per connection)     │  │ (per adapter config) │          │   │
│   │  │ kind="ws_conn"       │  │ kind="adapter"       │          │   │
│   │  └──────────────────────┘  └──────────────────────┘          │   │
│   │  ┌──────────────────────┐  ┌──────────────────────┐          │   │
│   │  │ RoutineRunActor      │  │ PipelineRunActor     │          │   │
│   │  │ (per run)            │  │ (per run)            │          │   │
│   │  │ kind="routine_run"   │  │ kind="pipeline_run"  │          │   │
│   │  └──────────────────────┘  └──────────────────────┘          │   │
│   │  ┌──────────────────────┐  ┌──────────────────────┐          │   │
│   │  │ ToolInvocationActor  │  │ KeyRotationActor     │          │   │
│   │  │ (per invocation)     │  │ (per secret)         │          │   │
│   │  │ kind="tool_invoke"   │  │ kind="key_rotation"  │          │   │
│   │  └──────────────────────┘  └──────────────────────┘          │   │
│   └──────────────────────────────────────────────────────────────┘   │
│          │                                                            │
│          ▼                                                            │
│   ┌──────────────────────────────────────────────────────────────┐   │
│   │  服务层                                                      │   │
│   │  pc-http (axum 56 路由)    pc-ws (WS live-events)            │   │
│   │  pc-heartbeat (kameo)      pc-realtime (broadcast)           │   │
│   └──────────────────────────────────────────────────────────────┘   │
│          │                                                            │
│          ▼                                                            │
│   ┌──────────────────────────────────────────────────────────────┐   │
│   │  数据层                                                      │   │
│   │  pc-repos (29 子模块)    pc-db (sqlx)    pc-activity          │   │
│   └──────────────────────────────────────────────────────────────┘   │
│          │                                                            │
│          ▼                                                            │
│   ┌──────────────────────────────────────────────────────────────┐   │
│   │  PostgreSQL (109 tables) + sqlx compile-time checked queries  │   │
│   └──────────────────────────────────────────────────────────────┘   │
└──────────────────────────────────────────────────────────────────────┘
```

## 五、Phase 级别执行计划

### Phase A（已完成）→ 核心框架
- [x] Cargo workspace + 17 crates 骨架
- [x] 109 张表 DDL 迁移
- [x] ActorRegistry + kameo 抽象
- [x] 56 路由全部注册
- [x] auth / authz 实现
- [x] heartbeat actor 实现
- [x] codex-local adapter 实现
- [x] realtime + WS 实现
- [x] cargo check / clippy / test 全部通过

### Phase B（进行中）→ 适配器补齐 + 路由真实化
- [ ] 10 个剩余适配器
- [ ] tool_access 真实化
- [ ] secrets 完整 CRUD + rotate + pc-secrets
- [ ] access.rs items 真实化
- [ ] board_chat / execution_workspaces / built_in_agents / llms

### Phase C → 插件系统
- [ ] pc-plugin-protocol
- [ ] pc-plugin-host

### Phase D → CLI
- [ ] paperclip-cli

### Phase E → 集成测试 + E2E
- [ ] 56 路由全覆盖
- [ ] 11 适配器覆盖
- [ ] 插件 SDK 互操作验证
- [ ] 性能基准

### Phase F → 部署
- [ ] musl 静态链接
- [ ] Docker 镜像
- [ ] 文档

## 六、关键设计决策回顾

| # | 决策 | 替代方案 | 理由 |
|---|---|---|---|
| D1 | kameo 0.22 + ActorRegistry facade | actix-web actor / 裸 tokio task | kameo 提供监管、邮箱、类型安全 ActorRef |
| D2 | ActorRegistry 用 Mutex<HashMap> | dashmap | 当前注册/查询频率低，Mutex 够用 |
| D3 | 109 表 SQL DDL 直接迁移 | 数据导出/导入 | schema 不变，零数据迁移 |
| D4 | sqlx compile-time SQL check | sea-orm / diesel | 保留 SQL 表达力 + 编译期安全 |
| D5 | 前端完全复用 | 重写 React | 前端 34.4 万行，重写成本太高 |
| D6 | 单进程多 actor | 多进程 | 当前规模不需要，后续可加 dist-kv |

## 七、风险与缓解

| 风险 | 概率 | 影响 | 缓解 |
|---|---|---|---|
| kameo 0.22 关键 bug | 低 | 高 | ActorRegistry facade 可替换底层 |
| 适配器协议不兼容 | 中 | 中 | 保持 stdio JSON-RPC 稳定 |
| 前端 API 契约遗漏 | 中 | 高 | E2E 测试覆盖 56 路由 |
| 性能回归 | 低 | 中 | criterion 基准 + wrk 压测 |
| 插件 SDK 不兼容 | 中 | 高 | 保留原 Node SDK 协议 |

## 八、下一步动作（本周）

1. 修复 `paperclip/ui/package.json` 中 lexical 虚假声明 → 恢复 Vite dev
2. 完成 tool_access 6 个端点真实化
3. 完成 secrets rotate / usage / access-events
4. 实现 pc-secrets crate (aes-gcm)
5. 补齐 access.rs 5 个 items=[]
6. cargo clippy + test 保持通过

