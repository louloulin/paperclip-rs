# paperclip → paperclip-rs 模块对照表

> 每一行 = 当前 `paperclip/` 仓库的一个模块或文件，对应 `paperclip-rs/` 工作区中的目标 crate 或任务条目。
> 用于实施时按表逐项落实。

---

## A. server 路由（56 个 → pc-http::routes）

| 当前 TS 模块 | 当前 LOC 级别 | Rust 任务 |
|---|---|---|
| `server/src/routes/access.ts` | route | `pc-http` 3.5.6 `routes::access` |
| `server/src/routes/activity.ts` | route | 3.5.3 `routes::activity` |
| `server/src/routes/adapters.ts` | route | 3.7.1 `routes::adapters` |
| `server/src/routes/agents.ts` | route | 3.2.2 `routes::agents` |
| `server/src/routes/approvals.ts` | route | 3.2.6 `routes::approvals` |
| `server/src/routes/assets.ts` | route | 3.7.4 `routes::assets` |
| `server/src/routes/attention.ts` | route | 3.5.4 `routes::attention` |
| `server/src/routes/auth.ts` | route | 3.5.5 `routes::auth` |
| `server/src/routes/authz.ts` | route | 3.5.5 `routes::authz`（授权检查端点） |
| `server/src/routes/board-chat.ts` | route | 3.3.3 / 3.4.1 `routes::board_chat` |
| `server/src/routes/built-in-agents.ts` | route | 3.7.2 `routes::built_in_agents` |
| `server/src/routes/cases.ts` | route | 3.2.5 `routes::cases` |
| `server/src/routes/companies.ts` | route | 3.2.1 `routes::companies` |
| `server/src/routes/company-import-paths.ts` | route | 3.2.1a `routes::company_import_paths` |
| `server/src/routes/company-skill-policy.ts` | route | 3.3.5 `routes::company_skill_policy` |
| `server/src/routes/company-skills.ts` | route | 3.3.5 `routes::company_skills` |
| `server/src/routes/costs.ts` | route | 3.5.3 `routes::costs` |
| `server/src/routes/dashboard.ts` | route | 3.5.3 `routes::dashboard` |
| `server/src/routes/decision-training.ts` | route | 3.7.5 `routes::decision_training` |
| `server/src/routes/decisions.ts` | route | 3.2.7 `routes::decisions` |
| `server/src/routes/environment-selection.ts` | route | 3.7.7 `routes::environment_selection` |
| `server/src/routes/environments.ts` | route | 3.3.10 `routes::environments` |
| `server/src/routes/execution-workspaces.ts` | route | 3.3.1 `routes::execution_workspaces` |
| `server/src/routes/file-resources.ts` | route | 3.3.4 `routes::file_resources` |
| `server/src/routes/folders.ts` | route | 3.7.8 `routes::folders` |
| `server/src/routes/goals.ts` | route | 3.3.2 `routes::goals` |
| `server/src/routes/health.ts` | route | 1.4.2 / 3.6.3 `pc-server::health` |
| `server/src/routes/inbox-agent-policy.ts` | route | 3.7.9 `routes::inbox_agent_policy` |
| `server/src/routes/inbox-dismissals.ts` | route | 3.4.5 `routes::inbox_dismissals` |
| `server/src/routes/instance-database-backups.ts` | route | 3.6.2 `routes::instance_database_backups` |
| `server/src/routes/instance-settings.ts` | route | 3.6.1 `routes::instance_settings` |
| `server/src/routes/issue-tree-control.ts` | route | 3.7.10 `routes::issue_tree_control` |
| `server/src/routes/issues.ts` | route | 3.2.3 `routes::issues` |
| `server/src/routes/issues-checkout-wakeup.ts` | route | 3.7.11 `routes::issues_checkout_wakeup` |
| `server/src/routes/llms.ts` | route | 3.6.5 `routes::llms` |
| `server/src/routes/openapi.ts` | route | 3.8 `pc-openapi` |
| `server/src/routes/org-chart-svg.ts` | route | 3.6.6 `routes::org_chart_svg` |
| `server/src/routes/pipelines.ts` | route | 3.7.13 `routes::pipelines` |
| `server/src/routes/plugin-ui-static.ts` | route | 3.6.7 `routes::plugin_ui_static` |
| `server/src/routes/plugins.ts` | route | 3.7.3 `routes::plugins` |
| `server/src/routes/projects.ts` | route | 3.7.15 / 3.2.4 `routes::projects` |
| `server/src/routes/resource-memberships.ts` | route | 3.7.16 / 3.4.3 `routes::resource_memberships` |
| `server/src/routes/routines.ts` | route | 3.7.17 / 4.4.1 `routes::routines` |
| `server/src/routes/secrets.ts` | route | 3.7.18 / 3.5.1 `routes::secrets` |
| `server/src/routes/sidebar-badges.ts` | route | 3.7.19 / 3.4.4 `routes::sidebar_badges` |
| `server/src/routes/sidebar-preferences.ts` | route | 3.7.19 / 3.4.4 `routes::sidebar_preferences` |
| `server/src/routes/smoke-lab.ts` | route | 3.7.20 `routes::smoke_lab` |
| `server/src/routes/status-cards.ts` | route | 3.7.21 `routes::status_cards` |
| `server/src/routes/summary-slots.ts` | route | 3.7.22 `routes::summary_slots` |
| `server/src/routes/teams-catalog.ts` | route | 3.7.23 `routes::teams_catalog` |
| `server/src/routes/tool-access.ts` | route | 3.7.24 / 3.5.2 `routes::tool_access` |
| `server/src/routes/tool-gateway.ts` | route | 3.7.25 / 3.5.2 `routes::tool_gateway` |
| `server/src/routes/user-profiles.ts` | route | 3.7.26 / 3.4.2 `routes::user_profiles` |
| `server/src/routes/workspace-command-authz.ts` | route | 3.7.27 `routes::workspace_command_authz` |
| `server/src/routes/workspace-runtime-service-authz.ts` | route | 3.7.28 `routes::workspace_runtime_service_authz` |
| `server/src/routes/index.ts` | re-exports | 3.1.5 移植路由注册表 |

---

## B. server 中间件与基础（13 个 → pc-http::middleware）

| 当前 TS | Rust 任务 |
|---|---|
| `middleware/api-compression.ts` | 3.1.2 compression middleware |
| `middleware/auth.ts` | 3.1.2 actor middleware |
| `middleware/board-mutation-guard.ts` | 2.3.3 board-mutation guard |
| `middleware/error-handler.ts` | 3.1.3 错误映射 |
| `middleware/http-log-policy.ts` | 3.1.2c `pc-http::middleware::http_log_policy` |
| `middleware/http-log-redaction.ts` | 6.3.4 log 重写 |
| `middleware/index.ts` | 3.1.1 middleware barrel |
| `middleware/logger.ts` | 1.2.2 `pc-telemetry` |
| `middleware/private-hostname-guard.ts` | 3.1.2 private-hostname guard |
| `middleware/redact-sensitive.ts` | 3.1.2d `pc-http::middleware::redact_sensitive` |
| `middleware/trust-proxy.ts` | 3.1.2b `pc-http::middleware::trust_proxy` |
| `middleware/validate.ts` | 3.1.4 请求体验证 |
| `middleware/cloud-tenant-actor.test.ts` | 测试随 actor middleware |

---

## C. server 认证/实时/HTTP/lib（5 模块）

| 当前 TS | Rust 任务 |
|---|---|
| `auth/better-auth.ts` | 2.2 `pc-auth`（行为复刻） |
| `realtime/live-events-ws.ts` | 4.2 `pc-ws` live-events |
| `realtime/environment-custom-image-terminal-ws.ts` | 4.2.7 自定义镜像终端 WS |
| `http/body-limits.ts` | 3.1.2a `pc-http::middleware::body_limits` |
| `lib/join-request-dedupe.ts` | 3.1.2e `pc-core::lib::join_request_dedupe` |
| `lib/objects.ts` | 3.1.2f `pc-core::lib::objects` |

---

## D. server 存储（6 个 → pc-storage）

| 当前 TS | Rust 任务 |
|---|---|
| `storage/index.ts` | 2.4.3a `StorageService` 聚合 |
| `storage/local-disk-provider.ts` | 2.4.2 `local_disk` provider |
| `storage/s3-provider.ts` | 2.4.3 `s3` provider |
| `storage/service.ts` | 2.4.3a `StorageService` 聚合 |
| `storage/provider-registry.ts` | 2.4.3a 注册表 |
| `storage/types.ts` | 2.4.1 `StorageProvider` trait |

---

## E. server 密钥（6 个 → pc-secrets）

| 当前 TS | Rust 任务 |
|---|---|
| `secrets/types.ts` | 2.4.4 `SecretsProvider` trait |
| `secrets/provider-registry.ts` | 2.4.6a 注册表 |
| `secrets/local-encrypted-provider.ts` | 2.4.5 `local_encrypted` |
| `secrets/aws-secrets-manager-provider.ts` | 2.4.6 `aws_sm` |
| `secrets/configured-provider.ts` | 2.4.6a `configured_provider` |
| `secrets/external-stub-providers.ts` | 2.4.6a stub providers |

---

## F. server 适配器（11 个文件 → pc-adapter-api + pc-adapter-*）

| 当前 TS | Rust 任务 |
|---|---|
| `server/src/adapters/index.ts` | 5.1 `pc-adapter-api` |
| `server/src/adapters/types.ts` | 5.1 trait |
| `server/src/adapters/registry.ts` | 5.1 registry |
| `server/src/adapters/utils.ts` | 5.1 utils |
| `server/src/adapters/builtin-adapter-types.ts` | 5.1 types |
| `server/src/adapters/http/*` | 5.1 HTTP 调用 helper |
| `server/src/adapters/process/*` | 5.1 子进程 helper |
| `server/src/adapters/claude-agent-id-header.ts` | 5.1 header helper |
| `server/src/adapters/codex-models.ts` | 5.1 model list |
| `server/src/adapters/cursor-models.ts` | 5.1 model list |
| `server/src/adapters/hermes-gateway-doc.ts` | 5.1 doc helper |

---

## G. packages/adapters（11 个内置适配器 → 11 个 crate）

| 当前包 | Rust crate |
|---|---|
| `@paperclipai/adapter-claude-local` | `pc-adapter-claude-local` (5.2.1 — `skills.rs` R391 done: list_claude_skills / sync_claude_skills / resolve_claude_skills_home / build_claude_skill_snapshot / resolve_claude_desired_skill_names) |
| `@paperclipai/adapter-codex-local` | `pc-adapter-codex-local` (5.2.2 — `skills.rs` R392 done: list_codex_skills / sync_codex_skills / build_codex_skill_snapshot / resolve_codex_desired_skill_names — **simplified variant, no skillsHome**) |
| `@paperclipai/adapter-cursor-cloud` | `pc-adapter-cursor-cloud` (5.2.3) |
| `@paperclipai/adapter-cursor-local` | `pc-adapter-cursor-local` (5.2.4) |
| `@paperclipai/adapter-gemini-local` | `pc-adapter-gemini-local` (5.2.5 — `skills.rs` R393 done: list_gemini_skills / sync_gemini_skills / build_gemini_skill_snapshot / resolve_gemini_skills_home / resolve_gemini_desired_skill_names — **first side-effecting sync: creates + repairs + removes symlinks**) |
| `@paperclipai/adapter-grok-local` | `pc-adapter-grok-local` (5.2.6) |
| `@paperclipai/adapter-hermes-gateway` | `pc-adapter-hermes-gateway` (5.2.7) |
| `@paperclipai/adapter-openclaw-gateway` | `pc-adapter-openclaw-gateway` (5.2.8) |
| `@paperclipai/adapter-opencode-local` | `pc-adapter-opencode-local` (5.2.9 — `skills.rs` R394 done: list_opencode_skills / sync_opencode_skills / build_opencode_skill_snapshot / resolve_opencode_skills_home / resolve_opencode_desired_skill_names — **shares Claude skills home, persistent + side-effecting sync**) |
| `@paperclipai/adapter-pi-local` | `pc-adapter-pi-local` (5.2.10) |
| `@paperclipai/adapter-hermes` | 同 `pc-adapter-hermes-gateway`（合并） |

---

## H. packages/db（142 文件 → pc-db）

| 当前 | Rust 任务 |
|---|---|
| `client.ts` | 1.3.1 `pc-db::Db` |
| `migrate.ts` | 1.3.3 `pc-db::migrate` |
| `migration-runtime.ts` | 1.3.3 runtime |
| `migration-safety-baseline.ts` | 1.3.5 safety |
| `migration-status.ts` | 6.2.1 `pc-migrate status` |
| `runtime-config.ts` | 1.3.4 嵌入式 PG 启动 |
| `embedded-postgres-error.ts` | 1.3.4 错误处理 |
| `embedded-postgres-native.ts` | 1.3.4 native 二进制 |
| `seed.ts` | 1.3.6 seed |
| `schema/*.ts`（109 文件） | 1.3.2 SQL DDL 迁移 |
| `migrations/*`（运行时迁移） | 1.3.2 迁移文件 |
| `backup.ts` / `backup-lib.ts` | 6.4 `pc-backup` |

---

## I. packages/shared（189 文件 → pc-core + 跨 crate 类型）

| 当前 | Rust 任务 |
|---|---|
| `adapter-agnostic-keys.ts` | 5.1 adapter keys |
| `adapter-type.ts` | 5.1 adapter types |
| `agent-eligibility.ts` | 2.1.2 `pc-repos::agent` |
| `agent-url-key.ts` | 5.1 URL keys |
| `api.ts`（API path 常量） | 3.1.5 路由注册表 |
| `app-definitions/*` | 5.1 app catalog |
| `config-schema.ts` | 5.1 config schema |
| `constants.ts` | `pc-core::constants` |
| `decision.ts` | 2.1.7 `pc-repos::decision` |
| `document-anchors.ts` | 6.6 `pc-doc-anchors` |
| `environment-custom-images.ts` | 2.1.10 `pc-repos::environment` |
| `environment-support.ts` | 2.1.10 同上 |
| `execution-workspace-guards.ts` | 2.1.11 `pc-repos::execution` |
| `external-objects.ts` / `external-objects-server.ts` | 2.1.14 `pc-repos::activity`（关联） |
| `feature-catalog.ts` | 6.5 `pc-feature-flags` |
| `frontmatter.ts` | `pc-core::frontmatter` |
| 其余 165 文件 | 各 repo 模块 + 适配器配置 |

---

## J. packages/adapter-utils（51 文件 → pc-adapter-api 共享）

| 当前 | Rust 任务 |
|---|---|
| `acpx-engine/*` | 5.1 acpx engine helper |
| `billing.ts` | 5.1 billing |
| `command-managed-runtime.ts` | 5.1 runtime |
| `command-redaction.ts` | 5.1 redaction |
| `exclude-patterns.ts` | 5.1 file excludes |
| `execution-target.ts` | 5.1 target |
| `git-workspace-sync.ts` | 5.1 git sync |
| `index.ts` | 5.1 公共导出 |
| `local-process-sandbox.ts` | 5.1 sandbox |
| `server-utils.ts` | `pc-acpx::prompt_compose` (R380+R381+R382+R383+R384+R385+R386+R387+R388+R389+R390 done) + `pc-acpx::build_prompt` (R381 done) + `pc-acpx::instance_root` (R386 done) + `pc-acpx::skill_sync_preference` (R387 done) + `pc-acpx::skill_snapshot` (R388 done) + `pc-acpx::skill_materialize` (R389 done) + `pc-acpx::skill_io` (R390 done — 7 skill I/O 函数 + 2 常量:is_maintainer_only_skill_target / resolve_paperclip_skills_dir / list_paperclip_skill_entries / read_installed_skill_targets / normalize_configured_paperclip_runtime_skills / read_paperclip_runtime_skill_entries / read_paperclip_skill_markdown / ensure_paperclip_skill_symlink / remove_maintainer_only_skill_symlinks + PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES / PAPERCLIP_SKILL_KEY_PREFIX) |
| `types.ts` | `pc-acpx::adapter_skills` (R391 done — `AdapterSkillContext` + `lookup_path` + `env_object`) |
| `log-redaction.ts` | 5.1 log redact |
| `mcp-isolation.integration.test.ts` | 5.1 测试 |
| `remote-execution-env.ts` | 5.1 remote env |
| `remote-managed-runtime.ts` | 5.1 remote runtime |
| `runtime-progress.ts` | 5.1 progress |
| `sandbox-*`（多个） | 5.1 sandbox helpers |

---

## K. packages/skills-catalog → pc-repos::skill + pc-feature-flags

| 当前 | Rust 任务 |
|---|---|
| `catalog-builder.ts` | 2.1.25 `pc-repos::skill` |
| `frontmatter.ts` | 6.5 frontmatter |
| `packaged-artifacts.ts` | 6.5 packaging |
| `shipped-catalog.ts` | 6.5 shipped |
| `types.ts` | 6.5 types |

---

## L. packages/plugins/sdk（10 模块 → pc-plugin-host + pc-plugin-protocol）

| 当前 | Rust 任务 |
|---|---|
| `define-plugin.ts` | 5.4 `pc-plugin-host` |
| `runWorker` / `worker-rpc-host.ts` | 5.4.1 Worker 池 |
| `protocol.ts` | 5.3 `pc-plugin-protocol` |
| `types.ts` | 5.3 types |
| `host-client-factory.ts` | 5.4 host client |
| `bundlers.ts` | 5.4 bundler presets |
| `dev-server.ts` / `dev-cli.ts` | 5.4 dev 工具 |
| `testing.ts` | 5.4 测试 harness |
| `ui/*` | 5.4 UI 子模块 |

---

## M. cli 命令（19 个 → pc-cli）

| 当前 | Rust 任务 |
|---|---|
| `allowed-hostname.ts` | 6.1.15 |
| `auth-bootstrap-ceo.ts` | 6.1.14 |
| `configure.ts` | 6.1.12 |
| `db-backup.ts` | 6.1.13 |
| `doctor.ts` | 6.1.5 |
| `env.ts` | 6.1.16 |
| `env-lab.ts` | 6.1.16 |
| `heartbeat-run.ts` | 6.1.7 |
| `install.ts` | 6.1.3 |
| `onboard.ts` | 6.1.4 |
| `pipelines.ts` | 6.1.8 |
| `routines.ts` | 6.1.9 |
| `run.ts` | 6.1.2 |
| `service.ts` | 6.1.10 |
| `uninstall.ts` | 6.1.17 |
| `update.ts` | 6.1.11 |
| `worktree.ts` | 6.1.6 |
| `worktree-lib.ts` | 6.1.6 |
| `worktree-merge-history-lib.ts` | 6.1.6 |

---

## N. ui（1168 文件 → **完全复用**）

| 维度 | 处理 |
|---|---|
| `ui/src/api/*`（60 客户端） | 通过 HTTP/WS 契约冻结，无改动 |
| `ui/src/components/*` | 无改动 |
| `ui/src/pages/*` | 无改动 |
| `ui/src/hooks/*` | 无改动 |
| `ui/src/context/*` | 无改动 |
| `ui/src/lib/*` | 无改动 |
| `ui/src/adapters/*` | 无改动 |
| `ui/src/plugins/*` | 无改动 |
| `ui/src/App.tsx` | 无改动 |
| `ui/src/index.css` | 无改动 |
| `ui/src/main.tsx` | 无改动 |
| `ui/vite.config.ts` | 6.7.2 通过 `VITE_API_BASE` 切换 base URL |
| `ui/vite.qa.config.mjs` | 6.7.2 同上 |
| `ui/vitest.config.ts` | 无改动 |
| `ui/storybook/*` | 无改动 |

UI 仅在切换部署时通过环境变量切到 Rust server；任何时候保留回滚到 Node server 的能力。

---

## O. 其他 packages

| 当前 | Rust 任务 |
|---|---|
| `packages/google-sheets-mcp-server` | 5.4 插件能力扩展（保留 npm） |
| `packages/kv-demo-mcp-server` | 5.4 插件示例（保留 npm） |
| `packages/mcp-server` | 5.4 MCP 工具分发 |
| `packages/teams-catalog` | 6.5 团队目录特性 |
| `packages/plugins/examples/*` | 5.4 插件示例 |
| `packages/plugins/sandbox-providers/*` | 5.4 沙箱 provider 插件 |

---

## 总结

| 类别 | 当前数量 | Rust 映射 |
|---|---|---|
| server 路由 | 56 | 56 任务条目 |
| server 服务 | 211 | 25 repo 子模块 |
| server middleware | 13 | pc-http middleware stack |
| server 存储/密钥 | 12 | pc-storage / pc-secrets |
| server 认证/实时/lib | 6 | pc-auth / pc-ws / pc-core::lib |
| server 适配器 host | 11 | pc-adapter-api |
| 内置适配器 | 11 | 11 crate |
| db schema | 109 | 25 repo 子模块 + 1 SQL 迁移 |
| db 客户端 | ~12 | pc-db |
| shared | 189 | pc-core + 跨 crate |
| adapter-utils | 51 | pc-adapter-api |
| skills-catalog | ~8 | pc-repos::skill + pc-feature-flags |
| 插件 SDK | 10 | pc-plugin-host + pc-plugin-protocol |
| CLI 命令 | 19 | pc-cli |
| UI 源文件 | 1168 | 完全复用（零改动） |
| **总计 Rust crate** | | **~30 crates + 2 binaries** |
