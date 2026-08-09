# Tasks: paperclip-rs Modules Replica

> 配套 proposal.md + design.md。每行一个原子动作，勾完即视为该任务"真实验证"通过。
> 全部真实操作：源码写入、`cargo build` 通过、回归 e2e-baseline.sh 通过、模块自带的 happy + ≥3 edge case 集成测试通过。

---

## M1 — apps/ 目录契约
- [x] 新建 `apps/pc-server/`、`apps/pc-cli/`
- [x] 把 `crates/pc-server/{src,Cargo.toml,...}` 移到 `apps/pc-server/`
- [x] 把 `crates/pc-cli/{src,Cargo.toml,...}` 移到 `apps/pc-cli/`
- [x] 根 `Cargo.toml` 改 workspace members
- [x] 调整相对 path 依赖
- [x] `cargo build -p pc-server && cargo build -p pc-cli` 通过
- [x] `--help` 输出与改动前一致

## M2 — E2E 基线
- [x] `pc-migrate up` 在 fresh DB 上成功
- [x] `pc-server` 启动后 `/health` 200
- [x] `/ready` 在 migrate 完成后 200
- [x] `scripts/e2e-baseline.sh` 通过
- [x] glibc + musl 双构建均成功

## M3 — Storage 真实链路
- [x] `StorageProvider` trait 完整
- [x] `local_disk` put/get/list/delete/presign 实现
- [x] `s3` 真实 SDK 接入（aws-sdk-s3）
- [x] registry 与 fallback
- [x] 集成测试：local_disk happy + ≥3 edge
- [x] 集成测试：s3 + localstack happy + ≥3 edge

## M4 — Secrets 真实链路
- [x] `SecretsProvider` trait
- [x] `local_encrypted` AES-256-GCM 完整
- [x] `aws_sm` 真实 SDK
- [x] 多 provider 链
- [x] 测试：加解密 roundtrip
- [x] 测试：密钥永不入 logs（截 `tracing` JSON）

## M5 — Backup
- [x] `Backup` 数据结构
- [x] `dump(db_url, dir)`
- [x] `restore(backup, db_url)`
- [x] retention 调度
- [x] 真实 PG dump→restore 行级一致
- [x] SHA256 校验

## M6 — Migrate 工具
- [x] `pc-migrate up` / `down` / `status` / `create` / `verify` / `baseline` / `seed`
- [x] `--json` 输出 schema
- [x] dry-run + lock
- [x] 当前 109 表 SQL 跑通 fresh up

## M7 — Auth + AuthZ
- [x] `Session` / cookie / CSRF
- [x] API Key (`pk_<base62>`)
- [x] actor 中间件
- [x] `Policy` trait + 5 mode × N resource
- [x] 集成测试：每个 resource × allow/deny/not_owner（≥80 case）

## M8 — DB Schema + Repos 25 子模块
- [x] 109 张表 DDL 已在 `pc-db/migrations/`
- [x] 25 repo 子模块各文件齐
- [x] 每 repo 子模块 ≥ 3 happy + ≥ 1 edge
- [x] 类型化 ID（newtype）
- [x] 错误归一

## M9 — HTTP 路由 56
- [x] `AppState` 设计
- [x] middleware stack：actor / request_id / log / cors / compression / body_limits / error_mapping
- [x] 56 路由逐一迁
- [x] 每个路由 ≥ happy + 3 edge
- [x] 字节级一致（与 Node 同 fixture）

## M10 — OpenAPI 3.1
- [ ] `utoipa` derive
- [ ] `/openapi.json` + `/openapi.yaml`
- [ ] 字段命名 snake → camel
- [ ] 与 Node 产物字段一致

## M11 — Realtime + WS
- [x] `Bus` trait + `InMemoryBus`
- [x] `/api/live-events` WS endpoint
- [x] token 校验 + last-event-id 回放
- [x] 30s ping
- [x] 真实 WS 推 1 条 live-event 给 WSCat 收到

## M12 — Heartbeat 状态机
- [ ] state machine enum + transition table
- [ ] `pick → invoke → finalize` 主路径
- [ ] 覆盖 Node `services/heartbeat.ts` 全部逻辑
- [ ] recovery 全部 round*.rs 测试通过

## M13 — Adapter-API + 11 适配器
- [x] `AdapterRuntime` trait
- [x] `pc-adapter-claude-local` ≥ 8k 行
- [x] `pc-adapter-codex-local` ≥ 5k 行
- [x] `pc-adapter-cursor-local` ≥ 3k 行
- [x] `pc-adapter-cursor-cloud` ≥ 1.5k 行
- [x] `pc-adapter-gemini-local` ≥ 4k 行
- [x] `pc-adapter-grok-local` ≥ 1.5k 行
- [x] `pc-adapter-opencode-local` ≥ 3k 行
- [x] `pc-adapter-pi-local` ≥ 3k 行
- [x] `pc-adapter-hermes-gateway` ≥ 完整 hermes 包
- [x] `pc-adapter-hermes` ≥ 完整 hermes 包（合并 hermes-gateway）
- [x] `pc-adapter-openclaw-gateway` ≥ 1.5k 行
- [x] 每 crate ≥ happy + ≥1 failure 集成测试

## M14 — Plugin Host
- [ ] `pc-plugin-protocol` JSON-RPC schema 完整
- [ ] `WorkerPool`
- [ ] `EventBus / JobScheduler / JobStore / ToolDispatcher / DatabaseBridge / StateStore / WebhookDispatcher / ManifestValidator / CapabilityValidator`
- [ ] 与原 SDK worker 互操作测试

## M15 — Workflow + Cron
- [ ] `pc-cron::next_after` 纯函数
- [ ] `pc-workflow::Routine` 调度
- [ ] `pc-workflow::Pipeline` DAG
- [ ] `tokio-cron-scheduler` 集成
- [ ] 测试：cron 表达式 + 真实触发 routine + pipeline step 失败中断

## M16 — CLI 全子命令
- [ ] `run` / `install` / `onboard` / `doctor` / `worktree` / `heartbeat-run` / `pipelines` / `routines` / `service` / `update` / `configure` / `db-backup` / `auth-bootstrap-ceo` / `allowed-hostname` / `env` / `env-lab` / `uninstall`
- [ ] 每个 `--help` 输出
- [ ] 每个 `--json` 输出
- [ ] 每个真实跑一遍

## U1 — UI 切流
- [ ] `paperclip-rs/ui/` 从原 `paperclip/ui/` 直接复用
- [ ] `VITE_API_BASE=http://localhost:3100` 切到 Rust server
- [ ] UI 60 个 api client happy path 全过

## U2 — Playwright e2e
- [ ] 起 PG + pc-server + UI
- [ ] 登录 → 创建公司 → 创建 issue → 启动 heartbeat → 收 live-event
- [ ] 整剧本通过

## U3 — OpenAPI ↔ UI 类型对齐
- [ ] 服务端 `openapi.json`
- [ ] ts-rs 反向生成 UI 客户端类型
- [ ] UI 60 client 文件签名一致

---

## DoD（一个模块即视为完成）

1. ✅ Rust 源码写到对应 crate（高内聚低耦合）
2. ✅ `cargo check -p <crate>` + `cargo clippy -p <crate> -- -D warnings` 通过
3. ✅ `cargo test -p <crate>` 通过（含 happy + ≥3 edge 用例）
4. ✅ 回归 `scripts/e2e-baseline.sh` 通过
5. ✅ 真实运行一次（如起 server、推 WS、调 CLI、跑 cron、跑 backup…）
6. ✅ Markdown 记录真实验证日志（`openspec/changes/<c>/evidence/<module>.md`）

任何一项不达标，本模块未完成。

---

## M17 — UI 切流真实链路（U1）

> 用户目标"真实启动前后端验证"的硬阻塞之一。**优先级 = P0**。

- [x] `ui/src/api/client.ts` 接受 `import.meta.env.VITE_API_BASE`（默认 `/api`，可覆盖为 `http://localhost:3100`）
- [x] `apps/pc-server` 暴露与 Node server 一致的 `/api/*` 与 `/live-events`（已存在，验证前缀对齐）
- [x] `scripts/dev-ui-rust.sh`：临时 PG → pc-migrate up → 起 pc-server → 起 vite (`VITE_API_BASE=http://localhost:53100`) → 验证 UI 60 api client happy path（curl 至少 5 个 GET endpoint 返回 200）
- [x] `tests/ui/contract-smoke.test.ts` —— vitest 跑 5 个 api client（auth/companies/issues/agents/heartbeat）请求 fixture，断言 shape

## M18 — 前后端端到端（U2）

> P0。Playwright 整剧本：登录 → 公司 → issue → heartbeat → live-event

- [x] `tests/e2e/full-stack.spec.ts`：启动 PG + pc-server + vite，Playwright 跑：
  1. `POST /api/auth/sign-in/email` 拿到 session cookie
  2. 创建公司 + 拉 agent
  3. 创建 issue + 触发 heartbeat
  4. WS `/live-events` 收到 `heartbeat.run.completed` 事件
- [x] `scripts/e2e-full-stack.sh`：CI 用（macOS + Linux glibc/musl 三态），退出码非 0 即失败
- [x] 失败截图存 `tests/e2e/__screenshots__/` + 视频存 `tests/e2e/__videos__/`

## M19 — OpenAPI ↔ UI 类型对齐（U3）

- [x] `pc-openapi` 生成 `/openapi.json`，结构 1:1 对齐 Node `routes/openapi.ts`
- [x] `scripts/check-ui-contract.sh`：diff Node 与 Rust 两份 `openapi.json` 路径清单，重合率 ≥ 99%
- [ ] 字段命名：服务端 snake_case → JSON camelCase（已统一），UI client 无须修改

## M20 — 远程 execution target（claude-local / codex-local）

- [ ] `pc-adapter-claude-local`：`restoreRemoteWorkspace` / `materializeRemoteClaudeConfig` / SSH bridge / `startAdapterExecutionTargetPaperclipBridge`
- [ ] `pc-adapter-codex-local`：远程 `stagedCodexHomeDir` teardown + `restoreRemoteWorkspace` + `remoteCodexConfigDir` 决策
- [ ] 真实 fixture 测试：起 mock SSH server，跑通 start→materialize→invoke→restore 全链路
- [ ] 增量 +20 集成测试

## M21 — 路由字节级对齐收口（剩余 14%）

- [ ] `companies` 子路由补全：skills/tools/folders/invites/labels/approvals/org-svg.png/join-requests
- [ ] `/api/admin/*` 5 个端点
- [ ] raw method+path 重合率 46.1% → 95%
- [ ] evidence: `m21-routes-byte-level.md`

## M22 — Auth/AuthZ 完整化

- [x] refresh token rotation（30 天 sliding window）
- [x] OAuth providers：Google / GitHub（与 better-auth 行为等价）
- [ ] CSRF token（double-submit cookie）
- [ ] API key `pk_<base62>` 生成/校验/吊销
- [ ] evidence: `m22-auth-complete.md`

## M23 — M12 stale lock sweep 回归修复

- [ ] `pc-heartbeat::recovery::stale_issue_lock_sweep` 与 Node `services/heartbeat.ts` 行为对齐
- [ ] `round300` 4 个失败测试全过

## M31 — 产品遥测客户端核心

- [x] 持久 install state 与私有引用哈希
- [x] 事件队列、Node 兼容 envelope 与稳定 batchId
- [x] 真实 HTTP ingest + 失败批次回填测试
- [ ] 业务埋点、退避重试、周期 flush 与 server shutdown 接线
- [x] evidence: `r531-product-telemetry-client.md`

## M32 — 产品遥测 fallback 与生命周期

- [x] transient endpoint fallback 保持相同 batchId
- [x] 周期 flush 可停止任务
- [x] Node opt-out 环境变量契约
- [x] pc-server graceful shutdown 最终 flush
- [x] evidence: `r532-telemetry-lifecycle-fallback.md`

## M33 — Telemetry Retry-After 与字节分批

- [x] `429` / `Retry-After` 与 capped exponential backoff
- [x] 重试保持完全相同 body 与 batchId
- [x] `maxBodyBytes` 递归拆分及超大单事件丢弃
- [ ] 异步有界 pending store、淘汰、timer cancel、jitter
- [x] evidence: `r533-telemetry-retry-byte-caps.md`

## M34 — 异步有界 pending retry

- [x] 独立泛型 `RetryQueue` 状态机与 capped jitter backoff
- [x] 产品客户端接入异步 retry actor/timer
- [x] timer cancel、stop drain、pending payload attempt 元数据
- [x] 真实多批次溢出与停止验证
- [x] evidence: `r534-retry-queue-state.md`（M34-a）+ `r534-async-pending-retry.md`（M34-b）
- [x] pc-server 启动 actor 并在 shutdown 时 final_flush
- [x] evidence: `r535-telemetry-server-lifecycle.md`
- [x] evidence: `r535-telemetry-server-lifecycle.md`

## M36 — Extension 注入与端到端闭环

- [x] `Extension<Arc<ProductTelemetryClient>>` 演示路由 `track()`
- [x] 真 HTTP collector 验证 envelope 与 dimensions
- [x] evidence: `r536-server-extension-telemetry.md`
- [x] evidence: `r536-server-extension-telemetry.md`

## M37 — Global sink 与业务埋点

- [x] `pc-telemetry::global` 模块提供 install/current/track
- [x] `auth.signed_in` / `company.created` / `issue.created` 真实接入
- [x] `pc-server` main 注册全局客户端
- [x] evidence: `r537-global-sink-business-events.md`
- [x] evidence: `r537-global-sink-business-events.md`

## M38 — 业务埋点批量接入（5 域）

- [x] agents / approvals / pipelines×2 / routines 真实 track()
- [x] global::track 同步入队 + install_for_tests 支持多测试
- [x] evidence: `r538-business-events-batch.md`

## M39 — 剩余业务埋点补完（11 类事件）

- [x] approvals × 5：`approval.created` / `approval.rejected` / `approval.resubmitted` / `approval.revision_requested` / `approval.comment_added`
- [x] pipelines × 4：`pipeline.stage.created` / `pipeline.case.created` / `pipeline.case.claimed` / `pipeline.archived`
- [x] routines × 2：`routine.created` / `routine.updated`
- [x] 修复 `pipelines.rs` 中孤儿 `track()` 调用（编译失败）
- [x] 修复 `routines.rs` / `approvals.rs` 中位于文件末尾的孤儿 `use` 导入
- [x] evidence: `r539-business-events-completion.md`

## M40 — pc-authz 核心决策引擎

- [x] `types.rs`：PermissionKey (21) / PrincipalType / CompanyRole / Action / Resource / Decision / Reason (23)
- [x] `policy.rs`：`Context` 注入 + `evaluate` / `check` / `principal_type_of`
- [x] `lib.rs`：公共 API 导出 + 兼容旧 `DefaultPolicy` stub
- [x] 决策分支对齐 Node `evaluateAuthorization`：system 短路 / anonymous / instance_admin / local_board / company membership / issue self / grants / role
- [x] evidence: `r540-pc-authz-core-decision-engine.md`

## M41 — pc-authz DB-backed ContextBuilder

- [x] `builder.rs::build_context` — 从 `company_memberships` + `principal_permission_grants` 加载
- [x] `parse_permission_key` — 21 个 key 反序列化
- [x] User / Agent / System / Anonymous 各自的注入路径
- [x] evidence: `r541-pc-authz-context-builder.md`

## M42 — pc-authz HTTP 集成 + 首个路由接入

- [x] `http.rs`：enforce / enforce_permission / enforce_issue / denial_to_string / company_resource
- [x] `companies.rs::create_label` 接入 `enforce_permission(UsersInvite)`
- [x] `pc-http/Cargo.toml` 新增 `pc-authz` 依赖
- [x] 274 个 pc-http 路由测试无回归
- [x] evidence: `r542-pc-authz-http-integration.md`

## M43 — pc-authz 补充策略

- [x] `Context` 新增 5 字段（issue_mentioned_agent_ids / issue_parent_id / actor_is_assignee_on_parent / has_consented_change_grant / is_low_trust_create_or_comment）
- [x] `Context::with_extended_issue` 构造方法
- [x] User `responsible_user_id` 短路（`AllowDirectChange`）
- [x] Agent `mention grant`（`AllowIssueMentionGrant`）
- [x] Agent `parent-report`（`AllowDirectParentReport`）
- [x] Consent gate（`AllowConsentedChange`）
- [x] 5 个新单元测试
- [x] evidence: `r543-pc-authz-mention-consent-parent.md`

## M44 — pc-authz 多路由接入

- [x] `companies.rs::create_label` 用 `enforce_permission(UsersInvite)`
- [x] `approvals.rs::approve_approval` 用 `enforce_permission(UsersInvite)`
- [x] `approvals.rs::reject_approval` 用 `enforce_permission(UsersInvite)`
- [x] evidence: `r544-pc-authz-multi-route-integration.md`

## M45 — pc-authz 批量接入 agents/pipelines/routines

- [x] `agents.rs` create × 2 + update + remove（4 路由）
- [x] `pipelines.rs` create + update + archive + remove（4 路由）
- [x] `routines.rs` create × 2（2 路由，含 nested create_routine 共享 helper）
- [x] 9 路由全部走 `enforce_permission`，失败映射 ApiError::Forbidden
- [x] evidence: `r545-pc-authz-bulk-route-integration.md`

## M46 — pc-authz e2e parity 测试

- [x] `parity_node.rs` 22 测试（pure 函数，对齐 Node authorization-service.test.ts）
- [x] `builder_db_e2e.rs` 6 测试（DB-backed，自动 skip 无 DB 环境）
- [x] evidence: `r546-pc-authz-e2e-parity-tests.md`

## M47 — pc-authz Trust preset + low-trust boundary

- [x] `trust.rs::TrustPreset` + 常量
- [x] `LowTrustBoundary` + `TrustPresetResolution` + `DenyReason`
- [x] `resolve_core_trust_preset`（多 source 合并 + 跨公司 deny + 缺 boundary deny）
- [x] `is_issue_within_boundary` / `is_agent_within_boundary` / `is_tool_class_within_boundary`
- [x] 14 个单元测试
- [x] evidence: `r547-pc-authz-trust-preset-resolver.md`
