# 多对一迁移：paperclip-rs → multica-rs

> 把 paperclip-rs 中的 crate 直接复用 / 部分复用 / 重写 / 删除 / 新增的最终归宿。

## 1. 直接复用

| paperclip-rs | multica-rs |
| --- | --- |
| `crates/pc-config` | `crates/mc-config` |
| `crates/pc-errors` | `crates/mc-errors` |
| `crates/pc-telemetry` | `crates/mc-telemetry` |
| `crates/pc-db` | `crates/mc-db` |
| `crates/pc-storage` | `crates/mc-storage` |
| `crates/pc-secrets` | `crates/mc-secrets` |
| `crates/pc-backup` | `crates/mc-backup` |
| `crates/pc-auth` | `crates/mc-auth` |
| `crates/pc-authz` | `crates/mc-authz` |
| `crates/pc-realtime` | `crates/mc-realtime` |
| `crates/pc-ws` | `crates/mc-ws` |
| `crates/pc-openapi` | `crates/mc-openapi` |
| `crates/pc-plugin-protocol` | `crates/mc-plugin-protocol` |
| `crates/pc-plugin-host` | `crates/mc-plugin-host` |
| `crates/pc-agent` | `crates/mc-agent` |
| `crates/pc-heartbeat` | `crates/mc-heartbeat` |
| `crates/pc-workflow` | `crates/mc-workflow` |
| `crates/pc-cron` | `crates/mc-cron` |
| `crates/pc-feature-flags` | `crates/mc-feature-flags` |
| `crates/pc-migrate` | `crates/mc-migrate` |
| `crates/pc-app-definitions` | `crates/mc-app-definitions` |
| `crates/pc-typescript-gen` | `crates/mc-typescript-gen` |
| `apps/pc-server` | `apps/mc-server` |
| `apps/pc-cli` | `apps/mc-cli` |

## 2. 部分复用（按目录 / trait 改造）

| paperclip-rs | multica-rs | 改造点 |
| --- | --- | --- |
| `crates/pc-core` | `crates/mc-core` | 新增 50+ 个领域类型（workspace / member / issue / agent / runtime / task / autopilot / squad / skill / channel / vcs / mcp / plugin / chat / inbox / wakeup / comment） |
| `crates/pc-repos` | `crates/mc-repos` | 重写 80% 文件；保留 Repo trait；删除 paperclip 专属的 `companies` `goals` 等 |
| `crates/pc-http` | `crates/mc-http` | middleware 链保留；router 拆给各领域 crate 注入 |
| `crates/pc-adapter-api` | `crates/mc-runtime-profile` | AdapterRegistry 改为 RuntimeProfileRegistry |
| `crates/pc-adapter-quota` | `crates/mc-runtime-quota` | payload 改 multica quota 模型 |
| `crates/pc-adapter-pi-local` | `crates/mc-adapter-pi-local` | 完全复用，env 前缀改 |
| `crates/pc-telemetry::product_telemetry` | `crates/mc-instance-telemetry` | 拆分为 instance 级别 |
| `crates/pc-feature-flags` | `crates/mc-feature-flags` | namespace 加 multica. |

## 3. 删除（multica 用不到）

| paperclip-rs | 原因 |
| --- | --- |
| `crates/pc-companies` | multica 用 workspace 而非 company |
| `crates/pc-company-member` | 同 |
| `crates/pc-folders` | multica 用 project / view |
| `crates/pc-goals` | multica 用 squad |
| `crates/pc-pipelines` | multica 用 autopilot |
| `crates/pc-routines` | multica 用 autopilot / cron |
| `crates/pc-routine-variables` | 同 |
| `crates/pc-projects` | multica 用 `mc-project`（语义不同） |
| `crates/pc-portability` | multica 用 `mc-portability`（基于 companies.sh） |
| `crates/pc-portability-zip` / `pc-portability-hash` / `pc-portability-fidelity` | 同 |
| `crates/pc-execution` / `pc-execution-workspace-guards` / `pc-execution-policy-bootstrap` | multica 用 `mc-task-lifecycle` |
| `crates/pc-document-anchors` / `pc-documents` / `pc-frontmatter` | multica 用 `mc-source-context` |
| `crates/pc-budgets` / `pc-costs` | multica 用 `mc-task-usage` |
| `crates/pc-approvals` / `pc-responsible-user-denial` | multica 用 `mc-attribution` |
| `crates/pc-decisions` / `pc-decision-training` | multica 用 `mc-task-actor` |
| `crates/pc-pipeline-case-type` / `pc-pipeline-case-outputs` / `pc-pipeline-conversation-context` / `pc-pipeline-health` | paperclip 概念 |
| `crates/pc-github-fetch` / `pc-github-external-objects` | multica 用 `mc-github` |
| `crates/pc-issue-references` / `pc-issue-attribution` / `pc-execution-allowlist` | multica 用 `mc-attribution` / `mc-issue` |
| `crates/pc-environment` / `pc-environment-redaction` / `pc-environment-support` | multica 用 `mc-runtime` 配置 |
| `crates/pc-tool` / `pc-tool-profile-binding` | multica 用 `mc-runtime-profile` |
| `crates/pc-trust-policy` / `pc-source-trust` / `pc-source-trust-resolver` / `pc-trust-preset-resolver` | multica 用 `mc-attribution` |
| `crates/pc-status-card-update-engine` | multica 用 `mc-issue` 字段 |
| `crates/pc-work-products` | multica 用 `mc-task-usage` 输出 |
| `crates/pc-run-liveness` / `pc-run-log-store` | multica 用 `mc-runtime-liveness` |
| `crates/pc-hot-restart` | multica 用 `mc-runtime` 重启 |
| `crates/pc-invite` | multica 用 `mc-invitation` |
| `crates/pc-agent-eligibility` / `pc-agent-jwt` / `pc-board-auth` | multica 用 `mc-agent` + `mc-auth` |
| `crates/pc-mentions` | multica 用 `mc-comment` / `mc-inbox` |
| `crates/pc-sidebar` | paperclip UI 概念 |
| `crates/pc-feature-catalog` / `pc-config-schema` | multica 用 `mc-feature-flags` + `mc-runtime-profile` |
| `crates/pc-log-redaction` / `pc-secret-redaction` | multica 用 `mc-telemetry::redact` |
| `crates/pc-connection-display` / `pc-url-keys` / `pc-network-bind` | multica 用 `mc-http` 内部 |
| `crates/pc-plugin-database` / `pc-plugin-state-store` / `pc-plugin-ui-static` | multica 用 `mc-plugin-storage` 等 |
| `crates/pc-codex-auth-reconciliation` / `pc-feedback` | multica 用 `mc-runtime` + `mc-feedback` |
| `crates/pc-acpx` | multica 不使用此工具 |
| `crates/pc-managed-config` | multica 用 `mc-instance` |
| `crates/pc-storage` / `pc-secrets` | 复用（见上） |
| `crates/pc-issue-attribution` | multica 用 `mc-attribution` |
| `crates/pc-api-routes` | 合并到 `mc-http` |
| `crates/pc-constants` | 合并到 `mc-core` |
| `crates/pc-external-objects` / `pc-external-objects-server` | multica 用 `mc-source-context` |
| `crates/pc-portability` / `pc-portability-zip` / `pc-portability-hash` / `pc-portability-fidelity` | multica 用 `mc-portability`（独立设计） |
| `crates/pc-execution_workspace_branch_reconcile_assertions` / `pc-execution_workspace_config` / `pc-execution_workspace_overview` / `pc-execution_workspace_policy` / `pc-execution_workspace_row_to_typed` / `pc-workspace_branch_incoherence*` / `pc-workspace_dirty_quarantine_formatter` / `pc-workspace_file_*` / `pc-workspace_realization` / `pc-workspace_runtime_*` | multica 用 `mc-runtime` 配置 + `mc-task-queue` |
| `crates/pc-feature_flags::rules` | 合并到 `mc-feature-flags` |
| `crates/pc-cron` / `pc-heartbeat` | 复用（见上） |
| `crates/pc-app-definitions` | 复用（见上） |

## 4. 新增（multica 独有）

### Workspace & Member
- `mc-workspace`
- `mc-member`
- `mc-invitation`
- `mc-seat-capacity`
- `mc-share-link`

### Runtime & Agent
- `mc-runtime`
- `mc-runtime-profile`
- `mc-runtime-app`
- `mc-runtime-blocklist`
- `mc-runtime-host`
- `mc-runtime-liveness`
- `mc-runtime-quota`
- `mc-runtime-models`
- `mc-runtime-model-catalog`
- `mc-runtime-local-skills`
- `mc-runtime-update`
- `mc-runtime-unusable-notice`
- `mc-agent`
- `mc-agent-builder`
- `mc-agent-starters`
- `mc-agent-runtime-skills`
- `mc-task-queue`
- `mc-task-lifecycle`
- `mc-task-usage`
- `mc-task-actor`
- `mc-adapter-pi-local`（复用）
- 26 个 runtime adapter：`mc-adapter-{claude-code, codex, cursor, copilot, kimi, opencode, openclaw, hermes, pi, antigravity, codebuddy, deveco, grok, kiro-cli, qodercli, qoderclicn, qwen, qwenpaw, reasonix, traecli, dsh, omp, mcode, dim, codearts, zeroclaw}-local`（其中 `pi-local` 直接复用 pc-）

### Issue & Comment
- `mc-issue`
- `mc-issue-status`
- `mc-issue-view`
- `mc-issue-view-preference`
- `mc-issue-metadata`
- `mc-issue-property`
- `mc-issue-label`
- `mc-issue-reaction`
- `mc-comment`
- `mc-comment-thread`
- `mc-comment-reaction`
- `mc-source-context`
- `mc-property`
- `mc-label`
- `mc-reaction`

### Chat
- `mc-chat`
- `mc-chat-session`
- `mc-chat-message`
- `mc-chat-draft`

### Project
- `mc-project`
- `mc-project-resource`
- `mc-project-view`

### Inbox & Notification
- `mc-inbox`
- `mc-inbox-archive`
- `mc-inbox-preference`
- `mc-notification`

### Squad
- `mc-squad`
- `mc-squad-briefing`

### Skill
- `mc-skill`
- `mc-skill-bundle`
- `mc-skill-refresh`

### Plugin
- `mc-plugin-protocol`（复用 pc-）
- `mc-plugin-host`（复用 pc-）
- `mc-plugin-package`
- `mc-plugin-hook`
- `mc-plugin-storage`
- `mc-plugin-secret`
- `mc-plugin-ui-surface`

### Channel
- `mc-channel`
- `mc-channel-slack`
- `mc-channel-lark`
- `mc-channel-dingtalk`
- `mc-channel-wecom`
- `mc-channel-telegram`
- `mc-channel-media`

### Wakeup / Cron / Autopilot
- `mc-wakeup`
- `mc-wakeup-event`
- `mc-wakeup-receipt`
- `mc-autopilot`
- `mc-autopilot-cron`
- `mc-autopilot-webhook`
- `mc-autopilot-quota`

### VCS / GitHub / MCP
- `mc-vcs`
- `mc-github`
- `mc-mcp`

### Activity / Onboarding / Cloud / Other
- `mc-activity`
- `mc-attribution`
- `mc-admission`
- `mc-self-exec`
- `mc-onboarding`
- `mc-mika`
- `mc-cloud`
- `mc-feedback`
- `mc-contact-sales`
- `mc-instance`
- `mc-instance-telemetry`
- `mc-maintenance`
- `mc-composio`
- `mc-portability`
- `mc-quick-action`
- `mc-attachment`
- `mc-file`
- `mc-avatar`
- `mc-verification`
- `mc-reserved-slug`
- `mc-webhook`
- `mc-webhook-rate-limit`
- `mc-client-usage`
- `mc-search`
- `mc-dashboard`
- `mc-pin`
- `mc-handshake`（daemon pairing）

合计新增 ~70 个 crate。

## 5. 文件级迁移（示例）

`paperclip-rs/crates/pc-core/src/lib.rs` 的领域类型 vs `multica-rs/crates/mc-core/src/lib.rs`：

| paperclip-rs | multica-rs |
| --- | --- |
| `Company`, `CompanyMember` | `Workspace`, `WorkspaceMember` |
| `Issue` | `Issue`（字段更丰富，含 stage / parent / position / metadata / properties / reactions / source_context） |
| `Agent` | `Agent` + `AgentRuntime` + `AgentRuntimeProfile` |
| `Goal` | `Squad` |
| `Pipeline` | `Autopilot` |
| `Routine` | `Autopilot`（统一模型） |
| `RoutineVariable` | `IssueProperty` |
| `Folder` | `Project` |
| `Board` | `IssueView` |
| `Document` | `SourceContext` |
| `Approval` | `Attribution` |
| `Budget`, `Cost` | `TaskUsage` |
| `Heartbeat` | `Wakeup`（事件 + 时间） |
| `Comment` | `Comment`（含 reactions / parent / resolved / triage） |
| `Inbox` | `Inbox` + `InboxArchive` |
| `Mention` | `CommentMention` |
| `PluginManifest` | `PluginPackage` |
| `Decision` | `TaskAttribution` |
| `WorkProduct` | `TaskOutput` |
| `StatusCard` | `IssueStatusCatalog` |
| `WorkspaceBranch` | `RuntimeBranch` |
| `Environment` | `RuntimeConfig` |
| `Skill` | `Skill`（structured） |
| `Run`, `RunEvent`, `RunLog` | `Task`, `TaskMessage`, `TaskUsage` |
| `Auth` | `Auth`（同） |

## 6. Cargo workspace 编排

`Cargo.toml` workspace 成员约 120 个，分组：

```
[workspace]
members = [
    # 基础设施（按依赖顺序）
    "crates/mc-config",
    "crates/mc-errors",
    "crates/mc-telemetry",
    "crates/mc-db",
    "crates/mc-core",
    "crates/mc-storage",
    "crates/mc-secrets",
    "crates/mc-backup",
    "crates/mc-auth",
    "crates/mc-authz",
    "crates/mc-realtime",
    "crates/mc-ws",
    "crates/mc-openapi",
    "crates/mc-feature-flags",
    "crates/mc-instance",
    "crates/mc-instance-telemetry",

    # 协议层
    "crates/mc-plugin-protocol",

    # 仓储层（按依赖顺序）
    "crates/mc-repos",

    # 领域层
    "crates/mc-workspace",
    "crates/mc-member",
    "crates/mc-invitation",
    "crates/mc-seat-capacity",
    "crates/mc-share-link",
    "crates/mc-runtime",
    "crates/mc-runtime-profile",
    "crates/mc-runtime-app",
    "crates/mc-runtime-blocklist",
    "crates/mc-runtime-host",
    "crates/mc-runtime-liveness",
    "crates/mc-runtime-quota",
    "crates/mc-runtime-models",
    "crates/mc-runtime-model-catalog",
    "crates/mc-runtime-local-skills",
    "crates/mc-runtime-update",
    "crates/mc-runtime-unusable-notice",
    "crates/mc-agent",
    "crates/mc-agent-builder",
    "crates/mc-agent-starters",
    "crates/mc-agent-runtime-skills",
    "crates/mc-task-queue",
    "crates/mc-task-lifecycle",
    "crates/mc-task-usage",
    "crates/mc-task-actor",
    "crates/mc-issue",
    "crates/mc-issue-status",
    "crates/mc-issue-view",
    "crates/mc-issue-view-preference",
    "crates/mc-issue-metadata",
    "crates/mc-comment",
    "crates/mc-comment-thread",
    "crates/mc-comment-reaction",
    "crates/mc-source-context",
    "crates/mc-property",
    "crates/mc-label",
    "crates/mc-reaction",
    "crates/mc-chat",
    "crates/mc-chat-session",
    "crates/mc-chat-message",
    "crates/mc-chat-draft",
    "crates/mc-project",
    "crates/mc-project-resource",
    "crates/mc-project-view",
    "crates/mc-inbox",
    "crates/mc-inbox-archive",
    "crates/mc-inbox-preference",
    "crates/mc-notification",
    "crates/mc-squad",
    "crates/mc-squad-briefing",
    "crates/mc-skill",
    "crates/mc-skill-bundle",
    "crates/mc-skill-refresh",
    "crates/mc-wakeup",
    "crates/mc-wakeup-event",
    "crates/mc-wakeup-receipt",
    "crates/mc-autopilot",
    "crates/mc-autopilot-cron",
    "crates/mc-autopilot-webhook",
    "crates/mc-autopilot-quota",
    "crates/mc-cron",
    "crates/mc-heartbeat",
    "crates/mc-workflow",
    "crates/mc-activity",
    "crates/mc-attribution",
    "crates/mc-admission",
    "crates/mc-self-exec",
    "crates/mc-onboarding",
    "crates/mc-mika",
    "crates/mc-cloud",
    "crates/mc-feedback",
    "crates/mc-contact-sales",
    "crates/mc-maintenance",
    "crates/mc-composio",
    "crates/mc-portability",
    "crates/mc-quick-action",
    "crates/mc-attachment",
    "crates/mc-file",
    "crates/mc-avatar",
    "crates/mc-verification",
    "crates/mc-reserved-slug",
    "crates/mc-webhook",
    "crates/mc-webhook-rate-limit",
    "crates/mc-client-usage",
    "crates/mc-search",
    "crates/mc-dashboard",
    "crates/mc-pin",

    # Channel
    "crates/mc-channel",
    "crates/mc-channel-slack",
    "crates/mc-channel-lark",
    "crates/mc-channel-dingtalk",
    "crates/mc-channel-wecom",
    "crates/mc-channel-telegram",
    "crates/mc-channel-media",

    # VCS / MCP
    "crates/mc-vcs",
    "crates/mc-github",
    "crates/mc-mcp",

    # Plugin
    "crates/mc-plugin-host",
    "crates/mc-plugin-package",
    "crates/mc-plugin-hook",
    "crates/mc-plugin-storage",
    "crates/mc-plugin-secret",
    "crates/mc-plugin-ui-surface",

    # Runtime adapter
    "crates/mc-adapter-pi-local",
    "crates/mc-adapter-claude-code-local",
    "crates/mc-adapter-codex-local",
    "crates/mc-adapter-cursor-local",
    "crates/mc-adapter-cursor-cloud",
    "crates/mc-adapter-copilot-local",
    "crates/mc-adapter-kimi-local",
    "crates/mc-adapter-opencode-local",
    "crates/mc-adapter-openclaw-local",
    "crates/mc-adapter-hermes-local",
    "crates/mc-adapter-antigravity-local",
    "crates/mc-adapter-codebuddy-local",
    "crates/mc-adapter-deveco-local",
    "crates/mc-adapter-grok-local",
    "crates/mc-adapter-kiro-local",
    "crates/mc-adapter-qodercli-local",
    "crates/mc-adapter-qoderclicn-local",
    "crates/mc-adapter-qwen-local",
    "crates/mc-adapter-qwenpaw-local",
    "crates/mc-adapter-reasonix-local",
    "crates/mc-adapter-traecli-local",
    "crates/mc-adapter-dsh-local",
    "crates/mc-adapter-omp-local",
    "crates/mc-adapter-mcode-local",
    "crates/mc-adapter-dim-local",
    "crates/mc-adapter-codearts-local",
    "crates/mc-adapter-zeroclaw-local",

    # HTTP
    "crates/mc-http",

    # CLI / Server
    "apps/mc-server",
    "apps/mc-cli",

    # 工具
    "crates/mc-migrate",
    "crates/mc-typescript-gen",
    "crates/mc-app-definitions",
]
```

合计 110+ workspace 成员。