# paperclip-rs 项目执行计划

> 版本：v1.0 · 日期：2026-08-02 · 状态：待评审 · 配套文档：proposal.md / design.md / tasks.md / specs/ / ARCHITECTURE-DIAGRAMS.md / MODULE-MAPPING.md

---

## 一、执行摘要（Executive Summary）

**目标**：将 Paperclip 后端从 Node.js + TypeScript 单体（760 源文件 / 44.4 万行）重写为 Rust 多 crate 工作区（~30 crates + 2 binaries），前端 React UI（1168 源文件 / 34.4 万行）**完全复用**，HTTP/WS 契约冻结、数据库 schema 兼容、行为等价。

**预期收益**：
- HTTP 路由延迟 ↓ 30–60%（消除 V8 启动开销 + 强类型 async）
- 常驻内存 ↓ 40–60%（无 V8 heap + 无中间件栈）
- 冷启动 < 200ms（vs Node 3–5s）
- Docker 镜像 ~80–120MB（vs 当前 ~500MB）
- 部署形态：单一 musl 静态链接二进制 / macOS / Windows 多目标

**周期估算**：约 17 周（4 个月），按 Phase A → G 顺序推进；任一阶段可灰度回滚到原 Node server（数据零迁移）。

**团队建议**：2–3 名 Rust 工程师 + 1 名前端/集成工程师；保留 1 名原 Node server 维护者做契约对照。

---

## 二、项目背景与动机

### 当前现状

Paperclip 仓库为 pnpm monorepo，包含 server（Node + Express）、ui（React + Vite）、cli（Node）、packages（adapters、db、shared、skills-catalog、plugins）等。后端单进程承担 HTTP API、WebSocket live-events、心跳调度、11 个适配器子进程编排、插件 Worker 池、嵌入式 PostgreSQL、备份恢复、密钥管理等职责。

### 痛点

| # | 痛点 | 影响 |
|---|---|---|
| 1 | 运行时性能与资源占用 | 400MB+ 内存、冷启 3–5s、CPU 密集路径（OpenAPI/zod/心跳轮询）开销大 |
| 2 | 类型与并发模型边界模糊 | async/await 难表达真实取消/超时/背压；adapter 编排缺编译期保证 |
| 3 | 部署与分发 | Node 运行时 + 原生模块（embedded-postgres、sharp、ssh2）跨平台构建脆弱 |
| 4 | 进程/任务模型分裂 | Node 单进程 vs 多 adapter 子进程 vs 插件 worker，缺乏统一抽象 |

### 重写价值

| 维度 | 当前 (Node) | 目标 (Rust) |
|---|---|---|
| 运行时 | V8 + Node 内置库 | tokio + Rust stdlib |
| 类型系统 | TypeScript（弱类型擦除） | Rust（零成本抽象、编译期保证） |
| 并发模型 | async/await + 事件循环 | tokio 多线程 + 显式取消/超时 |
| 部署 | 容器 + 原生模块 | musl 静态链接单二进制（多目标） |
| 进程编排 | child_process + stdio | tokio::process + trait 抽象 |
| 错误处理 | try/catch + 异步链断裂 | Result<T,E> + ? 操作符强制传播 |

---

## 三、目标架构概览

### 顶层布局

```
┌──────────────────────────────────────────────────────────────────┐
│                     paperclip-rs/  (Cargo workspace)             │
│                                                                  │
│   apps/                                                          │
│   ├── pc-server       : paperclip-server  (HTTP+WS+嵌入PG, 端口3100)
│   └── pc-cli          : paperclipai       (20+ 子命令)
│   pc-migrate          : paperclip-migrate (独立迁移工具)
│                                                                  │
│   crates/                                                        │
│   ├── 核心层    pc-errors / pc-telemetry / pc-config             │
│   ├── 领域层    pc-core / pc-db / pc-storage / pc-secrets /     │
│   │             pc-auth / pc-authz                               │
│   ├── 数据层    pc-repos (25 子模块) / pc-realtime / pc-activity │
│   ├── 服务层    pc-http / pc-ws / pc-heartbeat / pc-workflow     │
│   ├── 适配器    pc-adapter-api + 11 个 pc-adapter-{name}          │
│   ├── 插件      pc-plugin-protocol / pc-plugin-host              │
│   └── 辅助      pc-feature-flags / pc-doc-anchors /              │
│                 pc-backup / pc-openapi                           │
└──────────────────────────────────────────────────────────────────┘
```

### 关键设计决策

| # | 决策 | 替代 | 选定理由 |
|---|---|---|---|
| D1 | tokio 多线程 + spawn_blocking | async-std / smol | 与 axum/sqlx/hyper 生态完全对齐 |
| D2 | axum 0.7 + tower | actix-web / warp | 类型安全、hyper 1.x、社区活跃 |
| D3 | sqlx 0.8（编译期 SQL 校验） | sea-orm / diesel | 保留 SQL 表达力、编译期安全 |
| D4 | 109 张表 SQL DDL 直接迁移 | 数据导出/导入 | schema 不变，零数据迁移 |
| D5 | tokio::broadcast + trait | 强制 Redis pubsub | 单节点够用，预留扩展 |
| D6 | 适配器 worker 仍是子进程 | 重写 worker | 适配器作者体验不变 |
| D7 | 插件 SDK 协议稳定 | 重写协议 | 插件生态不破坏 |
| D8 | 自研 pc-auth 复刻 better-auth | 强依赖 Node 库 | 行为复刻可控 |
| D9 | 嵌入式 PG + 外部 PG 自动降级 | 强制外部 PG | 跨平台兼容 |
| D10 | tracing + JSON + 按需 OTLP | pino 等价字段 | 与可观测后端对齐 |
| D11 | 前端契约冻结 | UI 重写 | 不属于本期范围 |

### 数据流（核心链路）

```
  UI / CLI / cron
        │ POST /heartbeat
        ▼
  ┌─────────────┐
  │  pc-http    │   ← axum 路由 + 中间件栈
  └──────┬──────┘
         ▼
  ┌─────────────┐
  │ pc-heartbeat│   ← 状态机：PickRunnable → Finalize
  └──────┬──────┘
         ▼ spawn subprocess (JSON-RPC over stdio)
  ┌──────────────────────────┐
  │ pc-adapter-claude-local  │   ← 11 个内置适配器 host
  └──────┬───────────────────┘
         ▼ stream events
  ┌─────────────┐
  │ pc-realtime │   ← broadcast bus + 持久化到 DB
  └──────┬──────┘
         ▼
  ┌─────────────┐
  │   pc-ws     │   ← WebSocket /live-events
  └──────┬──────┘
         ▼
     UI (React, 复用)
```

---

## 四、主时间表（Master Timeline）

```
周次  1   2   3   4   5   6   7   8   9  10  11  12  13  14  15  16  17
     ├───┴───┤                       │                       │       ├─┤
     │ A 骨架│                       │                       │       │G│
     │       ├───┴───┬───┤           │                       │       ├─┤
     │       │ B 仓储│认证│           │                       │       │归│
     │       │       │   ├─┴─┬─┬─┬─┬─┤                       │       │档│
     │       │       │   │ C  │ │ │ │ │                       │       ├─┤
     │       │       │   │路由│ │ │ │ │                       │       │ │
     │       │       │   │覆盖│ │ │ │ │                       │       │ │
     │       │       │   ├─┬─┴─┴─┴─┴─┤                       │       │ │
     │       │       │   │ D  │实时+心跳│                       │       │ │
     │       │       │   │   ├───┬───┬─┴─┬─┬─┬─┬─┬─┤       │       │ │
     │       │       │   │   │ E 适配器+插件 │                       │ │
     │       │       │   │   │   ├───┬───┬─┴─┴─┴─┤       │       │ │
     │       │       │   │   │   │ F CLI+可观测性 │       │       │ │
     │       │       │   │   │   │   ├───┬───┬─┤       │       │ │
     │       │       │   │   │   │   │   │ G 切流量 │       │ │
     ├───────┴───────┴───┴───┴───┴───┴───┴───┴───────────────┴───────┤ │
     │ M1 骨架就绪 │ M2 仓储就绪 │ M3 路由就绪 │ M4 实时+心跳就绪 │ M5 适配器+插件 │ M6 CLI+可观测 │ M7 切流量完成 │
     └─────────────┴─────────────┴─────────────┴───────────────┴───────────────┴─────────────┘

     ★ 持续：单元/集成测试、性能基准、文档更新、原 UI 冒烟
```

### 关键里程碑（Milestones）

| 里程碑 | 触发 | 验收物 |
|---|---|---|
| **M1 骨架就绪** | Phase A 结束（2 周） | `paperclip-rs/` workspace + pc-server 二进制 + `GET /health` 返回 200 + 109 表迁移通过 |
| **M2 仓储就绪** | Phase B 结束（4 周） | 25 个 repo 子模块 + auth/authz + 核心 10 路由 + 双栈切换可工作 |
| **M3 路由就绪** | Phase C 结束（8 周） | 56 个路由全部迁移 + OpenAPI 文档 + 集成测试通过 |
| **M4 实时+心跳就绪** | Phase D 结束（10 周） | WebSocket live-events + heartbeat 状态机 + workflow |
| **M5 适配器+插件就绪** | Phase E 结束（14 周） | 11 个内置适配器 + 插件 Worker 池 + 端到端心跳→UI 推送 |
| **M6 CLI+可观测就绪** | Phase F 结束（16 周） | pc-cli 全部子命令 + OTel + 性能基准达标 |
| **M7 切流量完成** | Phase G 结束（17 周） | UI 默认指向 Rust server + Node server 归档 |

---

## 五、阶段计划（按 Phase 展开）

### Phase A — 工作区骨架与基础设施（周 1–2）

**目标**：建立可编译、可运行、可测试的 Rust workspace 骨架。

**关键任务**：
- Cargo workspace 根 + rust-toolchain + .cargo/config.toml
- GitHub Actions CI（fmt + clippy + test + musl + glibc 构建）
- 基础 crate：`pc-errors` / `pc-telemetry` / `pc-config` / `pc-core`（最小实体）
- 数据库：109 表 SQL DDL 迁移文件 + sqlx::migrate 集成 + 嵌入式 PG 启动
- `pc-server` 二进制骨架：config → telemetry → db → migrate → `/health` → graceful shutdown

**交付物**：
- `cargo run -p pc-server` 启动并 `curl /health` 返回 200
- 109 表迁移在 fresh DB 上完整通过
- CI 跑通 fmt + clippy + test

**负责人**：1 名 Rust 工程师（owner）+ 1 名 reviewer

---

### Phase B — 仓储层与认证授权（周 3–4）

**目标**：核心数据访问、认证、授权可用；首批 10 个核心路由上线。

**关键任务**：
- `pc-repos` 全部 25 个子模块（company / agent / issue / case / project / approval / decision / routine / pipeline / environment / execution / heartbeat / plugin / auth / activity / document / goal / folder / sidebar / inbox / summary / tool / smoke / settings / skill）
- `pc-auth`：session + cookie + CSRF + API key（复刻 better-auth 对外行为）
- `pc-authz`：Policy trait + 策略表（与 `services/authorization.ts` 等价）
- `pc-storage`：local-disk + s3 provider
- `pc-secrets`：local-encrypted + aws-sm provider
- 核心 10 路由：companies / agents / issues / projects / cases / approvals / decisions / routines / pipelines / environments

**双栈切换**：UI 通过 `VITE_API_BASE` 切换到 Rust server，与原 Node server 并行运行做对比。

**交付物**：
- 25 个 repo 子模块单元测试通过
- 10 个核心路由集成测试通过
- 双栈 A/B 测试报告（响应字节级一致）

**负责人**：2 名 Rust 工程师 + 1 名前端/集成工程师

---

### Phase C — HTTP 路由全覆盖（周 5–8）

**目标**：56 个路由模块逐一迁移，全部上线。

**关键任务**：
- 路由分组（4 批 × ~14 路由 / 批）：
  - **批 1（核心）**：companies / agents / issues / projects / cases / approvals / decisions / routines / pipelines / environments（Phase B 已开始，本阶段完成）
  - **批 2（工作流）**：execution-workspaces / goals / board-chat / file-resources / company-skill-policy / company-skills / company-import-paths
  - **批 3（协作）**：user-profiles / resource-memberships / sidebar-badges / sidebar-preferences / inbox-dismissals / inbox-agent-policy / invites / join-requests / board-chat（重复）
  - **批 4（集成）**：secrets / tool-access / tool-gateway / costs / activity / dashboard / attention / auth / authz / access
  - **批 5（平台）**：instance-settings / instance-database-backups / health / openapi / llms / org-chart-svg / plugin-ui-static / adapters / built-in-agents / plugins / assets / decision-training / document-annotations / environment-selection / folders / issue-tree-control / issues-checkout-wakeup / pipelines / projects / resource-memberships / routines / secrets / sidebar-badges / sidebar-preferences / smoke-lab / status-cards / summary-slots / teams-catalog / tool-access / tool-gateway / user-profiles / workspace-command-authz / workspace-runtime-service-authz

- `pc-http`：axum Router + middleware stack + 错误映射 + 请求体验证
- `pc-openapi`：utoipa 集成 + `/openapi.json` + `/openapi.yaml`

**交付物**：
- 全部 56 个路由迁移完成
- OpenAPI 3.1 文档自动生成
- 与原 server 端点行为字节级一致的集成测试套件

**负责人**：3 名 Rust 工程师（每位负责 ~18 路由）

---

### Phase D — 实时通信与心跳引擎（周 9–10）

**目标**：WebSocket live-events + heartbeat 状态机 + workflow 引擎。

**关键任务**：
- `pc-realtime`：RealtimeBus trait + InMemoryBus（tokio::sync::broadcast）
- `pc-ws`：WebSocket 升级、token 校验、subscribe/unsubscribe/ping 协议、断线重连缓冲
- `pc-heartbeat`：状态机 PickRunnable → Finalize，并发上限，monitor/watchdog/recovery
- `pc-workflow`：routines + pipelines + cron 调度（tokio-cron-scheduler）

**交付物**：
- WebSocket live-events 端到端测试
- Heartbeat 从触发到 live-events 推送的完整剧本
- Routines / pipelines cron 触发测试

**负责人**：1–2 名 Rust 工程师

---

### Phase E — 适配器与插件系统（周 11–14）

**目标**：11 个内置适配器 host + 插件 Worker 池。

**关键任务**：
- `pc-adapter-api`：AdapterRuntime trait + AdapterStream + 配置 schema
- 11 个 `pc-adapter-{name}` crate：
  - claude-local / codex-local / cursor-cloud / cursor-local / gemini-local / grok-local / hermes-gateway / openclaw-gateway / opencode-local / pi-local
- `pc-plugin-protocol`：serde 派生 RPC 消息类型 + JSON schema
- `pc-plugin-host`：WorkerPool / EventBus / JobScheduler / JobStore / ToolDispatcher / DatabaseBridge / StateStore / WebhookDispatcher / ManifestValidator / CapabilityValidator

**集成测试**：每个 adapter 与原 Node adapter 在同一 fixture 下输出等价；插件加载 → 注册事件 → 触发作业的端到端。

**交付物**：
- 11 个适配器 crate + 集成测试
- 插件 host 完整 RPC 协议实现 + 与原 SDK worker 互操作测试

**负责人**：2 名 Rust 工程师（每位负责 5–6 个 adapter）

---

### Phase F — CLI、可观测性与打磨（周 15–16）

**目标**：CLI 全部子命令 + OpenTelemetry + 端到端验证 + 性能基准。

**关键任务**：
- `pc-cli`（clap v4）：20+ 子命令（run / install / onboard / doctor / worktree / heartbeat-run / pipelines / routines / service / update / configure / db backup / auth-bootstrap-ceo / allowed-hostname / env / env-lab / uninstall），全部支持 `--json`
- `pc-migrate`：独立迁移工具（up/down/status/create）
- `pc-telemetry` 完善：OTLP exporter + 启动横幅 + access log + log 重写
- `pc-backup`：数据库备份链路
- `pc-feature-flags`：feature catalog
- `pc-doc-anchors`：文档锚点/批注
- 端到端验证：用原 server 与 Rust server 跑同一组 curl/集成测试；用原 UI（`VITE_API_BASE` 指向 Rust server）完整冒烟测试
- 性能基准：`wrk` 压测对比 Node server（目标：延迟 -30%、内存 -40%）

**交付物**：
- `paperclipai` 二进制覆盖所有 CLI 命令
- 端到端剧本通过
- 性能基准报告

**负责人**：1 名 Rust 工程师 + 1 名 SRE/可观测性工程师

---

### Phase G — 切流量与归档（周 17）

**目标**：灰度切换、全量上线、归档原 Node server。

**关键任务**：
- UI 默认 `VITE_API_BASE` 切换到 Rust server；保留 Node server 作为只读回滚（监听 3101）
- 监控错误率、延迟、内存 7 天
- 全量：移除 `paperclip/server/` 与 `paperclip/cli/` 运行依赖（保留归档）
- 更新根 README 与 AGENTS.md 指向 `paperclip-rs/`
- 调整 CI：Rust 构建为默认；Node 构建仅保留 UI 与适配器 UI bundle
- 发布 `paperclip-server:1.0.0` 与 `paperclipai:1.0.0` 容器镜像
- 文档：`ARCHITECTURE.md` / `OPERATIONS.md` / `PLUGIN_AUTHORING.md` / `MIGRATION_FROM_NODE.md`

**交付物**：
- 1.0.0 正式发布
- 文档齐全
- Node server 归档

**负责人**：全体成员 + 发布经理

---

## 六、持续质量保障（横切关注点）

### 测试金字塔

```
        ╱  ╲
       ╱ E2E╲         Playwright（复用原 e2e 套件）
      ╱──────╲
     ╱  集成   ╲       每个路由 happy + 3 edge case
    ╱──────────╲
   ╱   单元测试  ╲     ≥80% 覆盖率（cargo-llvm-cov）
  ╱──────────────╲
 ╱  静态分析 + Lint╲   clippy -D warnings, rustfmt, cargo-deny
╱──────────────────╲
```

### 安全

- `cargo-audit` 在 CI 阻断高危漏洞
- `cargo-deny` 阻断未授权 license
- 密钥管理 review（避免日志泄露、错误回显）
- 依赖 SBOM（`cargo-cyclonedx`）
- 威胁模型文档

### 性能

- `criterion` 基准（每 crate 关键路径）
- `wrk` HTTP 压测脚本
- 内存 profiling（`heaptrack` / `dhat`）
- 火焰图（`cargo-flamegraph`）

### 可观测性

- Prometheus metrics 端点（`/metrics`）
- 结构化日志采样率可配置
- 健康检查分级：`/health`（liveness）vs `/ready`（readiness）
- 关键路径 trace 采样率

---

## 七、风险登记册（Risk Register）

| ID | 风险 | 概率 | 影响 | 缓解策略 | 负责人 |
|---|---|---|---|---|---|
| R1 | 行为偏差（API/WS 与原 server 不一致） | 高 | 高 | 以原 OpenAPI + Vitest 集成测试为契约；每路由一集成测试 | Phase 负责人 |
| R2 | 数据库 schema 漂移 | 中 | 高 | SQL DDL 直接来自 Drizzle 推导；CI 跑 `pc-migrate up` 验证 | DB 工程师 |
| R3 | 嵌入式 PG 在某平台无预构建二进制 | 中 | 中 | 外部 PG 优先 + 嵌入式失败时降级 | Release 工程师 |
| R4 | async/await 取消语义差异 | 中 | 中 | `tokio::time::timeout` + `CancellationToken` 显式传播；集成测试 | Phase D 负责人 |
| R5 | 第三方 SDK（better-auth）行为难复刻 | 中 | 高 | 仅复刻对外行为；UI 通过 cookie/CSRF 头验证 | Auth 工程师 |
| R6 | 适配器 worker 兼容 | 低 | 高 | JSON-RPC schema 单元测试 + 互操作测试 | Adapter 工程师 |
| R7 | 个别路径性能不如 V8 JIT | 低 | 中 | criterion 基准 + spawn_blocking | 性能工程师 |
| R8 | 单二进制依赖 glibc | 中 | 中 | musl 静态 + glibc 动态双构建 | Release 工程师 |
| R9 | 重写周期长，需求漂移 | 中 | 高 | 增量迁移 + 双栈并行；任一阶段可回滚 | PM |
| R10 | OTLP exporter 性能开销 | 低 | 低 | 默认关闭，按需启用 | 可观测性工程师 |

---

## 八、资源估算

### 团队配置（最小可行）

| 角色 | 人数 | 投入 | 持续时间 |
|---|---|---|---|
| Rust 工程师（owner，核心层 + 路由 + 心跳） | 1 | 全职 | 17 周 |
| Rust 工程师（仓储 + 认证 + 适配器） | 1 | 全职 | 17 周 |
| Rust 工程师（CLI + 可观测 + 发布） | 1 | 全职 | 15 周 |
| 前端/集成工程师（双栈切换 + e2e） | 1 | 半职 | 8 周（Phase B–G） |
| 原 Node server 维护者（契约对照顾问） | 1 | 兼职 | 4 周（Phase A–C 集中） |
| SRE / 可观测性工程师 | 1 | 兼职 | 4 周（Phase F–G 集中） |
| PM（里程碑跟踪 + 风险升级） | 1 | 兼职 | 全程 |

### 基础设施

| 项 | 用途 | 备注 |
|---|---|---|
| GitHub Actions runner | CI（fmt + clippy + test + 双构建） | macOS + Linux + Windows |
| 集成测试数据库 | 每 PR 跑迁移 + e2e | 可选 ephemeral PG |
| 压测环境 | Phase F 性能基准 | 类生产规格 |
| 灰度集群 | Phase G 切流量 | 镜像两个 server 版本 |

---

## 九、沟通计划

| 事件 | 频率 | 受众 | 形式 |
|---|---|---|---|
| 每日站会 | 每个工作日 | 团队全体 | 15 分钟同步 |
| 周复盘 | 每周五 | 团队 + 利益相关方 | 1 小时，含演示 |
| 里程碑评审 | 每个 Phase 结束 | 管理层 + 团队 | 半天，含演示 + 决策 |
| 风险升级 | 即时 | PM + 技术负责人 | Slack/邮件 |
| 发布前冻结 | Phase G 前 1 周 | 全员 | 代码冻结 + 集中验证 |

---

## 十、文档矩阵

| 文档 | 读者 | 时机 |
|---|---|---|
| `proposal.md` | 全员、利益相关方 | Open 阶段（已完成） |
| `design.md` | 工程师 | Open 阶段（已完成） |
| `tasks.md` | 工程师 | Open 阶段（已完成） |
| `specs/*.md` | 工程师 + 自动化 | Open 阶段（已完成） |
| `ARCHITECTURE-DIAGRAMS.md` | 全员 | Open 阶段（已完成） |
| `MODULE-MAPPING.md` | 工程师 | Open 阶段（已完成） |
| `PROJECT-PLAN.md`（本文档） | PM + 利益相关方 | Open 阶段（已完成） |
| `ARCHITECTURE.md`（crate 内部 API） | 工程师 | Phase A 末 |
| `OPERATIONS.md`（部署/备份/监控） | SRE | Phase F 末 |
| `PLUGIN_AUTHORING.md` | 插件作者 | Phase E 末 |
| `MIGRATION_FROM_NODE.md` | 部署者 | Phase G 末 |
| `CHANGELOG.md` | 全员 | 持续 |

---

## 十一、成功标准（Definition of Done）

### Phase 级别 DoD

每个 Phase 结束时必须满足：

- [ ] 所有单元测试通过（覆盖率 ≥ 80%）
- [ ] 所有集成测试通过（每个路由 happy + 3 edge case）
- [ ] `cargo clippy -- -D warnings` 无警告
- [ ] `cargo fmt --check` 无 diff
- [ ] `cargo build --release` 在 musl + glibc 目标均成功
- [ ] 原 UI（`VITE_API_BASE` 指向 Rust）冒烟测试通过
- [ ] 端到端剧本通过（启动 → 创建 → 触发 → 推送）
- [ ] 文档已更新（对应阶段的 README/操作手册）
- [ ] 性能基准已记录（criterion + wrk）
- [ ] 风险登记册已更新

### 项目级 DoD（最终交付）

- [ ] 全部 56 路由迁移并通过集成测试
- [ ] 全部 11 适配器 host 实现
- [ ] 插件 host 与原 SDK worker 互操作
- [ ] CLI 全部子命令覆盖
- [ ] OpenAPI 3.1 文档自动生成且与原 server 字段一致
- [ ] UI 默认指向 Rust server 稳定运行 7 天
- [ ] 性能基准：延迟 ↓ ≥ 30%，内存 ↓ ≥ 40%
- [ ] Docker 镜像 ≤ 120MB（musl 静态）
- [ ] 1.0.0 正式发布
- [ ] 文档齐全（ARCHITECTURE / OPERATIONS / PLUGIN_AUTHORING / MIGRATION）

---

## 十二、回滚策略

任何阶段发现阻塞问题：

1. **代码回滚**：UI `VITE_API_BASE` 切回 Node server（数据零迁移）
2. **配置回滚**：移除 Rust server 端口（3100 → 0），Node server 持续监听 3101
3. **数据回滚**：不需要（109 表 schema 不变）
4. **沟通**：立即通知全团队 + 利益相关方 + 启动事后复盘

每个 Phase 结束保留"切换开关"配置项，确保任何时刻可在 5 分钟内切回 Node server。

---

## 附录 A：与其他文档的关系

```
                    ┌────────────────────────────┐
                    │      PROJECT-PLAN.md       │  ← 本文档（人读总体执行蓝图）
                    │      17 周 / 7 阶段         │
                    └─────────────┬──────────────┘
                                  │
        ┌─────────────────────────┼─────────────────────────┐
        ▼                         ▼                         ▼
┌───────────────┐         ┌───────────────┐         ┌───────────────┐
│ proposal.md   │         │  design.md    │         │  tasks.md     │
│ Why/What      │         │ How           │         │ 275 checkbox  │
│ 69 行         │         │ 444 行        │         │ 430 行        │
└───────────────┘         └───────────────┘         └───────────────┘
                                  │
                                  ▼
                         ┌─────────────────┐
                         │   specs/*.md    │  19 文件 / 28 req / 33 scenario
                         │   (机器可读契约) │
                         └─────────────────┘

辅助：
- ARCHITECTURE-DIAGRAMS.md  10 张架构图
- MODULE-MAPPING.md         15 节逐模块对照
- .comet.yaml               phase=design
```

| `docs/01-VITE-ERROR-ROOT-CAUSE.md` | Vite Lexical.mjs ENOENT 根因分析与修复 | 运维 |
| `docs/02-PAPERCLIP-ARCHITECTURE.md` | Node/TS 端完整基线架构分析 | 全体 |
| `docs/03-KAMEO-ACTOR-ANALYSIS.md` | kameo Actor 架构分析与路由优化计划 | 后端 |
| `docs/04-EXECUTION-PLAN.md` | 最新执行计划（已完成 + 待实现 + 风险评估） | PM |

## 附录 B：术语表

| 术语 | 含义 |
|---|---|
| Adapter | AI 代理运行时（Claude/Codex/Cursor 等）的 host 端实现 |
| Plugin | 通过 `paperclip-plugin-*` npm 包扩展的第三方能力 |
| Heartbeat | agent 一次完整执行周期（pick → invoke → finalize） |
| Live-event | 实时推送给已订阅 WebSocket 客户端的业务事件 |
| Issue / Case / Project / Approval / Decision / Routine / Pipeline | 核心业务实体 |
| Hoisted linker | pnpm 配置：`node-linker=hoisted`（所有依赖提升到根 node_modules） |

## 附录 C：关键文档链接

- `paperclip-rs/openspec/changes/paperclip-rs-rewrite/proposal.md`
- `paperclip-rs/openspec/changes/paperclip-rs-rewrite/design.md`
- `paperclip-rs/openspec/changes/paperclip-rs-rewrite/tasks.md`
- `paperclip-rs/openspec/changes/paperclip-rs-rewrite/specs/`
- `paperclip-rs/ARCHITECTURE-DIAGRAMS.md`
- `paperclip-rs/MODULE-MAPPING.md`
- `paperclip-rs/openspec/changes/paperclip-rs-rewrite/.comet.yaml`

---

**计划版本**：v1.0 · **最后更新**：2026-08-02 · **状态**：已交付，待评审
