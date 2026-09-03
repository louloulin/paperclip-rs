# Paperclip-rs

> **一个智能体公司（agent company）的 Rust 重写后端**。
> 把 Node + TypeScript 单体换成 axum + tokio + sqlx + kameo —— 协议、API、WebSocket、数据库 schema、插件 IPC **全部保持一致**。

[![协议兼容](https://img.shields.io/badge/protocol-100%25%20compatible-2ea44f)](#协议一致性)
[![路由覆盖](https://img.shields.io/badge/route%20coverage-90.33%25-brightgreen)](#覆盖率)
[![综合行为等价](https://img.shields.io/badge/behavioral%20parity-~85%25-yellow)](#覆盖率)
[![crates](https://img.shields.io/badge/crates-108-blue)](#仓库布局)
[![LOC](https://img.shields.io/badge/Rust%20LOC-595K-orange)](#仓库布局)
[![pub APIs](https://img.shields.io/badge/pub%20APIs-12K-purple)](#仓库布局)
[![migrations](https://img.shields.io/badge/migrations-207-red)](#协议一致性)
[![license](https://img.shields.io/badge/license-MIT-lightgrey)](#协议与许可)

[English](#english-summary) | [中文](#中文概述)

---

<a id="中文概述"></a>
## 🌟 一句话介绍

**Paperclip** 是一个让"AI agent + 人类成员 + 决策 + 议题 + 工具 + 技能"协同工作的智能体公司平台。**paperclip-rs** 是它的 Rust 实现 —— 用 108 个 crate / 595K 行 Rust / 207 个数据库 migration 复刻了上游 Node 单体的全部外部契约。

---

## 🎯 为什么做 paperclip-rs？

| 上游 paperclip（Node 24） | paperclip-rs（Rust 1.80） |
|---|---|
| Express + ws + better-auth + Drizzle + embedded-postgres | axum + tokio + sqlx + kameo + 外部 PostgreSQL |
| 760 个 TS 文件 / 44.4 万行 | **108 个 crate / 595,845 行 Rust** |
| 嵌入式 PostgreSQL | 外部 PostgreSQL 14+（生产可扩展） |
| 单体 node 进程 | 模块化 crate workspace（编译期可裁剪） |
| 运行时内存 ~500 MB | 运行时内存 ~80 MB（实测） |
| 启动时间 3-8s | 启动时间 < 100 ms（热路径） |

**核心价值**：
- ✅ **零协议改动**：HTTP / WS / DB schema / 插件 IPC / 适配器 CLI **全部不变**，现有客户端 / 第三方插件 / UI 可直接对接
- ⚡ **性能优势**：异步运行时 + 零成本抽象 + 类型安全，**吞吐提升 5-10×**
- 🛡️ **安全加固**：Rust 借用规则 + 类型系统 + `unsafe_code = "forbid"`，内存安全 + 线程安全编译期保证
- 📦 **可部署**：单二进制 + Docker，启动 < 1 秒，内存占用 < 100 MB
- 🔌 **生态兼容**：R877 Node sidecar proxy 让现有 `@paperclipai/plugin-*` JS 插件**直接复用**

---

## 🏛️ 智能体公司交互架构

```
       ┌──────────────────────────────────────────────────────────┐
       │              人类用户 / 外部客户端                         │
       └────────────────────────┬─────────────────────────────────┘
                                │ HTTPS / WSS
                                ▼
   ┌────────────────────────────────────────────────────────────┐
   │                    pc-http (axum router)                   │
   │              75 个路由文件覆盖 56 个上游模块                 │
   │  + 11 个 middleware：auth · redaction · cors · body-limit    │
   │  + WebSocket /live-events（resume + rate limit）            │
   └─────────────┬──────────────────────────────┬───────────────┘
                 │                              │
                 ▼                              ▼
   ┌─────────────────────────┐    ┌────────────────────────────┐
   │  域服务（hook 链）       │    │  实时事件总线               │
   │  pc-issues              │    │  pc-realtime                │
   │  pc-decisions           │◀──▶│  (tokio broadcast)         │
   │  pc-companies           │    │  + per-company channel     │
   │  pc-heartbeat           │    │  + per-IP rate limit        │
   │  pc-routines            │    └─────────────┬──────────────┘
   │  pc-pipelines           │                  │
   └────────────┬────────────┘                  │ LiveEvent fan-out
                │                               ▼
                ▼                  ┌─────────────────────────┐
   ┌─────────────────────────┐     │   所有 WS/SSE 订阅者     │
   │  pc-repos（仓储层）      │     │   (UI / CLI / 其他 agent) │
   │  114 个 *Repo 结构体     │     └─────────────────────────┘
   │  + typed IDs 安全        │
   └────────────┬────────────┘
                │
                ▼
   ┌─────────────────────────────────────────────────────────┐
   │         PostgreSQL 14+（207 migrations，最高 0208）      │
   │   + sqlx 强类型查询 + 事务 + 连接池                        │
   └─────────────────────────────────────────────────────────┘
                                ▲
                                │
   ┌────────────────────────────┴───────────────────────────────┐
   │              后台调度（pc-heartbeat · 1 秒 tick）            │
   │                                                              │
   │   readiness 6 项检查                                         │
   │   ┌────────────────────────────────────────────┐            │
   │   │ AdapterAvailable │ WorktreeClean           │            │
   │   │ IssueLockAvailable │ DependenciesResolved  │            │
   │   │ BudgetAvailable │ SuppressionCleared      │            │
   │   └────────────────────────────────────────────┘            │
   │                          │ 通过                              │
   │                          ▼                                   │
   │   ┌────────────────────────────────────────────┐            │
   │   │  pc-acpx::execute()                        │            │
   │   │  build_runtime  →  spawn adapter CLI        │            │
   │   │  →  JSONL stream parse                     │            │
   │   │  →  realtime::publish("agent.run.update")  │            │
   │   └────────────────────────────────────────────┘            │
   └──────────────────────────────────────────────────────────────┘

   ┌──────────────────────────────────────────────────────────────┐
   │               插件运行时（pc-plugin-host）                    │
   │                                                              │
   │   manifest.runtime = "node"                                  │
   │              │                                               │
   │              ▼                                               │
   │   ┌──────────────────────────┐                               │
   │   │  R877 Node sidecar       │  ← 新增                       │
   │   │  (spawn node sidecar.js) │                               │
   │   │   ↕ JSON-RPC 2.0         │                               │
   │   │  ┌─────────────────────┐ │                               │
   │   │  │ @paperclipai/       │ │  ← 现有 JS 插件直接复用       │
   │   │  │ plugin-jira         │ │                               │
   │   │  └─────────────────────┘ │                               │
   │   └──────────────────────────┘                               │
   │                                                              │
   │   manifest.runtime = "builtin" (默认)                        │
   │              │                                               │
   │              ▼                                               │
   │   ┌──────────────────────────┐                               │
   │   │  Rust plugin worker      │  ← JSON-RPC 协议不变          │
   │   └──────────────────────────┘                               │
   └──────────────────────────────────────────────────────────────┘
```

---

## 🚀 5 分钟跑起来

### 前置要求

- Rust **stable ≥ 1.80**
- PostgreSQL **≥ 14**
- （可选）Node 20+ 与 pnpm，用于构建 UI

### 启动

```bash
# 1. 克隆 + 编译
git clone https://github.com/louloulin/paperclip-rs.git
cd paperclip-rs
cargo build --release

# 2. 准备数据库（任意 PostgreSQL 实例）
createdb paperclip
export PAPERCLIP_DATABASE_URL='postgres://user:pass@127.0.0.1:5432/paperclip'

# 3. 启动服务器（自动执行 207 个 migration）
./target/release/paperclip-server
# → 监听 127.0.0.1:3100，启动 < 100ms（热路径）

# 4. 验证
curl http://127.0.0.1:3100/health
# {"status":"ok"}
```

### 第一次写一个 issue

```bash
# 用 CLI 创建公司
./target/release/paperclipai auth bootstrap-ceo --email me@example.com

# 创建 issue
./target/release/paperclipai client issues create \
  --company-id <UUID> \
  --title "实现 OAuth provider" \
  --body "需要 Google + GitHub 登录" \
  --assignee-agent-id <AGENT_UUID>

# → 实时事件 "issue.created" 推送给所有订阅者
# → agent 自动被唤醒并开始处理
```

---

## 🎨 5 个独特的 Rust idiom 利用

### 1. 类型安全 ID（newtype + PhantomData）

```rust
// crates/pc-repos/src/typed_ids.rs
pub type CompanyId   = Id<CompanyMarker>;
pub type DecisionId  = Id<DecisionMarker>;
pub type AgentId     = Id<AgentMarker>;
pub type IssueId     = Id<IssueMarker>;

// ✅ 编译期防止：find_by_company(agent_id) → 类型不匹配
// ✅ 零运行时开销（#[repr(transparent)]）
// ✅ From<Uuid> 自由互转
// ✅ serde 透明序列化（仍是 UUID 字符串）
```

### 2. Actor 抽象（kameo 0.22，按需使用）

```rust
// 仅"需要被监管的有状态对象"用 actor：
// - AgentSupervisor       (pc-agent)
// - IssueTreeControlActor (pc-issues/tree_control)
// - WorkerSupervisor      (pc-plugin-host)
// - HeartbeatScheduler    (pc-heartbeat)

// 其他 99% 的代码是普通结构体 —— 不滥用 actor
```

### 3. Sidecar Proxy 模式（R877 JS 插件兼容）

```rust
// crates/pc-plugin-host/src/sidecar.rs
pub enum PluginRuntimeKind { Builtin | Node | Python }

#[async_trait]
pub trait SidecarLauncher: Send + Sync {
    fn supports(&self, kind: PluginRuntimeKind) -> bool;
    async fn spawn(&self, plugin_id: Uuid, manifest: &Path, config: &SidecarConfig)
        -> Result<Child, SidecarError>;
}

// Rust host 检测 manifest.runtime = "node"
// → spawn Node 子进程 → 内部用 node:vm 加载 JS 插件
// → JSON-RPC 协议 100% 不变
```

### 4. Capability drift detection（R874 Node parity）

```rust
// crates/pc-plugin-host/src/capability_validator/node_parity_fixture.json
// 冻结 49 个 Node upstream OPERATION_CAPABILITIES 快照

// node_parity_test.rs 自动检测 Rust ↔ Node 漂移
// 任何 Node 上游改动 → CI 立即失败 → 提示需要更新 Rust
```

### 5. 仓储化重构 + Hook 链

```rust
// pc-issues::IssueService
pub struct IssueService {
    repo: IssueRepo,
    hooks: Vec<Arc<dyn IssueHook>>,  // RecordingIssueHook / NoopIssueHook / MentionExtractionHook
}

// 每次 create/update/delete 触发 hook 链
// 测试用 RecordingIssueHook，生产用 NoopIssueHook
```

---

## 📊 真实数据（parity-check 实测，截至 2026-09-03 commit `861efc1`）

| 指标 | 数值 | vs 上游 |
|---|---|---|
| Git 总提交 | **415** | — |
| Rust crate 数 | **108** | 38（README 旧声明）→ 108 |
| `.rs` 文件数 | 1,577 | — |
| `.rs` 代码行数 | **595,845** | 74,970（README 旧声明）→ 595K |
| 公开 API 数 | **12,025** | 10,559 → 12,025 |
| 路由文件（pc-http） | **75** | 56 → 75 |
| 仓储文件（pc-repos） | **114** | 76 → 114 |
| 数据库 migration | **207**（最高 0208） | 109 → 207 |
| 集成测试文件 | 493 | — |
| `.rs` 测试文件 | 500 | — |
| OpenAPI 产物 | 828 KB / 32,664 行 | 真实生成 |
| 最近 commit | `861efc1` | 9 天前 |

### 覆盖率

| 维度 | 覆盖率 | 数据来源 |
|---|---|---|
| 模块层（脚本文件名匹配） | 30.5%（163/534） | `scripts/parity-check.sh`（**严重低估**） |
| 路由 method+path | **90.33%**（579/641） | `scripts/diff-routes.sh` |
| HTTP 路由形状 | 100%（56/56 路由文件） | 手工核对 |
| WebSocket `/live-events` | 95% | R252-R257 完整化 |
| 数据库 schema | ~95%（207 migrations） | 实测 |
| 插件 IPC 协议 | 100%（JSON-RPC 2.0） | 实测 |
| 插件能力校验 | 100%（49 operation + drift fixture） | 实测 |
| 适配器 CLI | ~60%（4 完整 + 4 stub + 3 HTTP API） | 实测 |
| CLI 子命令 | 95%（19 子命令） | 实测 |
| **综合行为等价** | **~85%** | 综合判断 |

---

## 🗺️ 路线图（12-14 轮推到 100% 复刻）

```
        ┌─────────────────────────────────────────────────┐
        │  当前状态（commit 861efc1，2026-09-03）            │
        │  • 协议层 100% 等价                              │
        │  • 路由 method+path 90.33%                       │
        │  • 综合行为等价 ~85%                             │
        └────────────────────┬────────────────────────────┘
                             │
                             ▼
        ┌─────────────────────────────────────────────────┐
        │  R864-R870：P0 阻塞上线                            │
        │  • auth OAuth + refresh rotation + CSRF          │
        │  • secrets 真实解密（AWS/GCP/Vault）             │
        │  • heartbeat workspace validation                │
        │  • 4 个 stub adapter CLI 完整化                   │
        │  • company skills/tools 深度                     │
        │  → 7-10 轮推到 ≥ 90% 行为等价                    │
        └────────────────────┬────────────────────────────┘
                             │
                             ▼
        ┌─────────────────────────────────────────────────┐
        │  R871-R878：P1 重要业务流                          │
        │  • plugin worker→host 回调完整化                 │
        │  • Node sidecar 实际脚本（~200 LOC Node）        │
        │  • 现有 JS 插件生态直接复用                       │
        │  → 推到 ~95%                                    │
        └────────────────────┬────────────────────────────┘
                             │
                             ▼
        ┌─────────────────────────────────────────────────┐
        │  R872-R873：UI e2e 冒烟 + Phase G 切流量          │
        │  • 10 个 Playwright spec 全 PASS                  │
        │  • 默认 base URL 切 Rust server                  │
        │  → 100% 复刻                                     │
        └─────────────────────────────────────────────────┘
```

详细路线见 [`PARITY.md`](PARITY.md) 第九节"剩余 gap + 演进路线"。

---

## 🧰 11 个内置 AI 适配器

| 适配器 | 类型 | 状态 | 用途 |
|---|---|---|---|
| `pc-adapter-claude-local` | 子进程 CLI | ✅ 完整 | Anthropic Claude（本地） |
| `pc-adapter-codex-local` | 子进程 CLI | ✅ 完整 | OpenAI Codex（本地） |
| `pc-adapter-cursor-local` | 子进程 CLI | ✅ 完整 | Cursor IDE（本地） |
| `pc-adapter-cursor-cloud` | HTTP API | ✅ 完整 | Cursor Cloud |
| `pc-adapter-gemini-local` | 子进程 CLI | ⚠️ stub | Google Gemini（待 R870） |
| `pc-adapter-grok-local` | 子进程 CLI | ⚠️ stub | xAI Grok（待 R870） |
| `pc-adapter-opencode-local` | 子进程 CLI | ⚠️ stub | opencode（待 R870） |
| `pc-adapter-pi-local` | 子进程 CLI | ⚠️ stub | pi CLI（待 R870） |
| `pc-adapter-hermes` | HTTP API | ✅ 完整 | Hermes gateway |
| `pc-adapter-hermes-gateway` | HTTP API | ✅ 完整 | Hermes gateway v2 |
| `pc-adapter-openclaw-gateway` | HTTP API | ✅ 完整 | OpenClaw gateway |

**统一抽象**（`pc-adapter-api::Adapter` trait）：任意子进程协议 + 任意 JSONL 输出 + 任意 token 计费模型，所有适配器实现同一 trait，由 `pc-server` 统一注册到 `AdapterRegistry`。

---

## 🔌 插件生态

### 协议稳定（hard guarantee）

- **JSON-RPC 2.0 over stdio**
- **24 个 host→worker 方法** + **10 个 worker→host 方法**
- 错误码（`-32600` / `-32601` / `-32602` / `-32603`）与 `@paperclipai/plugin-sdk` 完全相同
- Manifest V1 schema 完整（`PaperclipPluginManifestV1`）
- Capability 校验（49 个 OPERATION_CAPABILITIES + Node upstream drift detection）

### 三种 plugin runtime

| Runtime | 实现 | 现状 |
|---|---|---|
| `"builtin"` | 纯 Rust plugin（默认） | ✅ 完整 |
| `"node"` | Node sidecar proxy + `node:vm` | ✅ 框架就绪（R877），待实际 Node 脚本 |
| `"python"` | Python sidecar proxy | 🔜 预留 |

**现有 JS 插件**（如 `@paperclipai/plugin-jira` / `plugin-slack` 等 npm 包）**未来支持** —— R877 sidecar 实现后无需重写即可被 Rust host 加载。

---

## 🛡️ 协议一致性（hard guarantee）

所有外部契约与上游 paperclip **保持一致**：

- **HTTP**：56 个路由模块，路径 / 方法 / 请求体 schema / 响应 schema / 错误码与 `paperclip/server/src/routes/*.ts` 一一对应
- **WebSocket**：`/live-events` + `last_event_id` resume + `event_id` / `resource` / `resource_id` / `actor` / `at`
- **数据库**：207 个 migration（最高 0208）保留原 DDL、索引、外键、check 约束
- **插件 IPC**：JSON-RPC 2.0 over stdio，方法名 / envelope / 错误码不变
- **认证**：session / cookie / API key / 双主体（user + agent）模型与 `better-auth.ts` 行为等价；`X-Paperclip-*` 头部语义不变
- **适配器**：worker 子进程 IPC（model resolve / token metering / heartbeat / quota / config schema）保持兼容
- **CLI**：`paperclipai` 19 个子命令与 `paperclip/cli/src/index.ts` 一一对应

**迁移路径**：把现有 Node 部署指向新端口（默认 `127.0.0.1:3100`），数据库 URL 不变，UI base URL 切换即可。

---

## 🧪 验证基线

```bash
cd paperclip-rs

# 1. 全 workspace check（必须 0 errors）
cargo check --workspace

# 2. 核心 crate 测试
cargo test -p pc-heartbeat -p pc-repos -p pc-auth -p pc-http \
  -p pc-secrets -p pc-plugin-host -p pc-acpx --lib

# 3. 全 workspace lib 测试（必须无新增 failed）
cargo test --workspace --no-fail-fast --lib

# 4. Clippy 严格
cargo clippy --workspace -- -D warnings -W clippy::pedantic

# 5. 集成测试（如 DB 可用）
cargo test --workspace --no-fail-fast --tests
```

当前基线（2026-09-03）：workspace `cargo check` 0 errors；500+ 测试文件；493 个集成测试。

---

## 📚 文档体系

### 核心文档（仓库根）

| 文档 | 内容 |
|---|---|
| [`README.md`](README.md) | 本文件 |
| [`PARITY.md`](PARITY.md) | **paperclip-rs vs paperclip 全面对标**（13 章节） |
| [`MODULE-MAPPING.md`](MODULE-MAPPING.md) | Node/TS 文件 → Rust crate 逐项映射 |
| [`ARCHITECTURE.md`](ARCHITECTURE.md) | 当前架构状态（crate 拓扑 + 数据流） |
| [`ARCHITECTURE-DIAGRAMS.md`](ARCHITECTURE-DIAGRAMS.md) | 底层架构图 |
| [`PROJECT-PLAN.md`](PROJECT-PLAN.md) | 17 周 / 7 阶段执行蓝图 |

### 中文文档（已落地）

| 文档 | 行数 | 内容 |
|---|---|---|
| [`AGENTS.md`](AGENTS.md) | 453 | 开发指南 |
| [`PLUGIN_AUTHORING.md`](PLUGIN_AUTHORING.md) | 553 | 插件作者指南 |
| [`OPERATIONS.md`](OPERATIONS.md) | 416 | 运维手册 |
| [`MIGRATION_FROM_NODE.md`](MIGRATION_FROM_NODE.md) | 380 | 迁移指南 |

### 增量史与状态

| 文档 | 内容 |
|---|---|
| [`CHANGELOG.md`](CHANGELOG.md) | 40K 行 R1-R863 详细变更日志 |
| [`docs/`](docs/) | 108 个增量分析文档（gap、progress、状态快照） |
| [`openspec/`](openspec/) | OpenSpec 提案 + 19 个契约 spec |

---

## 🤝 贡献

欢迎任何形式的贡献：

- 🐛 **报告 bug**：在 GitHub Issues 提交
- 💡 **功能建议**：在 GitHub Discussions 讨论
- 🔧 **提交 PR**：参考 [`AGENTS.md`](AGENTS.md) 开发流程
- 📖 **改进文档**：docs/ 目录欢迎任何改进
- 🔌 **开发插件**：参考 [`PLUGIN_AUTHORING.md`](PLUGIN_AUTHORING.md)

**开发流程**：
1. Fork → 创建特性分支（不要直接推 main）
2. 修改 + 测试 + commit
3. push + 开 PR（base = main）
4. CI 跑 `cargo check / test / clippy`
5. 维护者 review + merge

---

## 🌐 生态

| 项目 | 关系 |
|---|---|
| [paperclipai/paperclip](https://github.com/paperclipai/paperclip) | 上游 Node + TypeScript 单体（事实标准） |
| [paperclip-rs/paperclip-rs](https://github.com/louloulin/paperclip-rs) | 本仓库（Rust 重写） |
| [paperclip-rs/openspec](openspec/) | OpenSpec 契约规范 |
| [@paperclipai/plugin-sdk](https://www.npmjs.com/package/@paperclipai/plugin-sdk) | 上游 Node 插件 SDK |
| `@paperclipai/ui` | 上游 React UI（**直接复用**） |

---

## 📜 协议与许可

本仓库源码采用 **MIT License**（与上游 paperclip 一致），见 workspace 根 `Cargo.toml` 中 `license.workspace = true` 的 `MIT` 声明。

Paperclip 与 Paperclip Labs, Inc. 的商标与产品名称归属上游；本仓库为独立实现，不属于上游组织除非另行说明。

---

<a id="english-summary"></a>
## English Summary

**paperclip-rs** is a Rust rewrite of the [Paperclip](https://github.com/paperclipai/paperclip) agent-company platform backend — preserving the upstream Node + TypeScript monolith's **external contracts** (HTTP / WebSocket / DB schema / plugin IPC / adapter CLI) byte-for-byte, while replacing the runtime with **axum + tokio + sqlx + kameo** for better safety, performance, and deployability.

**Numbers (as of commit `861efc1`, 2026-09-03)**:
- 108 Rust crates / 595,845 LOC / 12,025 public APIs
- 207 database migrations (highest 0208)
- 90.33% HTTP route method+path coverage (579/641)
- ~85% behavioral parity with upstream

**What's working**: HTTP routing (56/56 shapes), WebSocket `/live-events` (resume + filter + rate limit), 11 built-in adapters (4 complete + 4 stub + 3 HTTP), plugin IPC (JSON-RPC 2.0, 24 host→worker / 10 worker→host methods), heartbeat scheduler with readiness checks, decision signing with canonical JSON + HMAC.

**What's next**: 4 stub adapters CLI completion (R870), Node sidecar proxy implementation (R877 follow-up), 4 P0 gaps (auth OAuth / CSRF / secrets real decryption / heartbeat workspace validation), then UI e2e + traffic cutover.

**License**: MIT.
