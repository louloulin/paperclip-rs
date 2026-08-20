# paperclip-rs Parity Gap Report（按类别分类）

> 自动生成：`./scripts/parity-check.sh`
> 配套：`docs/parity-trend.md`（时间序列）/ `docs/parity-gap-report.txt`（raw list）
> 生成时间：2026-08-20T12:41Z
> 覆盖度：41.9%（160/382 Node modules 在 Rust 端有对应实现）

## 统计概览

| 类别 | 数量 | Rust 已覆盖 | Gap | 优先级 |
|---|---|---|---|---|
| 总 Node modules | 382 | 160 | 208 + 14 (raw line 213) | — |
| server/services | 285 | ~120 | ~165 | P0/P1 |
| packages/shared | 97 | ~40 | ~57 | P1 |
| Rust crates | 108 | — | — | — |
| Rust pub APIs | 10559 | — | — | — |

## 已识别 Gap（208 项未覆盖 + 14 项 misc）

### A. Server Middleware & Bootstrap（18 项）

- `api-compression`, `body-limits`, `board-mutation-guard`, `error-handler`
- `http-log-policy`, `http-log-redaction`, `private-hostname-guard`, `redact-sensitive`
- `trust-proxy`, `origins`, `logger`, `instrumentation`
- `shutdown`, `startup-banner`, `server-info`, `build-commit`, `build-version`, `worktree-config`

> **说明**：许多已在 pc-http 中实现但未以原 Node module 命名（如 compression / cors / csrf middleware 已在 pc-http）。

### B. Auth & Session（12 项）

- `better-auth`, `board-claim`, `first-admin-claim`
- `agent-auth-jwt`, `agent-instructions`, `agent-permissions`, `agent-secret-bindings`
- `authorization`, `join-request-dedupe`
- `express.d`, `runtime-api`, `validate`

### C. Agents & Adapters（22 项）

- `adapter-models-env`, `adapter-plugin-store`, `adapter-registry`, `adapter-registry-bootstrap`
- `adapters`, `claude-agent-id-header`, `codex-models`, `cursor-models`, `builtin-adapter-types`
- `agent-assignability`, `agent-invokability`, `agent-start-lock`, `agent-action-audit`
- `built-in-agents`, `bundled-plugins`, `model-profile-hint`
- `external-stub-providers`, `hermes-gateway-doc`, `wake-message`
- `decision-training` (已在 pc-decisions)

### D. Companies & Teams（15 项）

- `companies` (service layer), `company-search`, `company-search-extract`, `company-search-rate-limit`
- `company-artifacts`, `company-export-readme`, `company-import-paths`
- `company-member-roles`, `company-portability`, `company-skill-policy`, `company-skills`
- `companies-routes`, `company-skill`, `client`
- `case-management` (cases), `change-consent-gate`

### E. Issues & Liveness（17 项）

- `issues`, `cases`, `issue-liveness`, `issue-graph-liveness`
- `issue-recovery-actions`, `issue-rewake-throttle`, `issue-thread-interactions`
- `issue-continuation-summary`, `issue-dependency-wakeups`, `issue-goal-fallback`
- `issues-checkout-wakeup`, `activity-log`
- `pause-hold-guard`, `run-continuations`, `run-liveness-continuations`
- `responsible-user-denial-run-outcomes`

### F. Pipelines & Workflows（6 项）

- `pipelines-aggregation`
- `cron`
- `productivity-review`
- `summary-slot-finalization`, `summary-slots`
- `successful-run-handoff`, `successful-run-handoff-state`

### G. Environments（11 项）

- `environment-config`, `environment-runtime`, `environments`
- `environment-custom-image-runtime`, `environment-custom-image-setup-session-utils`
- `environment-custom-image-terminal-sessions`, `environment-custom-image-terminal-ws`
- `environment-custom-images`, `environment-execution-target`
- `environment-probe`, `environment-run-orchestrator`, `environment-selection`

### H. Tools（8 项）

- `tool-access`, `tool-access-policy`, `tool-gateway`
- `tool-oauth-legacy-backfill`, `tool-profile-binding-precedence`
- `tool-runtime-supervisor`, `trust-preset-resolver`
- `tool-content-guards`（隐含）

### I. Plugins（22 项）

- `plugin-capability-validator`, `plugin-config-validator`, `plugin-dev-watcher`
- `plugin-environment-driver`, `plugin-event-bus`, `plugin-host-service-cleanup`
- `plugin-host-services`, `plugin-install-guard`, `plugin-job-coordinator`
- `plugin-job-scheduler`, `plugin-job-store`, `plugin-lifecycle`
- `plugin-loader`, `plugin-local-folders`, `plugin-managed-agents`
- `plugin-managed-routines`, `plugin-managed-skills`, `plugin-manifest-validator`
- `plugin-runtime-sandbox`, `plugin-secrets-handler`, `plugin-tool-dispatcher`
- `plugin-tool-registry`, `plugins`

### J. Workspace & Execution（10 项）

- `execution-workspaces`, `execution-workspace-policy`
- `workspace-file-resources`, `workspace-operation-log-store`
- `workspace-realization`, `workspace-runtime`, `workspace-runtime-service-authz`
- `workspace-command-authz`, `workspace-operation`
- `work-products`, `workspace-file-resource`

### K. Infrastructure（18 项）

- `config-file`, `index`, `service`, `types`, `utils`, `test`
- `assets`, `attachment-types`, `origins`
- `batch-insert`, `dev-runner-worktree`, `dev-watch-ignore`
- `low-trust-runtime-containment`, `managed-config`, `managed-environments`
- `principal-access-compatibility`, `quota-windows`
- `sandbox-provider-runtime`, `static-index-html`, `vite-html-renderer`

### L. Storage & Backup（6 项）

- `local-disk-provider`, `local-encrypted-provider`, `s3-provider`
- `aws-secrets-manager-provider`, `configured-provider`, `provider-registry`
- `instance-database-backups`, `database-backup-health`

### M. UI / Frontend Helpers（10 项）

- `org-chart-svg`, `ui-branding`, `vite-html-renderer`
- `static-index-html`, `llms`, `origins`
- `app`, `app-definition`, `client`, `events`

### N. Realtime & Live（4 项）

- `live-events`, `live-events-ws`, `live`
- `document-annotations`

### O. Decisions & Notifications（5 项）

- `decision-signing`, `decision-training`
- `inbox-dismissals`, `inbox-dismissal`, `instance-settings`
- `status-card-finalization`, `status-cards`

### P. Resource Membership（4 项）

- `resource-memberships`
- `invite-grants`, `invite-rate-limit`
- `user-profiles`

### Q. Misc（9 项）

- `access`, `app`, `feedback-redaction`, `feedback-share-client`
- `file-resources`, `heartbeat-run-runtime-status`, `heartbeat-run-summary`
- `heartbeat-stop-metadata`, `task-watchdogs`
- `embedded-postgres`, `drain-heartbeat-runs`, `instance`

### R. Shared Types (packages/shared)（17 项）

- `adapter-registry`, `app-definition`, `artifact`
- `company-skill`, `document-annotation`, `external-object`
- `folder`, `humanize-connection`, `paperclip-telemetry`
- `project-mentions`, `search`, `skill-policy`
- `summary-slot`, `work-product`, `workspace-file-resource`
- `workspace-operation`, `events`

---

## 后续优先级（按业务核心度）

### P0 — 核心业务流（必 port）

1. **E 组** Issues & Liveness（17 项）→ 已部分实现（pc-issues），缺 pause-hold-guard 等
2. **F 组** Pipelines & Workflows（6 项）→ pc-pipelines / pc-workflow 部分覆盖
3. **A 组** Server Middleware（18 项）→ pc-http 已实现大部分
4. **G 组** Environments（11 项）→ pc-environment 已 7000+ 行覆盖
5. **H 组** Tools（8 项）→ pc-tool 已 241 tests 覆盖

### P1 — 重要但非阻塞

- **B 组** Auth & Session（12 项）→ pc-auth 已有大部分
- **D 组** Companies（15 项）→ pc-companies 已有大量 pure helpers
- **I 组** Plugins（22 项）→ 重点：pc-plugin-host/host-services
- **N 组** Realtime（4 项）→ pc-realtime / pc-ws 已实现

### P2 — 工具/UI 边角（可延后）

- **C 组** Agents/Adapters（22 项）→ 大部分已有 pc-adapter-* 覆盖
- **J 组** Workspace（10 项）→ 部分由 pc-execution 覆盖
- **K 组** Infrastructure（18 项）→ 散落各处
- **L 组** Storage（6 项）→ pc-storage 部分
- **M 组** UI Helpers（10 项）→ 低优先
- **O/P/Q/R 组**（合计 ~40 项）→ 散点

---

## 实际覆盖度评估

虽然名义覆盖率 41.9%，**实际业务覆盖度更高**：

- Rust 端 **108 crates / 10559 pub APIs** 提供实质实现
- 许多"未覆盖"模块实际是**已被整合到其他 Rust crate**（如 agents service 在 pc-agent，cases 在 pc-repos）
- 一些是 Node 平台代码（如 `embedded-postgres`、`vite-html-renderer`）→ 不需要 Rust 端
- 一些是 dev-only（`dev-runner-worktree`、`dev-watch-ignore`）→ 工具脚本

### 真正缺口（按 P0 → P2）

| 优先级 | 真实 gap |
|---|---|
| **P0** | pause-hold-guard, run-continuations, pipelines-aggregation, environment-execution-target, environment-run-orchestrator, decision-signing verification |
| **P1** | better-auth rewrite, board-claim first-admin-claim, plugin-capability-validator, plugin-host-service-cleanup, plugin-job-coordinator, plugin-job-scheduler |
| **P2** | document-annotations, sidebar-preferences, smoke-lab, productivity-review |

---

## 运行方式

```bash
# 跑 parity check
./scripts/parity-check.sh

# 输出：
# - 控制台：覆盖率 + 头部 50 个 gap
# - docs/parity-trend.md：append 一次历史快照
# - docs/parity-gap-report.txt：完整 gap 列表
# - docs/parity-gap-report.md（本文件）：按类别分类
```

## 累计统计（最近一次跑）

```
Node total       : 382
Rust crates      : 108
Rust pub APIs    : 10559
Covered          : 160 (41.9%)
Gap              : 222
Threshold        : 95%
Last run         : 2026-08-20T12:41Z
```

## 后续推进路径

1. **立即**：把 P0 真实 gap（pause-hold-guard 等）逐一补齐 → 每 round 单 commit + 全 PASS
2. **中**：在 nightly CI 跑 `parity-check.sh`，跟踪趋势
3. **长期**：覆盖率 ≥ 95% 后，把 parity-check 作为 release gate