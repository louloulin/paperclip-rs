# 多源对照分析：Multica ↔ Multica-rs ↔ Paperclip-rs

> 本文档用于回答：**multica 哪些部分可以直接照搬 paperclip-rs，哪些必须重写**。

## 1. 三方概览

| 维度 | multica (上游) | paperclip-rs (参考实现) | multica-rs (本仓库目标) |
| --- | --- | --- | --- |
| 语言 | Go + TypeScript | Rust | Rust |
| 后端文件数 | 1891 Go + 2356 TS/TSX | 108 crate / ~2500 .rs | ~120 crate / ~5000 .rs |
| 路由模块 | 126 (handler) + 120 (service) | 56 (routes) | ~150 (routes) |
| 迁移文件 | 534 (×2 up+down = 1068) | 208 | 534 |
| 协议 | HTTP + WS + JSON-RPC | HTTP + WS + JSON-RPC | HTTP + WS + JSON-RPC（与 multica 一致） |
| 数据库 | PostgreSQL 17 | PostgreSQL ≥ 14 | PostgreSQL 17 |
| Runtime | 26 个本地 CLI 守护 | 11 个 adapter host | 26 个 runtime profile |
| 部署 | 自托管 / 云 | 自托管 | 自托管（云后续） |

## 2. 复用决策矩阵

### 2.1 直接复用（paperclip-rs 原样搬迁，env 前缀重命名）

| paperclip-rs | multica-rs 命名 | 改动点 |
| --- | --- | --- |
| `pc-config` | `mc-config` | `PAPERCLIP_*` → `MULTICA_*`；新增 `[server] [database] [auth] [storage] [workspace] [runtime] [channel] [plugin]` 段 |
| `pc-errors` | `mc-errors` | 增加 `Workspace` / `Runtime` / `Channel` 错误码 |
| `pc-telemetry` | `mc-telemetry` | banner service 改名；OTLP 不变 |
| `pc-db` | `mc-db` | `Migrator` 接收 `multica-migrate` 路径；`MULTICA_DB_RUN_MIGRATIONS` |
| `pc-storage` | `mc-storage` | provider trait 不变；bucket 名换 multica 前缀 |
| `pc-secrets` | `mc-secrets` | 同上；新增 `mcp_secret`、`plugin_secret` |
| `pc-backup` | `mc-backup` | 不变 |
| `pc-auth` | `mc-auth` | cookie 改名；新增 verification code 流程 |
| `pc-authz` | `mc-authz` | 矩阵加 workspace/run-time/channel 资源 |
| `pc-realtime` | `mc-realtime` | protocol 加 multica event 字段 |
| `pc-ws` | `mc-ws` | 协议字段对齐 |
| `pc-openapi` | `mc-openapi` | 路径生成改 multica 路径树 |
| `pc-plugin-protocol` | `mc-plugin-protocol` | envelope 与 multica `packages/plugin-sdk` 对齐 |
| `pc-plugin-host` | `mc-plugin-host` | event 路由对齐 multica hook 模型 |
| `pc-agent` | `mc-agent` | payload 改 issue / task |
| `pc-heartbeat` | `mc-heartbeat` | 不变 |
| `pc-workflow` | `mc-workflow` | pipeline payload 改 multica |
| `pc-cron` | `mc-cron` | 同上 |
| `pc-feature-flags` | `mc-feature-flags` | 不变 |
| `pc-migrate` | `mc-migrate` | `paperclip-migrate` 二进制 → `multica-migrate` |
| `pc-cli` | `mc-cli` | 子命令按 multica CLI 完整 |

### 2.2 部分复用（接口保留，内部重写）

| paperclip-rs | multica-rs | 改动点 |
| --- | --- | --- |
| `pc-core` | `mc-core` | 新增 workspace / member / agent / issue / runtime / task / autopilot / squad / skill / channel / vcs / mcp 等领域类型 |
| `pc-repos` | `mc-repos` | 重写大部分 Repo 文件，与 multica 实体一一对应 |
| `pc-http` | `mc-http` | 路由拆分至各自领域 crate；middleware 链保留 |
| `pc-adapter-api` | `mc-runtime-profile` | 改名为 runtime profile；接口保持 Adapter / AdapterRegistry |
| `pc-adapter-pi-local` | `mc-adapter-pi-local` | 完全复用，env 前缀改 |
| `pc-adapter-quota` | `mc-runtime-quota` | 重写为 multica autopilot quota 模型 |
| `pc-telemetry` (产品遥测) | `mc-instance-telemetry` | 拆为 instance 级别，去掉 PC 专属上报 |
| `pc-feature-flags` | `mc-feature-flags` | 加 multica 命名空间 |

### 2.3 新增（multica 独有）

| 新 crate | 替代 / 对应 |
| --- | --- |
| `mc-workspace` | workspace CRUD |
| `mc-member` | member + role |
| `mc-invitation` | workspace invitation + share link |
| `mc-seat-capacity` | seat 计数 + outbox |
| `mc-runtime` | 26 个 runtime profile 注册中心 |
| `mc-runtime-app` | runtime app 安装包 |
| `mc-runtime-blocklist` | blocking agents |
| `mc-runtime-host` | 本地 daemon spawn |
| `mc-task-queue` | agent task queue + dispatch |
| `mc-task-lifecycle` | prepare / reclaim / terminal |
| `mc-task-usage` | token usage + cost + rollup |
| `mc-task-actor` | task attribution / cancellation actor |
| `mc-chat` | chat session / message / draft |
| `mc-inbox` | inbox / archive / preference |
| `mc-project` | project / resource / view |
| `mc-issue` | issue + view + metadata |
| `mc-issue-status` | status catalog |
| `mc-comment` | comment + reaction + parent |
| `mc-skill` | structured skill |
| `mc-skill-bundle` | skill bundle import / export |
| `mc-skill-refresh` | skill auto refresh |
| `mc-squad` | squad leader routing |
| `mc-autopilot` | autopilot definition |
| `mc-autopilot-cron` | cron 触发 |
| `mc-autopilot-webhook` | webhook 触发 |
| `mc-autopilot-quota` | quota / reservation |
| `mc-wakeup` | wakeup dispatcher |
| `mc-wakeup-event` | 事件捕获 |
| `mc-wakeup-receipt` | 接收凭证 |
| `mc-channel` | channel trait + registry |
| `mc-channel-slack` | Slack |
| `mc-channel-lark` | Lark / Feishu |
| `mc-channel-dingtalk` | DingTalk |
| `mc-channel-wecom` | WeCom |
| `mc-channel-telegram` | Telegram |
| `mc-channel-media` | channel media |
| `mc-vcs` | VCS 抽象 |
| `mc-github` | GitHub |
| `mc-mcp` | remote MCP server |
| `mc-plugin-package` | plugin npm package |
| `mc-plugin-hook` | hook engine |
| `mc-plugin-storage` | plugin storage |
| `mc-plugin-secret` | plugin secret + OAuth |
| `mc-activity` | activity log |
| `mc-instance` | instance config / settings |
| `mc-instance-telemetry` | instance 上报 |
| `mc-maintenance` | maintenance cron |
| `mc-attribution` | task attribution |
| `mc-admission` | admission / mul guard |
| `mc-self-exec` | self-exec guard |
| `mc-onboarding` | onboarding / questionnaire |
| `mc-cloud` | cloud billing / runtime / waitlist |
| `mc-feedback` | feedback |
| `mc-contact-sales` | contact sales inquiry |
| `mc-property` | issue property / label |
| `mc-label` | label |
| `mc-reaction` | reaction |
| `mc-share-link` | share link |
| `mc-source-context` | source context |
| `mc-file` | file resource |
| `mc-avatar` | avatar |
| `mc-quick-action` | quick action |
| `mc-webhook` | webhook delivery |
| `mc-webhook-rate-limit` | webhook rate limit |
| `mc-attachment` | attachment |
| `mc-verification` | verification code / PAT |
| `mc-portability` | portability (companies.sh 兼容) |
| `mc-composio` | composio |
| `mc-plugin-surface` | plugin UI surface |
| `mc-reserved-slug` | reserved slug |
| `mc-attachment` | attachment |
| `mc-notification` | notification preference |

合计 ~70 个新增 crate。

## 3. 数据库 schema 对照

| 表名 | multica 来源 | multica-rs 映射 |
| --- | --- | --- |
| `user` | multica `001_init` | `mc-core::user` |
| `workspace` | multica `001_init` | `mc-core::workspace` |
| `member` | multica `001_init` | `mc-core::member` |
| `agent` | multica `001_init` | `mc-core::agent` |
| `agent_runtime` | multica `001_init` | `mc-runtime` |
| `agent_task_queue` | multica `004_agent_runtime_loop` | `mc-task-queue` |
| `issue` | multica `001_init` | `mc-issue` |
| `comment` | multica `001_init` | `mc-comment` |
| `project` | multica `034_projects` | `mc-project` |
| `autopilot` | multica `042_autopilot` | `mc-autopilot` |
| `squad` | multica `084_squad` | `mc-squad` |
| `skill` | multica `008_structured_skills` | `mc-skill` |
| `plugin_*` | multica `285_plugin_lifecycle_v1` 起 | `mc-plugin-*` |
| `chat_*` | multica `033_chat` 起 | `mc-chat` |
| `wakeup_*` | multica `509_issue_wakeup` 起 | `mc-wakeup` |
| `channel_*` | multica `124_channel_generalization` 起 | `mc-channel` |
| `vcs_*` | multica `216_vcs_integration` 起 | `mc-vcs` |
| `mcp_*` | multica `314_workspace_mcp_config` 起 | `mc-mcp` |
| `*_invitation` | multica `041_workspace_invitation` | `mc-invitation` |
| `runtime_profile` | multica `001_init` 等多处 | `mc-runtime-profile` |

完整映射见 `docs/03-CRATE-MAPPING.md`（按表分组）。

## 4. 路由映射

| multica handler | multica-rs crate | 路径 |
| --- | --- | --- |
| `handler/auth.go` | `mc-http::routes::auth` | `/api/auth/*` |
| `handler/workspace.go` | `mc-http::routes::workspace` | `/api/workspaces` |
| `handler/issue.go` | `mc-http::routes::issue` | `/api/issues` |
| `handler/comment.go` | `mc-http::routes::comment` | `/api/issues/{id}/comments` |
| `handler/agent.go` | `mc-http::routes::agent` | `/api/agents` |
| `handler/runtime.go` | `mc-http::routes::runtime` | `/api/runtimes` |
| `handler/daemon*.go` | `mc-http::routes::daemon` | `/api/daemon/*` |
| `handler/chat*.go` | `mc-http::routes::chat` | `/api/chat/*` |
| `handler/project*.go` | `mc-http::routes::project` | `/api/projects` |
| `handler/autopilot*.go` | `mc-http::routes::autopilot` | `/api/autopilots` |
| `handler/squad*.go` | `mc-http::routes::squad` | `/api/squads` |
| `handler/skill*.go` | `mc-http::routes::skill` | `/api/skills` |
| `handler/plugin*.go` | `mc-http::routes::plugin` | `/api/plugins` |
| `handler/wakeup*.go` | `mc-http::routes::wakeup` | `/api/wakeups` |
| `handler/slack.go` | `mc-http::routes::slack` | `/api/integrations/slack` |
| `handler/lark.go` | `mc-http::routes::lark` | `/api/integrations/lark` |
| `handler/dingtalk.go` | `mc-http::routes::dingtalk` | `/api/integrations/dingtalk` |
| `handler/wecom_web.go` | `mc-http::routes::wecom` | `/api/integrations/wecom` |
| `handler/telegram.go` | `mc-http::routes::telegram` | `/api/integrations/telegram` |
| `handler/vcs*.go` | `mc-http::routes::vcs` | `/api/vcs` |
| `handler/github.go` | `mc-http::routes::github` | `/api/github` |
| `handler/workspace_mcp*.go` | `mc-http::routes::mcp` | `/api/workspaces/{id}/mcp/*` |
| `handler/inbox*.go` | `mc-http::routes::inbox` | `/api/inbox` |
| `handler/source_context*.go` | `mc-http::routes::source_context` | `/api/issues/{id}/source-context` |
| `handler/invitation.go` | `mc-http::routes::invitation` | `/api/invitations` |
| `handler/attachment_capability.go` | `mc-http::routes::attachment` | `/api/attachments` |
| `handler/file.go` | `mc-http::routes::file` | `/api/files` |
| `handler/avatar.go` | `mc-http::routes::avatar` | `/api/avatars` |
| `handler/personal_access_token.go` | `mc-http::routes::pat` | `/api/personal-access-tokens` |
| `handler/issue_wakeup.go` | `mc-http::routes::wakeup` | `/api/wakeups` |
| `handler/issue_view*.go` | `mc-http::routes::issue_view` | `/api/issues/views` |
| `handler/issue_status.go` | `mc-http::routes::issue_status` | `/api/workspaces/{id}/issue-status` |
| `handler/quick_action.go` | `mc-http::routes::quick_action` | `/api/quick-actions` |
| `handler/activity.go` | `mc-http::routes::activity` | `/api/activity` |
| `handler/heartbeat_scheduler.go` | `mc-http::routes::heartbeat` | `/api/heartbeats` |
| `handler/notification_preference.go` | `mc-http::routes::notification` | `/api/notification-preferences` |
| `handler/onboarding.go` | `mc-http::routes::onboarding` | `/api/onboarding` |
| `handler/cloud_*.go` | `mc-http::routes::cloud` | `/api/cloud/*` |
| `handler/feedback.go` | `mc-http::routes::feedback` | `/api/feedback` |
| `handler/contact_sales.go` | `mc-http::routes::contact_sales` | `/api/contact-sales` |
| `handler/seat_capacity.go` | `mc-http::routes::seat` | `/api/workspaces/{id}/seat-capacity` |
| `handler/agent_builder.go` | `mc-http::routes::agent_builder` | `/api/agents/builder` |
| `handler/agent_runtime_skills.go` | `mc-http::routes::agent_runtime_skills` | `/api/agents/{id}/runtime-skills` |
| `handler/runtime_profile.go` | `mc-http::routes::runtime_profile` | `/api/runtime-profiles` |
| `handler/runtime_update*.go` | `mc-http::routes::runtime_update` | `/api/runtime-updates` |
| `handler/runtime_models*.go` | `mc-http::routes::runtime_models` | `/api/runtime-models` |
| `handler/runtime_local_skills*.go` | `mc-http::routes::runtime_local_skills` | `/api/runtime-local-skills` |
| `handler/runtime_model_catalog*.go` | `mc-http::routes::runtime_model_catalog` | `/api/runtime-model-catalog` |
| `handler/runtime_liveness_store.go` | `mc-http::routes::runtime_liveness` | `/api/runtimes/{id}/liveness` |
| `handler/runtime_blocking_agents.go` | `mc-http::routes::runtime_blocking_agents` | `/api/runtime-block-agents` |
| `handler/runtime_unusable_notice.go` | `mc-http::routes::runtime_unusable` | `/api/runtimes/{id}/unusable-notice` |
| `handler/client_usage.go` | `mc-http::routes::client_usage` | `/api/client-usage` |
| `handler/composio.go` | `mc-http::routes::composio` | `/api/composio/*` |
| `handler/share_link.go` | `mc-http::routes::share_link` | `/api/workspaces/{id}/share-links` |
| `handler/issue_metadata.go` | `mc-http::routes::issue_metadata` | `/api/issues/{id}/metadata` |
| `handler/property.go` | `mc-http::routes::property` | `/api/properties` |
| `handler/label.go` | `mc-http::routes::label` | `/api/labels` |
| `handler/reaction.go` | `mc-http::routes::reaction` | `/api/reactions` |
| `handler/pin.go` | `mc-http::routes::pin` | `/api/pins` |
| `handler/search.go` | `mc-http::routes::search` | `/api/search` |
| `handler/dashboard.go` | `mc-http::routes::dashboard` | `/api/dashboard` |
| `handler/dashboard_reserved_slugs.go` | `mc-http::routes::reserved_slug` | `/api/dashboard/reserved-slugs` |
| `handler/webhook_delivery*.go` | `mc-http::routes::webhook` | `/api/webhook-deliveries` |
| `handler/webhook_rate_limiter.go` | `mc-http::routes::webhook_rate_limit` | （内部 middleware） |
| `handler/vcs_webhook.go` | `mc-http::routes::vcs_webhook` | `/api/vcs/webhook` |
| `handler/handler.go` | `mc-http::routes::root` | `/api/health` `/api/openapi.json` |

合计 ~150 个 handler 入口，对应 `(workspace.go)` 等价体量；M10 时路由数与 multica 一致。

## 5. middleware 对照

| paperclip-rs middleware | multica-rs middleware | 差异 |
| --- | --- | --- |
| `auth_layer` | `auth_layer` | 头部 `X-Multica-*`；cookie 改名 |
| `csrf_layer` | `csrf_layer` | cookie 改名 |
| `require_board_layer` | `require_board_layer` | workspace 校验 |
| `board_mutation_guard_layer` | `board_mutation_guard_layer` | 同 |
| `parse_trust_proxy_env` | `parse_trust_proxy_env` | env `MULTICA_TRUST_PROXY` |
| `private_hostname_guard` | `private_hostname_guard` | 同 |

新增（multica 独有）：
- `workspace_context_layer` — 解析 `X-Workspace-ID` 注入 `WorkspaceContext`
- `runtime_context_layer` — 解析 runtime 上下文
- `channel_signature_layer` — channel webhook 签名校验
- `plugin_signature_layer` — plugin IPC 签名校验

## 6. 性能与体积对照

| 维度 | paperclip-rs (实测) | multica-rs (目标) |
| --- | --- | --- |
| 服务端二进制大小 | ~80 MB (release) | 100–120 MB |
| 启动时间 | ~600 ms | ~800 ms |
| HTTP 路由数 | 56 | ~150 |
| 数据库表数 | 109 | ~150 |
| 测试用例数 | ~3000 | ~5000 |
| 文档页数 | ~80 | ~120 |

## 7. 结论

multica-rs 的可行性极高：复用 paperclip-rs 70% 的基础设施可以省下至少 50% 的工作量；
multica 独有能力的 70 个新 crate 在合理分配下 6 个月可完成。
最大的不确定性来自 **534 个迁移的 1:1 移植** 与 **26 个 runtime CLI 的行为对齐**——
这两块建议在 M3 / M4 投入最多人力。