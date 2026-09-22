# multica-rs：Rust 重写 Multica 后端的规划

> 在 [`paperclip-rs`](../paperclip-rs) 工作区的基础上，把 Multica（Go + TypeScript + PostgreSQL）后端
> 改写为 Rust crate 工作区。本文档是**首次全量规划**，描述目标、能力复用分析、crate 蓝图与里程碑。

## 1. 背景

Multica 是一个"AI 同事可以出现在板上"的工作空间——把 Claude Code、Codex、Cursor 等 26 个
CLI 驱动为可分派任务的队友，把每一次执行的意图、过程与 diff 留在同一个 issue 里。
当前实现：

| 层 | 技术栈 |
| --- | --- |
| Web / Desktop / Mobile | Next.js 16 / Electron / Expo (TypeScript) |
| Backend | Go（Chi + gorilla/websocket + pgx） |
| 数据库 | PostgreSQL 17（pgcrypto + pg_trgm） |
| Agent runtime | 本地 daemon，spawn 上述 26 个 CLI |

仓库规模（2026-09 截图）：

```
multica/server/internal/handler/  126 个非测试 handler + 300+ 测试
multica/server/internal/service/  120 个 service 文件
multica/server/migrations/        534 个迁移文件（up + down 共 1068）
multica/server/pkg/               12 个子包
multica/apps/                     5 个应用（web / desktop / mobile / docs / ui-lab）
multica/packages/                 6 个共享 TS 包（core / ui / views / ...）
```

`paperclip-rs` 已经是同一类项目（Rust 改写 paperclip）的工作区，有 108 个 crate、56 路由、
208 个迁移和一套经实践验证的 crate 分层：
`pc-config → pc-errors → pc-telemetry → pc-db → pc-core → pc-repos → pc-http → pc-server`。

## 2. 复用分析

`paperclip-rs` 与 multica 解决的问题不同（前者是 agent 后端，后者是"团队工作空间 + 多 runtime 派单"），
但**底层基建几乎完全可复用**：

| paperclip-rs crate / 机制 | multica-rs 复用方式 |
| --- | --- |
| `pc-config`（env + .env → 强类型 Config） | 直接改名 `mc-config`，env 前缀 `PAPERCLIP_*` → `MULTICA_*`，新增 runtime/channel/agent 段 |
| `pc-errors`（thiserror + HTTP 状态映射） | 直接改名 `mc-errors`，把 CHI 错误约定映射进来 |
| `pc-telemetry`（tracing JSON + 横幅 + OTLP） | 整体搬迁为 `mc-telemetry`，banner 改 multica |
| `pc-db`（sqlx pool + 健康检查） | 整体搬迁，新增 `MULTICA_DB_RUN_MIGRATIONS` 开关 |
| `pc-core`（领域类型 + actor 抽象 + kameo） | 大部分保留；新增 workspace / agent / issue 领域类型 |
| `pc-auth`（session / cookie / API key） | 整体搬迁，cookie 命名改为 `multica_session`；接入 verification_code / pat 双因素 |
| `pc-authz`（资源 × 动作 × 主体） | 复用矩阵；新增 workspace-scoped 资源策略 |
| `pc-realtime`（tokio broadcast + WS） | 复用；协议新增 multica 自己的 event 字段 |
| `pc-http`（axum router + middleware） | 复用 middleware 链；router 拆给 `mc-http` |
| `pc-openapi`（OpenAPI 3.1 生成） | 复用生成器，路径对齐 multica |
| `pc-repos`（76 个 Repo） | 50% 直接复用、改名；50% 重写为 multica 实体（issue / workspace / project / agent / runtime / task / autopilot / squad / plugin） |
| `pc-migrate`（drizzle 风格 SQL） | 复用 runner；初始 534 个迁移按 multica schema 灌入 |
| `pc-plugin-protocol` / `pc-plugin-host`（JSON-RPC over stdio） | 整体复用；事件 envelope 与 multica `packages/plugin-sdk` 对齐 |
| `pc-adapter-*`（11 个内置 adapter） | 仅 `pi-local` 直接复用；其余替换为 multica 的 26 个 runtime profile |
| `pc-agent`（agent supervisor） | 复用骨架；payload 改为 multica task / issue 模型 |
| `pc-heartbeat` / `pc-workflow` | 复用；驱动 multica 的 autopilot / scheduled run |
| `pc-storage` / `pc-secrets` | 整体复用；增加 MCP secret 与插件安装 secret |
| `pc-backup` | 整体复用 |
| `pc-cli`（paperclipai 二进制） | 重命名为 `multica`，子命令对齐 `multica` CLI |

**复用率估计**：~55%（基础设施 + 协议 + plugin IPC），其余 ~45% 是 multica 领域层的全新实现。

## 3. 多出来的能力（必须新增）

Multica 与 paperclip 不重合的部分：

| multica 领域 | 新 crate |
| --- | --- |
| Workspace / Member / Invitation / Seat | `mc-workspace` `mc-member` `mc-invitation` `mc-seat-capacity` |
| Runtime（26 种 CLI profile + daemon pair） | `mc-runtime` `mc-runtime-profile` `mc-runtime-app` `mc-runtime-blocklist` |
| Task Queue（agent task dispatch + retry） | `mc-task-queue` `mc-task-lifecycle` `mc-task-usage` `mc-task-actor` |
| Chat / Chat Session / Chat Message / Chat Draft | `mc-chat` `mc-chat-session` `mc-chat-message` `mc-chat-draft` |
| Comment（含 reactions / parent / resolved / triage） | `mc-comment` `mc-comment-reaction` `mc-comment-thread` |
| Inbox（item / archive / preferences） | `mc-inbox` `mc-inbox-archive` `mc-inbox-preference` |
| Project（CRUD + resource + view） | `mc-project` `mc-project-resource` `mc-project-view` |
| Issue view（filter / group / facet / variant） | `mc-issue-view` `mc-issue-view-preference` |
| Status catalog（workspace 自定义 status） | `mc-issue-status` |
| Squad（leader 路由） | `mc-squad` |
| Autopilot（cron + webhook + quota） | `mc-autopilot` `mc-autopilot-cron` `mc-autopilot-webhook` `mc-autopilot-quota` |
| Wakeup（事件唤醒 + 时间唤醒） | `mc-wakeup` `mc-wakeup-event` `mc-wakeup-receipt` |
| Skill（structured skill + bundle + refresh） | `mc-skill` `mc-skill-bundle` `mc-skill-refresh` |
| Plugin（multica 自己的 plugin-sdk 协议） | `mc-plugin-protocol` `mc-plugin-host` `mc-plugin-package` `mc-plugin-hook` `mc-plugin-storage` `mc-plugin-secret` |
| Channel（Slack / Lark / DingTalk / WeCom / Telegram / 自定义） | `mc-channel` `mc-channel-slack` `mc-channel-lark` `mc-channel-dingtalk` `mc-channel-wecom` `mc-channel-telegram` `mc-channel-media` |
| GitHub / VCS integration | `mc-vcs` `mc-github` |
| MCP（remote MCP server / config） | `mc-mcp` `mc-workspace-mcp` |
| Self-host telemetry / instance state | `mc-instance` `mc-instance-telemetry` |
| Maintenance cron（list-tailing 等） | `mc-maintenance` |
| Activity log（语义 + 频次） | `mc-activity` |
| File / Attachment / Image / Quick action | `mc-file` `mc-attachment` `mc-quick-action` |
| Avatar | `mc-avatar` |
| Notification preference | `mc-notification` |
| Onboarding / Starter content / Mika agent | `mc-onboarding` `mc-mika` |
| Cloud（billing / runtime / waitlist） | `mc-cloud` |
| Property / Label / Reaction | `mc-property` `mc-label` `mc-reaction` |
| Reserved slug / Share link / Source context | `mc-reserved-slug` `mc-share-link` `mc-source-context` |
| Webhook delivery / Rate limit | `mc-webhook` `mc-webhook-rate-limit` |
| Personal access token / verification code | `mc-pat` `mc-verification-code` |
| Contact sales / Feedback | `mc-contact-sales` `mc-feedback` |
| Attribution / Admission / Self-exec | `mc-attribution` `mc-admission` `mc-self-exec` |
| Agent builder / Conversation starters | `mc-agent-builder` `mc-agent-starters` |
| Composio / Composable tool surface | `mc-composio` `mc-plugin-surface` |

合计新增 ~70 个 crate。

## 4. 目标工作区布局

```
multica-rs/
├── crates/                # Cargo workspace
│   ├── mc-config          # 强类型 Config（env + .env）
│   ├── mc-errors          # 统一错误 + HTTP 状态映射
│   ├── mc-telemetry       # tracing + 启动横幅
│   ├── mc-db              # sqlx 连接池 + 迁移 runner
│   ├── mc-core            # 领域类型 / 不变量 / actor 抽象
│   ├── mc-repos           # 仓储层（1 文件 = 1 Repo）
│   ├── mc-migrate         # CLI：migrate / diff / lint
│   ├── mc-storage         # local-disk / s3
│   ├── mc-secrets         # 本地加密 / AWS Secrets Manager
│   ├── mc-auth            # session / cookie / API key
│   ├── mc-authz           # 资源 × 动作 × 主体
│   ├── mc-http            # axum 路由 + middleware
│   ├── mc-openapi         # OpenAPI 3.1 生成
│   ├── mc-realtime        # tokio broadcast + WS
│   ├── mc-ws              # WebSocket handler
│   ├── mc-heartbeat       # heartbeat supervisor
│   ├── mc-task-queue      # agent task dispatch
│   ├── mc-agent           # agent supervisor
│   ├── mc-cron            # scheduled run
│   ├── mc-workflow        # routines + pipelines
│   ├── mc-workspace       # workspace CRUD
│   ├── mc-member          # workspace member
│   ├── mc-invitation      # workspace invitation
│   ├── mc-runtime         # runtime profile + daemon pair
│   ├── mc-issue           # issue + comment + view
│   ├── mc-issue-status    # status catalog
│   ├── mc-project         # project + view
│   ├── mc-agent-builder   # agent builder + starters
│   ├── mc-skill           # structured skill
│   ├── mc-squad           # squad leader routing
│   ├── mc-autopilot       # autopilot + cron + webhook + quota
│   ├── mc-wakeup          # wakeup event/time
│   ├── mc-chat            # chat session + message
│   ├── mc-inbox           # inbox + archive
│   ├── mc-comment         # comment thread + reaction
│   ├── mc-notification    # notification preference
│   ├── mc-channel         # channel adapters
│   ├── mc-channel-{slack,lark,dingtalk,wecom,telegram}
│   ├── mc-vcs / mc-github # VCS integration
│   ├── mc-mcp             # MCP
│   ├── mc-plugin-{protocol,host,package,hook,storage,secret}
│   ├── mc-activity        # activity log
│   ├── mc-feature-flags   # feature flag catalog
│   ├── mc-instance        # instance telemetry / state
│   ├── mc-maintenance     # maintenance cron
│   ├── mc-attribution      # task attribution
│   ├── mc-admission       # admission / mul guard
│   ├── mc-onboarding      # onboarding
│   ├── mc-cloud           # cloud billing / waitlist
│   ├── mc-feedback        # feedback
│   ├── mc-property        # issue property / label
│   ├── mc-share-link      # share link
│   ├── mc-source-context  # source context
│   ├── mc-file / mc-avatar # files / avatars
│   ├── mc-quick-action    # quick action
│   ├── mc-webhook         # webhook delivery
│   ├── mc-attachment      # attachment
│   ├── mc-verification    # verification code / pat
│   ├── mc-self-exec       # self-exec guard
│   ├── mc-portability     # portability (companies.sh 兼容)
│   ├── mc-composio        # composio
│   ├── mc-plugin-surface  # plugin UI surface
│   ├── mc-reserved-slug   # reserved slug
│   ├── mc-runtime-blocklist / mc-runtime-apps
│   ├── mc-adapter-{claude-code,codex,cursor,kimi,…}  # 26 个 runtime profile
│   └── mc-runtime-host    # daemon spawn / IPC
├── apps/
│   ├── mc-server          # multica-server 二进制
│   └── mc-cli             # multica CLI 二进制
├── migrations/            # 534 个迁移文件
├── docs/                  # 架构 / 计划 / 进度
└── Cargo.toml             # workspace 根
```

总规模目标：**~120 crate** + **~600 路由模块** + **~550 迁移文件**（multica 1:1 移植）。

## 5. 协议一致性

multica-rs 必须**行为等价**于上游 multica：
- HTTP：路径 / 方法 / 请求体 / 响应 / 错误码与 `multica/server/internal/handler/*.go` 对齐；
  OpenAPI 由 `mc-openapi` 生成。
- WebSocket：`/live-events` 通道；`last_event_id` resume；字段与上游一致。
- 数据库：534 张表的 DDL、索引、外键、check 约束；`MULTICA_DB_RUN_MIGRATIONS=false` 跳过。
- Plugin IPC：JSON-RPC 2.0 over stdio，与 `multica/packages/plugin-sdk` 对齐。
- Auth：session / cookie / API key / 个人访问令牌 / 双因素；`X-Multica-*` 头部语义不变。
- Runtime：26 个 runtime profile 与上游一一对应；CLI 调用规范一致。
- Channel：6 个内置 channel（Slack / Lark / DingTalk / WeCom / Telegram / 自定义）与上游一一对应。

UI 兼容性：`apps/web` / `apps/desktop` 仅切换 base URL 即可对接 multica-rs（与 multica 的 Next.js + React UI 兼容）。

## 6. 里程碑

| 阶段 | 时间（估） | 交付 |
| --- | --- | --- |
| **M0：基础设施搬迁** | 1–2 周 | `mc-config` `mc-errors` `mc-telemetry` `mc-db` `mc-core` `mc-auth` `mc-authz` `mc-realtime` `mc-ws` `mc-storage` `mc-secrets` `mc-backup` + 最小 server / cli 二进制 + 1 个迁移文件 |
| **M1：Workspace / Member / Auth** | 2 周 | workspace / member / invitation / PAT / verification code / 全部 auth 路由 + session / cookie |
| **M2：Issue / Comment / Inbox** | 4 周 | issue 全套（CRUD / view / status catalog / triage） + comment（含 reactions / parent / resolved） + inbox + 反应 |
| **M3：Runtime / Agent / Task Queue** | 4 周 | 26 个 runtime profile + daemon pair + agent CRUD + agent builder + task queue + lifecycle + usage + retry |
| **M4：Chat / Project / Squad** | 3 周 | chat session/message/draft + project + project view + squad briefing |
| **M5：Autopilot / Wakeup / Cron** | 3 周 | autopilot (cron + webhook + quota) + wakeup (event + time) + scheduled routines |
| **M6：Skill / Plugin** | 3 周 | structured skill + skill bundle + skill refresh + multica plugin host + package + hook + storage + secret + remote MCP |
| **M7：Channel（6 个）** | 4 周 | Slack + Lark + DingTalk + WeCom + Telegram + 自定义 channel + channel media + channel reply |
| **M8：VCS / GitHub / MCP** | 2 周 | GitHub + 自托管 VCS（GitLab / Gitea / Forgejo） + workspace MCP + agent MCP |
| **M9：Activity / Onboarding / Cloud** | 3 周 | activity log + onboarding + Mika + cloud billing + waitlist + feedback + contact sales + instance telemetry |
| **M10：UI 兼容 / 性能 / 文档** | 3 周 | OpenAPI 完整 / UI base URL 切换 E2E / 文档 / 一致性测试 / release |

总计 ~30 周 / 7 个月（单人）或 12 周 / 3 个月（3 人小队）。

## 7. 当前进度

本次提交实现：

- ✅ workspace / Cargo.toml 骨架
- ✅ crate：mc-config / mc-errors / mc-telemetry / mc-db / mc-core / mc-auth / mc-authz / mc-realtime / mc-storage / mc-secrets / mc-http / mc-repos / mc-migrate
- ✅ apps：mc-server / mc-cli
- ✅ migrations：0001_init.up.sql（建表骨架，对应 multica 001_init）
- ✅ smoke test：单测 + 集成（`mc-db::Migrator` + `mc-config::build_with`）

### 进度更新（2026-09-22 13:40 CST，LUM-1346 cycle）

- ✅ M1 scaffold 已合入 `feat/multica-rs-initial`（`056d2ae`）：六个 Repo stub + 四个 `mount_slice_*` 切片锚点。
- 🔄 **M1 三个切片并行实现中**（都基于 `056d2ae`，完成后 PR 回 `feat/multica-rs-initial`）：
  - LUM-1343 `feat/multica-rs-m1a-workspace-member`：workspace / member / user Repo + `/api/workspaces/*` + `/api/me`
  - LUM-1345 `feat/multica-rs-m1b-auth`：verification_code / pat Repo + send-code / verify-code / logout / refresh
  - LUM-1344 `feat/multica-rs-m1c-invitation-pat`：invitation Repo + `/api/invitations/*` + `/api/me/pats`
- 📦 平行实现 `feat/multica-rs-m1`（LUM-1335，`3ad402e`，in_review）：完整 M1 + share-link + cli-token + `migrations/0002`，
  集成时按 `docs/09-M1-INTEGRATION.md` 的仲裁规则 cherry-pick 增量，不整支合并。
- 📋 后续规划已就绪：M1 集成操作手册 `docs/09-M1-INTEGRATION.md`、M2 三切片计划 `docs/10-M2-PLAN.md`。

下一里程碑（M1）目标：完成 workspace / member / invitation / PAT / verification 路由与服务，
完成 auth 双因素。（M1 集成与 M2 晋升规则见 `docs/09` / `docs/10`。）

### 进度更新（2026-09-22 15:00 CST，LUM-1356 cycle）

- 🔄 M1 三切片并行运行健康（run 于 06:09–06:11 UTC 启动）：三个任务 workdir 均已进入
  `cargo build --workspace` 编译阶段（06:58 UTC 后各持续产出约 1700–2000 个 target 产物，
  依赖下载走 rsproxy 镜像完成），无 429、无 package-cache 死锁。
- ✅ GitHub 分支逐一对齐核验：`feat/multica-rs-initial` @ `9dcd2f2`、
  `feat/multica-rs-m1a-workspace-member` @ `2c4da9f`、`feat/multica-rs-m1b-auth` @ `89f5e94`、
  `feat/multica-rs-m1c-invitation-pat` @ `60c9d49`（origin 与本地一致）。
- 📋 后续队列核验完整：LUM-1347（M1-D 集成）→ LUM-1348 / 1350 / 1349（M2-A/B/C）→
  LUM-1354（M2 集成）→ LUM-1355（M2-D）；本 cycle 补立项 **LUM-1357**（M3 切片计划，
  backlog，晋升条件 = LUM-1354 完成后），补上 M2 之后的队列断档。
- ⏭ 下一 cycle（16:00 CST）动作：若 M1×3 交付（status `in_review` 且分支已 push）→
  晋升 LUM-1347 开始集成；否则继续健康核查，不硬塞新任务（并发上限 3）。

### 进度更新（2026-09-22 15:30 CST，LUM-1358 cycle）

- ✅ **M1-C（LUM-1344）已交付并 push**：`feat/multica-rs-m1c-invitation-pat` @ `d88b259`，
  `cargo build/test --workspace` 全绿（39 suites/0 failed），PG16 e2e：invitations 3/3、pats 3/3、
  invitation repo 5/5；顺带修掉 axum 0.7 的 `{id}` → `:id` 路由语法缺陷（`{id}` 被当字面量段，恒 404）。
- 🔄 M1-A（LUM-1343）/ M1-B（LUM-1345）继续运行、无 429：A 已完成 workspace/member/user 三 Repo +
  session 中间件（新目录 `mc-http/src/middleware/`），**routes 尚未开始**；B 已落 `routes/auth.rs` 5 条路由
  + pat/verification_code Repo。
- 🔧 **工具链事实更正**：`/usr/bin/cargo` 是 1.75.0，**无法构建本仓库**（`rust-version = "1.80"`）；
  实际可用的是 `~/.cargo/bin` 下的 rustup stable **1.98.1**，构建/测试必须
  `PATH="$HOME/.cargo/bin:$PATH"`。
- 🔍 **M0 基线 `056d2ae` 自身不编译**（`mc-errors` 缺 `anyhow`、`mc-auth` 用 `crate::store` 等，7 个文件）；
  三个切片各自独立做了**逐字节相同**的修复 → 集成时这些文件属"假冲突"，取任一方即可。
- 📌 `docs/09-M1-INTEGRATION.md` 新增第 7 节（实测增量）：切片状态、22 个共有文件的分类、
  4 个真分歧文件（`redact.rs` / `migrate.rs` / `state.rs` / `mc-http/Cargo.toml`）的仲裁建议、
  axum 路由语法扫查命令、`/api/me` 重复注册点。
- ⏭ 下一 cycle（16:00 CST）：M1×3 全部交付（`in_review` + 分支已 push）才晋升 LUM-1347；
  A 是 M1 关键路径，若仍无路由落地需重点跟进。

### 进度更新（2026-09-22 16:00 CST，LUM-1359 cycle）

- ✅ **M1 三切片全部交付**（GitHub 实测 head）：
  A（LUM-1343）`feat/multica-rs-m1a-workspace-member` @ `f2e2e8b` / PR #3；
  B（LUM-1345）`feat/multica-rs-m1b-auth` @ `cce354c` / PR #2；
  C（LUM-1344）`feat/multica-rs-m1c-invitation-pat` @ `d88b259` / PR #1。
  三个切片的 run 均已结束，`--siblings --active` 只剩协调 run → 并发位空出。
- 🚀 **晋升 LUM-1347（M1-D 集成）**：晋升条件（三子 issue `in_review` + 分支已 push + 并发位空 + 无 cargo 抢锁）全部满足。
- 🔬 **axum 0.7.9 `.merge` 实测**（同版本最小复现，非推断）：同 path + 同 method 重复注册
  **panic**（`Overlapping method route`）；同 path 不同 method 可合并；字面量 `{id}` 与 `:id` 可共存（静默 404）。
  → 合并后必须删掉 B 的 `GET /api/me` 占位，并以 A 的 `mount.rs::router()`（已删 `/api/workspaces` 系列占位）为底。
- 🔍 上游路由前缀核对：`/auth/{send-code,verify-code,logout}` 无 `/api` 前缀（`server/cmd/server/router.go:1472-1475`），
  B 的实现与上游一致，集成时**不要**"统一"成 `/api/auth/*`。
- ⏭ 下一 cycle（16:30 CST）：核查 LUM-1347 集成进度（merge 结果 / 四条验证命令输出 / CI），
  集成合入后按其评论结论晋升 M2 三切片（LUM-1348 / 1350 / 1349，三路并行 = 并发上限）。

## 8. 风险与权衡

| 风险 | 缓解 |
| --- | --- |
| multica schema 持续演化（已 534 个迁移，仍在增长） | 迁移 runner 设计为支持按文件追加；CI 增加 schema drift 检查 |
| 26 个 runtime CLI 的运行时差异大 | runtime profile trait 抽象 + 自检开关，避免硬编码 |
| Channel 实现复杂（Lark / DingTalk 等各家 API） | 按 channel crate 拆分；先实现 Slack（已熟悉），其余逐个迁移 |
| Plugin JSON-RPC 与上游 SDK 差异 | 维护 fixture 测试集；与 upstream sdk 同号调用 |
| 性能 / 启动时间 | axum 路由 lazy 注册；release profile 用 LTO + strip |
| 单人工作量过大 | 阶段性裁剪：M0→M4 是 MVP；M5+ 是 feature parity |

## 9. 立项原则

- **协议一致性优先**：任何行为变更都要先在 OpenAPI / fixtures 里固化，再实现。
- **复用 paperclip-rs 的 70%**：避免重复造轮子，但允许 rename / 重组。
- **一文件一 Repo**：避免 1000 行的 god file；每个 Repo 单一职责。
- **unsafe_code = "forbid"**、**clippy pedantic** 与 paperclip-rs 一致。
- **OpenSpec 提案先行**：每个大改动先有 `openspec/changes/<id>/proposal.md`。

——

下一步：实现 M0 全套与 M1 的 workspace / member / invitation / PAT / verification code。