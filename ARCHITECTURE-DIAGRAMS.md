# paperclip-rs — 架构图

> 与 `openspec/changes/paperclip-rs-rewrite/{proposal,design,tasks}.md` 配套。所有图使用纯文本，可在任何编辑器查看。

---

## 图 1：当前 Paperclip 架构（Node/TS 单体）

```
┌──────────────────────────────────────────────────────────────────────────┐
│                         paperclip (pnpm monorepo)                        │
├──────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌────────────────────┐  ┌────────────────────┐  ┌────────────────────┐  │
│  │   ui/  (React)     │  │   cli/  (Node)     │  │  packages/plugins  │  │
│  │   1168 files       │  │   paperclipai      │  │  examples/sdks     │  │
│  │   ~344k LOC        │  │   20+ commands     │  │                    │  │
│  │   60 api clients   │  │                    │  │                    │  │
│  └──────────┬─────────┘  └──────────┬─────────┘  └─────────┬──────────┘  │
│             │ HTTP/WS               │ spawn                 │            │
│             ▼                       ▼                       ▼            │
│  ┌──────────────────────────────────────────────────────────────────┐    │
│  │                    server/  (Node + Express)                     │    │
│  │                                                                  │    │
│  │   56 routes  │  212 services  │  better-auth  │  ws  │  multer   │    │
│  │                                                                  │    │
│  │   ┌─────────────────────────────────────────────────────────┐    │    │
│  │   │  Heartbeat Engine  (services/heartbeat.ts)               │    │    │
│  │   │  Plugin Worker Mgr (services/plugin-worker-manager.ts)   │    │    │
│  │   │  Live Events WS    (realtime/live-events-ws.ts)          │    │    │
│  │   │  Activity Log      (services/activity-log.ts)            │    │    │
│  │   └─────────────────────────────────────────────────────────┘    │    │
│  │                                                                  │    │
│  │   ┌─────────────────────────────────────────────────────────┐    │    │
│  │   │  11 adapters (packages/adapters/*)                      │    │    │
│  │   │  claude-local / codex-local / cursor-{cloud,local} /    │    │    │
│  │   │  gemini-local / grok-local / hermes / openclaw-gateway / │    │    │
│  │   │  opencode-local / pi-local                               │    │    │
│  │   └─────────────────────────────────────────────────────────┘    │    │
│  └─────────────────────────┬────────────────────────────────────────┘    │
│                            │ Drizzle ORM                                │
│                            ▼                                            │
│              ┌──────────────────────────────┐                           │
│              │   PostgreSQL (109 tables)    │                           │
│              │   embedded-postgres (local)  │                           │
│              └──────────────────────────────┘                           │
│                                                                          │
│   ┌───────────────────────────────────────────────────────────────┐    │
│   │  packages/db        : Drizzle schemas + migrations            │    │
│   │  packages/shared    : cross-package types & zod schemas        │    │
│   │  packages/adapter-utils : runtime/target/billing/quotas       │    │
│   │  packages/skills-catalog : skill definitions                  │    │
│   └───────────────────────────────────────────────────────────────┘    │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
```

**量化**：server 760 文件/44 万行、ui 1168 文件/34 万行、109 张表、56 路由、212 服务、11 适配器、插件 SDK ~10 模块。

---

## 图 2：目标 paperclip-rs 工作区

```
┌──────────────────────────────────────────────────────────────────────────┐
│                          paperclip-rs/  (Cargo workspace)                │
├──────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│   ┌───────────────────────────────────────────────────────────────┐     │
│   │                       apps/ (binaries)                        │     │
│   │   ┌──────────────────┐   ┌──────────────────┐                │     │
│   │   │   pc-server      │   │    pc-cli        │                │     │
│   │   │  paperclip-server│   │  paperclipai     │                │     │
│   │   │  HTTP+WS+Embed PG│   │  20+ subcommands │                │     │
│   │   └────────┬─────────┘   └──────────────────┘                │     │
│   └────────────┼──────────────────────────────────────────────────┘     │
│                │                                                         │
│   ┌────────────▼────────────────────────────────────────────────────┐    │
│   │                       crates/ (libraries)                       │    │
│   │                                                                 │    │
│   │   ┌────────────┐  ┌────────────┐  ┌────────────┐  ┌──────────┐ │    │
│   │   │  pc-http   │  │  pc-ws     │  │ pc-heartbeat│  │pc-workflow│ │    │
│   │   │  axum      │  │  WS upgrade│  │ state machine│  │routine   │ │    │
│   │   │  routes    │  │  live-events│ │ tokio       │  │pipeline  │ │    │
│   │   └─────┬──────┘  └─────┬──────┘  └──────┬──────┘  └────┬─────┘ │    │
│   │         │               │                │             │       │    │
│   │         └───────────────┼────────────────┼─────────────┘       │    │
│   │                         ▼                ▼                     │    │
│   │   ┌─────────────────────────────────────────────────────────┐  │    │
│   │   │                     pc-repos                            │  │    │
│   │   │  company / agent / issue / case / project / approval /  │  │    │
│   │   │  decision / routine / pipeline / environment /          │  │    │
│   │   │  execution / heartbeat / plugin / auth / activity /     │  │    │
│   │   │  document / goal / folder / sidebar / inbox / summary / │  │    │
│   │   │  tool / smoke / settings / skill                       │  │    │
│   │   └──────────────────────┬──────────────────────────────────┘  │    │
│   │                          │                                     │    │
│   │         ┌────────────────┼────────────────┐                    │    │
│   │         ▼                ▼                ▼                    │    │
│   │   ┌──────────┐    ┌──────────┐     ┌──────────┐              │    │
│   │   │  pc-db   │    │ pc-core  │     │ pc-errors│              │    │
│   │   │  sqlx    │    │ domain   │     │ → HTTP   │              │    │
│   │   │  migrate │    │ types    │     │          │              │    │
│   │   └──────────┘    └──────────┘     └──────────┘              │    │
│   │                                                                │    │
│   │   ┌──────────┐  ┌──────────┐  ┌──────────────────────────┐   │    │
│   │   │ pc-auth  │  │ pc-authz │  │ pc-storage / pc-secrets   │   │    │
│   │   │ session  │  │ Policy   │  │ local/S3 / local-enc/AWS │   │    │
│   │   └──────────┘  └──────────┘  └──────────────────────────┘   │    │
│   │                                                                │    │
│   │   ┌────────────────────────────────────────────────────────┐  │    │
│   │   │   pc-adapter-api  +  11× pc-adapter-{name}              │  │    │
│   │   └────────────────────────────────────────────────────────┘  │    │
│   │                                                                │    │
│   │   ┌────────────────────────────────────────────────────────┐  │    │
│   │   │   pc-plugin-host  +  pc-plugin-protocol                 │  │    │
│   │   └────────────────────────────────────────────────────────┘  │    │
│   │                                                                │    │
│   │   ┌────────────────────────────────────────────────────────┐  │    │
│   │   │   pc-realtime / pc-activity / pc-feature-flags /        │  │    │
│   │   │   pc-doc-anchors / pc-backup / pc-config / pc-telemetry│  │    │
│   │   └────────────────────────────────────────────────────────┘  │    │
│   └────────────────────────────────────────────────────────────────┘    │
│                                                                          │
└──────────────────────────────────────────────────────────────────────────┘
```

---

## 图 3（更新版）：Crate 依赖图 — 实际实现状态 (2026-08-03)

```
┌──────────────────────────────────────────────────────────────────────┐
│                     paperclip-rs/  (Cargo workspace)                 │
│                                                                      │
│   apps/                                                              │
│   ┌──────────────────────────────────────────────────────────────┐   │
│   │                                                              │   │
│   │   pc-server (✅ 已完成)         pc-cli (⏳ 待实现)             │   │
│   │   ┌──────────────────┐         ┌──────────────────┐          │   │
│   │   │ main.rs          │         │ paperclipai      │          │   │
│   │   │ Axum Router      │         │ clap 20+ 子命令  │          │   │
│   │   │ 56 路由          │         │                  │          │   │
│   │   └─────────┬────────┘         └──────────────────┘          │   │
│   │             │                                                │   │
│   └─────────────┼────────────────────────────────────────────────┘   │
│                 │                                                    │
│   crates/                                                            │
│   ┌─────────────┼────────────────────────────────────────────────┐   │
│   │   ====== 服务层 ======                                        │   │
│   │   ┌─────────┴──────────┐  ┌──────────────────┐               │   │
│   │   │  pc-http (✅)      │  │  pc-ws (✅)      │               │   │
│   │   │  axum 56 routes    │──│  WS live-events  │               │   │
│   │   │  serde(camelCase)  │  │                  │               │   │
│   │   └─────────┬──────────┘  └────────┬─────────┘               │   │
│   │             │                      │                          │   │
│   │   ┌─────────┴──────────┐  ┌───────┴──────────┐               │   │
│   │   │  pc-heartbeat (✅) │  │  pc-realtime (✅) │               │   │
│   │   │  HeartbeatActor    │  │  broadcast chan   │               │   │
│   │   │  kameo actor       │  │  LiveEvent        │               │   │
│   │   └─────────┬──────────┘  └────────┬─────────┘               │   │
│   │             │                      │                          │   │
│   │   ====== 领域层 ======                                        │   │
│   │   ┌─────────┴──────────────────────┴──────────────────┐      │   │
│   │   │                  pc-core (✅)                      │      │   │
│   │   │  Actor + ActorRegistry + DomainMessage + kameo_api│      │   │
│   │   │  Id + Timestamp + Money                           │      │   │
│   │   │  底层: kameo 0.22 (ActorRef, Spawn, Message)     │      │   │
│   │   └─────────┬─────────────────────────────────────────┘      │   │
│   │             │                                                │   │
│   │   ====== 数据层 ======                                        │   │
│   │   ┌─────────┴──────────────────────────────────────────┐     │   │
│   │   │              pc-repos (✅)                          │     │   │
│   │   │  29 子模块: company/agent/issue/case/project/      │     │   │
│   │   │  approval/decision/routine/pipeline/environment/   │     │   │
│   │   │  execution/heartbeat/plugin/auth/activity/document/│     │   │
│   │   │  goal/folder/sidebar/inbox/summary/tool/skill/     │     │   │
│   │   │  settings/smoke/cost/membership/user_profile      │     │   │
│   │   └─────────┬──────────────────────────────────────────┘     │   │
│   │             │                                                │   │
│   │   ┌─────────┼──────────────────────────────────────────┐     │   │
│   │   │  pc-db (✅)    │  sqlx 0.8 + compile-time SQL check│     │   │
│   │   │                 │  109 tables DDL + 嵌入式迁移      │     │   │
│   │   └─────────────────┴──────────────────────────────────┘     │   │
│   │                                                                │   │
│   │   ====== 基础层 ======                                        │   │
│   │   ┌──────────────┐  ┌──────────────┐  ┌──────────────┐       │   │
│   │   │ pc-errors(✅)│  │pc-telemetry  │  │ pc-config(✅) │       │   │
│   │   │ ApiError     │  │  (✅)        │  │ 环境变量     │       │   │
│   │   │ → HTTP code  │  │ tracing/json  │  │ RunMode      │       │   │
│   │   └──────────────┘  └──────────────┘  └──────────────┘       │   │
│   │                                                                │   │
│   │   ====== 认证/授权 ======                                     │   │
│   │   ┌──────────────┐  ┌──────────────┐                         │   │
│   │   │ pc-auth (✅) │  │ pc-authz (✅)│                         │   │
│   │   │ session/cookie│  │ 权限矩阵    │                         │   │
│   │   └──────────────┘  └──────────────┘                         │   │
│   │                                                                │   │
│   │   ====== 适配器 (1/11 完成) ======                            │   │
│   │   ┌────────────────┐  ┌───────────────────────┐               │   │
│   │   │pc-adapter-api  │  │ pc-adapter-process(✅)│               │   │
│   │   │  (✅)          │  │  tokio::process       │               │   │
│   │   │ Adapter trait  │  └───────────────────────┘               │   │
│   │   └────────────────┘                                         │   │
│   │   ┌──────────────────────────────────────────┐               │   │
│   │   │ pc-adapter-codex-local (✅)              │               │   │
│   │   │  其余 10 个适配器 ⏳                      │               │   │
│   │   │  claude-local / cursor-{cloud,local} /   │               │   │
│   │   │  gemini-local / grok-local / hermes /    │               │   │
│   │   │  hermes-gateway / openclaw-gateway /     │               │   │
│   │   │  opencode-local / pi-local               │               │   │
│   │   └──────────────────────────────────────────┘               │   │
│   │                                                                │   │
│   │   ====== 插件 (待实现) ======                                 │   │
│   │   ┌────────────────┐  ┌──────────────────┐                   │   │
│   │   │pc-plugin-      │  │ pc-plugin-host   │                   │   │
│   │   │protocol (⏳)   │  │   (⏳)           │                   │   │
│   │   └────────────────┘  └──────────────────┘                   │   │
│   │                                                                │   │
│   │   ====== 辅助 ======                                          │   │
│   │   ✅ pc-storage   ✅ pc-backup    ✅ pc-openapi              │   │
│   │   ✅ pc-feature-flags   ✅ pc-doc-anchors                    │   │
│   │   ⏳ pc-secrets (aes-gcm)                                     │   │
│   └────────────────────────────────────────────────────────────┘   │
│                                                                    │
│   ✅ = 已完成并真实化     ⏳ = 待实现                               │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

## 图 3b：Actor 注册表与 kameo actor 实例（运行时视图）

```
ActorRegistry (Mutex<HashMap<ActorKey, RegisteredActor>>)

  ┌──────────────────────────────────────────────────────────┐
  │ ActorKey { kind: "heartbeat_run", id: run-1 }           │
  │   → HeartbeatActor { run_id, state, adapter, ... }     │
  │ ActorKey { kind: "heartbeat_run", id: run-2 }           │
  │   → HeartbeatActor { ... }                              │
  │ ActorKey { kind: "plugin_worker", id: plugin-x }        │
  │   → PluginWorkerActor { ... }                            │
  │ ActorKey { kind: "adapter", id: config-y }              │
  │   → AdapterBridgeActor { ... }                           │
  │ ActorKey { kind: "ws_conn", id: conn-z }                │
  │   → WsConnectionActor { ... }                            │
  │ ActorKey { kind: "tool_invoke", id: invoke-1 }          │
  │   → ToolInvocationActor { ... }                          │
  │ ActorKey { kind: "key_rotation", id: secret-1 }         │
  │   → KeyRotationActor { ... }                             │
  │ ActorKey { kind: "system", id: "root" }                 │
  │   → SystemActor (graceful shutdown root)                │
  └──────────────────────────────────────────────────────────┘
```


## 图 4：核心数据流 — 心跳 → 适配器 → live-events → UI

```
┌──────────────────┐  cron / monitor_next_check_at
│  pc-heartbeat    │ ◀────────────────────────────────────────┐
│  (state machine) │                                          │
└────────┬─────────┘                                          │
         │ pick runnable                                      │
         ▼                                                    │
┌──────────────────┐  acquire run lock                        │
│  pc-repos        │                                          │
│  heartbeat_runs  │                                          │
└────────┬─────────┘                                          │
         │ spawn subprocess                                   │
         ▼                                                    │
┌──────────────────────────────────────────────────────────┐  │
│   pc-adapter-claude-local (host)                          │  │
│   ┌────────────────────────────────────────────────────┐  │  │
│   │ JSON-RPC over stdio                                 │  │  │
│   │                                                    │  │  │
│   │   invoke(model, prompt, tools, ctx) ──▶ worker     │  │  │
│   │   ◀── stream { assistant_text, tool_call, usage }  │  │  │
│   └────────────────────────────────────────────────────┘  │  │
└────────────────────────┬─────────────────────────────────┘  │
                         │ stream events                      │
                         ▼                                    │
┌──────────────────────────────────────────────────────────┐  │
│   pc-realtime  (tokio::sync::broadcast)                   │  │
│   - persist to heartbeat_run_events (via pc-repos)        │  │
│   - broadcast LiveEvent { company_id, kind, payload }     │  │
└────────────────────────┬─────────────────────────────────┘  │
                         │                                    │
                         ▼                                    │
┌──────────────────────────────────────────────────────────┐  │
│   pc-ws  (axum WebSocket upgrade on GET /live-events)     │  │
│   - subscribe(company_id)                                 │  │
│   - send(event)                                           │  │
└────────────────────────┬─────────────────────────────────┘  │
                         │                                    │
                         ▼                                    │
┌──────────────────────────────────────────────────────────┐  │
│   paperclip/ui  (React, 复用, 完全不动)                    │──┘
│   - LiveUpdatesProvider.context                           │
│   - heartbeat UI / activity feed / 实时消息
└──────────────────────────────────────────────────────────┘
```

并行持久化：心跳每完成一个阶段都写入 `heartbeat_runs` / `heartbeat_run_events` / `activity_log` / `cost_events`，供 UI 的非实时视图查询。

---

## 图 5：插件 Worker 架构（保持与原 @paperclipai/plugin-sdk 兼容）

```
┌─────────────────────────────────────────────────────────────────┐
│                    pc-server (host)                              │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │  pc-plugin-host::WorkerPool                                │  │
│  │  ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐        │  │
│  │  │ worker1 │  │ worker2 │  │ worker3 │  │ worker4 │   ...  │  │
│  │  │ (subpro)│  │ (subpro)│  │ (subpro)│  │ (subpro)│        │  │
│  │  └────┬────┘  └────┬────┘  └────┬────┘  └────┬────┘        │  │
│  │       │ stdio JSON-RPC                                          │
│  └───────┼────────────┼────────────┼────────────┼────────────┘  │
│          │            │            │            │                │
└──────────┼────────────┼────────────┼────────────┼────────────────┘
           ▼            ▼            ▼            ▼
    ┌─────────┐  ┌─────────┐  ┌─────────┐  ┌─────────┐
    │ Node    │  │ Node    │  │ Python  │  │ Rust    │  ← 任意 runtime
    │ plugin  │  │ plugin  │  │ plugin  │  │ plugin  │     只要遵循
    │ (复用   │  │ (新写)  │  │ (新写)  │  │ (未来)  │     JSON-RPC
    │ 原SDK)  │  │         │  │         │  │         │
    └─────────┘  └─────────┘  └─────────┘  └─────────┘

协议（pc-plugin-protocol，host + worker 共享）：
  - methods: setup / onHealth / events.on / jobs.register / data.register / state.get / state.put / log / db.query
  - 消息类型：Request / Response / Event / Log
  - 能力声明：PaperclipPluginManifestV1
```

---

## 图 6：前端复用与契约冻结

```
┌─────────────────────────────────────────────────────────────────┐
│                                                                  │
│    paperclip/ui/   (React + Vite, 1168 文件 / 34 万行)           │
│                                                                  │
│    ┌──────────────────────────────────────────────────────┐      │
│    │   src/api/  (60 个客户端模块)                        │      │
│    │   - agents.ts / issues.ts / companies.ts / ...      │      │
│    │   - 使用 fetch + zod 校验                             │      │
│    └────────────────────────┬─────────────────────────────┘      │
│                             │                                    │
│                             │ HTTP / WebSocket                   │
│                             │ (契约冻结：路径、方法、schema、     │
│                             │  错误码、头部保持与原 server 一致)   │
│                             ▼                                    │
│    ┌──────────────────────────────────────────────────────┐      │
│    │   旧 server (Node)  ←──────────  回滚开关 ──────────  │      │
│    │   localhost:3101                                        │      │
│    └──────────────────────────────────────────────────────┘      │
│                                                                  │
│    ┌──────────────────────────────────────────────────────┐      │
│    │   ★ 新 paperclip-rs (Rust)  ←──────────  默认指向 ─── │      │
│    │   localhost:3100                                        │      │
│    └──────────────────────────────────────────────────────┘      │
│                                                                  │
│   通过 `VITE_API_BASE` 在 dev/prod 中切换：                      │
│     - 开发：指向 Rust server（更快冷启动）                       │
│     - 生产：Rust server 单二进制部署                             │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘

优势：
  - UI 代码 100% 复用，无任何 TypeScript/React 改动
  - 双栈过渡期可灰度切换
  - 任意时刻可回滚到 Node server（数据零迁移）
```

---

## 图 7：迁移路径（7 阶段）

```
Phase A (W1-2)   Phase B (W3-4)   Phase C (W5-8)   Phase D (W9-10)
┌──────────┐    ┌──────────┐    ┌──────────┐    ┌──────────┐
│ 骨架     │ ─▶ │ 仓储+认证 │ ─▶ │ 路由覆盖 │ ─▶ │ 实时+心跳│
│ - workspace│   │ - pc-repos│   │ - 56 路由 │   │ - pc-ws  │
│ - pc-core │    │ - pc-auth │   │ - pc-http │   │ - heartbeat│
│ - pc-db   │    │ - pc-authz│   │ - 集成测试│   │ - workflow│
│ - /health │    │ - 核心路由│   │           │   │           │
└──────────┘    └──────────┘    └──────────┘    └──────────┘
                                                        │
Phase E (W11-14)  Phase F (W15-16)  Phase G (W17)        │
┌──────────┐    ┌──────────┐    ┌──────────┐           │
│ 适配器+插件│ ─▶ │ CLI+可观测│ ─▶ │ 切流量   │ ◀─────────┘
│ - pc-adapter│   │ - pc-cli │    │ - UI 默认 │
│   -api     │   │ - pc-mig │    │   指向 Rust│
│ - 11 适配器│   │ - OTel   │    │ - 7天监控 │
│ - pc-plugin│   │ - 端到端 │    │ - 归档 Node│
│   -host   │    │ - 性能基准│   │           │
└──────────┘    └──────────┘    └──────────┘

每个阶段结束验证门禁（tasks §9）：
  - 单元/集成测试通过
  - clippy 无 warning
  - rustfmt 无 diff
  - 原 UI 冒烟通过
  - 端到端剧本通过
  - 文档更新
  - 性能基准记录
```

---

## 图 8：DB Schema 映射（109 张表 → pc-repos 子模块）

```
                ┌─────────────────────────────────────────┐
                │           PostgreSQL 109 表             │
                └────────────────┬────────────────────────┘
                                 │
        ┌────────────────────────┼─────────────────────────┐
        │                        │                         │
        ▼                        ▼                         ▼
  ┌──────────┐            ┌──────────┐             ┌──────────┐
  │ company  │            │  agent   │             │  issue   │
  │  5 tables│            │  6 tbls  │             │ 18 tbls  │
  └─────┬────┘            └────┬─────┘             └────┬─────┘
        │                      │                       │
        └──────────────────────┼───────────────────────┘
                               │
                               ▼
                    ┌──────────────────────┐
                    │   pc-repos::* 子模块 │
                    │  每个子模块负责       │
                    │  一组聚合的 CRUD     │
                    └──────────────────────┘

子模块清单（共 25 个）：
  company / agent / issue / case / project / approval /
  decision / routine / pipeline / environment / execution /
  heartbeat / plugin / auth / activity / document / goal /
  folder / sidebar / inbox / summary / tool / smoke /
  settings / skill
```

---

## 图 9：部署形态（单二进制 + Docker + 多架构）

```
┌─────────────────────────────────────────────────────────────────┐
│                       部署产物                                   │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Linux x86_64    : paperclip-server-{ver}-x86_64-unknown-linux-gnu
│  Linux arm64     : paperclip-server-{ver}-aarch64-unknown-linux-gnu
│  Linux musl      : paperclip-server-{ver}-x86_64-unknown-linux-musl (静态)
│  macOS x86_64    : paperclip-server-{ver}-x86_64-apple-darwin
│  macOS arm64     : paperclip-server-{ver}-aarch64-apple-darwin
│  Windows         : paperclip-server-{ver}-x86_64-pc-windows-msvc
│                                                                 │
│  CLI 同步发布    : paperclipai-{ver}-{target}.{tar.gz|zip}      │
│                                                                 │
│  Docker 镜像:                                                    │
│    ghcr.io/paperclipai/server:{ver}-musl                        │
│      - distroless + musl 静态链接                                │
│      - 体积 ~80-120 MB (vs 原 Node 镜像 ~500MB)                  │
│      - 多架构 manifest (amd64 + arm64)                          │
│                                                                 │
│  npm 包:                                                        │
│    @paperclipai/server  → 下载对应平台原生二进制                  │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## 图 10：可观测性栈

```
┌─────────────────────────────────────────────────────────────────┐
│                       应用层                                     │
│                                                                 │
│   pc-http middleware ──── tracing span (method, path, status)   │
│   pc-ws   ─────────────── tracing span (conn_id, events)         │
│   pc-heartbeat ─────────── tracing span (run_id, agent_id)       │
│   pc-repos ─────────────── tracing span (query, duration)       │
│   pc-adapter-* ─────────── tracing span (model, tokens)          │
│                                                                 │
└──────────────────────────┬──────────────────────────────────────┘
                           │
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  ▼
  ┌──────────┐       ┌──────────┐       ┌──────────────┐
  │ tracing  │       │ tracing  │       │   tracing    │
  │ console  │       │   OTLP   │       │  structured  │
  │ (JSON)   │       │ exporter │       │   logs       │
  │ 总是开启 │       │ 按需开启 │       │   总是开启   │
  └──────────┘       └──────────┘       └──────────────┘
        │                  │                  │
        ▼                  ▼                  ▼
   stdout             OTLP backend         stdout
   (k8s logs)        (Tempo/Jaeger)        (Loki/ES)
                          │
                          ▼
                    Prometheus metrics
                    (/metrics endpoint)
```

