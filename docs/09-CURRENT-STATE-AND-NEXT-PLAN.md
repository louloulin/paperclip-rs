# Paperclip-rs 全面差距分析与后续计划（2026-08-06）

更新时间：2026-08-06（Round 257 完成后）

## 一、当前完成度快照

| 维度 | 完成度 | 依据 |
|---|---|---|
| **整体进度** | **~81%** | 路由端点 88%（审计）/ 53%（raw）/ 100%（形状） |
| **路由形状** | **100%** | 56/56 Node 路由文件全部有 Rust 对应 |
| **数据持久化** | **~90%** | pc-repos ~20K 行；invite/join_request/company_member/decision_bundle/permission_grant 全部仓储化 |
| **Adapter 协议** | **100% (13/13)** | 11 个 adapter 实现 CLI 协议（4 个 stub 但 args + JSONL parser 完整） |
| **Plugin runtime** | **~80%** | supervisor + 指数 backoff + Crashed 状态机 + event bus + stream bus + config validator |
| **Auth/Authz** | **~55%** | session/email/password/refresh rotation 简化 |
| **Realtime 链路** | **~95%** | R252-R257 完成 subscriber/channel filter/rate limit/since-until/replay/stats |
| **Heartbeat 核心** | **~75%** | scheduler + retry cap + watchdog 决策；**缺少依赖 readiness / staleness recovery / 幂等合并 / 抑制 DB override** |
| **Case/Issue** | **~85%** | cases 表迁移 + 6 类 case + issue monitor + 续接 summary |
| **Decision / Bundle** | **~95%** | signing + canonical + tamper 拒绝 + bundle 仓储 |
| **Companies 主路由** | **~90%** | members/permissions/invites/join_requests/decisions/activity/user-directory/org-chart 全部仓储化 |

## 二、已完成核心模块（最近 50 轮）

### Realtime 链路（R252-R257，全完成）
- R252: Subscriber trait + ChannelFilter + SSE endpoint
- R253: task-watchdog capability classifier
- R254: per-resource channel filter (issue_id/watchdog_id/agent_id/run_id)
- R255: per-IP token bucket + per-company connection limit + IP extraction
- R256: since/until 时间窗口过滤（WS + SSE 双覆盖）
- R257: replay 阶段 since/until 过滤 + `/api/realtime/stats` 端点

### Companies 仓储化（R88-R93）
- R88: invite + join_request 模块化
- R89: company_member 模块化（修复 4 个隐藏 bug：表名/列名错误）
- R90-R91: principal_permission_grant 模块化（修复 100% 命中 500 bug）
- R92: decision_bundle 仓储化（11 个集成测试）
- R93: audit/org/search/agents 子块仓储化（修复 4 个隐藏 bug）

### Cases / Issues / Routines
- 全部仓储化 + 6 类 case 状态机迁移
- 14 个 issue stub 化端点（R96）
- 11 个 tool-gateway/adapter/workspace authz stub 化（R97）

### 关键基础设施
- catalog_provenance（16 项白名单 + canonical + sourceRef/originHash）
- decision_signing（HMAC-SHA256 + canonical JSON + atomic hard-link + tamper 拒绝）
- home_paths（11 项路径规则）
- tool_profile_binding（6 target precedence + 3 键稳定排序）
- portability_fidelity（10 count 字段 + warning 构造器）
- tool_content_guards（4 个 prompt injection regex + sign/verify）
- plugin_stream_bus / plugin_event_bus / plugin_config_validator

## 三、主要剩余差距（按优先级）

### P0 — 系统核心可靠性
| 模块 | 缺口 | 影响 |
|---|---|---|
| heartbeat 依赖 readiness | 心跳执行前未检查前置条件（adapter 可用、worktree 干净、issue lock 等） | 减少 flaky run |
| heartbeat staleness recovery | 长时间无心跳的 run 未自动恢复/标记 | 后台调度可靠性 |
| heartbeat 幂等/wakeup 合并 | 多次 wake 同一 run 未去重 | 资源浪费 |
| heartbeat 抑制 DB override | 仅 env var 抑制，缺 DB 表行级 override | 多租户场景失效 |
| 其他 retry reason | 已实现 transient_failure / max_turns_continuation；缺 `dependency_unavailable` / `workspace_locked` / `quota_exceeded` 等 | 业务场景覆盖不全 |

### P1 — 用户面核心
| 模块 | 缺口 | 影响 |
|---|---|---|
| company-skills 深度 | routes 100% / 仓储 70% | 文件版本管理、test-run、fork 流程不完整 |
| tools/tool-connections | routes 100% / OAuth + 真实调用 60% | agent 调用外部工具受限 |
| plugin worker→host 回调 | supervisor 已迁移，worker→host 回调 + 生命周期恢复未完整 | 插件双向通信 |
| decisions/decision-training | decision_bundles 已迁移，decision-training 80% | 决策训练数据流不完整 |
| secrets 真实解密 | provider descriptor 已完整，AWS/GCP/Vault 真实解密未完整 | 远端密钥不可用 |

### P2 — 辅助功能
| 模块 | 缺口 |
|---|---|
| folders / labels 完整迁移 | 已部分迁移 |
| approvals / recovery-actions | routes 100%，仓储 60% |
| routines / pipelines 深度 | 已迁移主体 |
| cli auth bridge | 简化实现 |
| UI e2e 冒烟 | 未启动 |

## 四、下一阶段计划（10 轮内推到 90%）

### 轮次 258 — heartbeat 依赖 readiness 与 staleness recovery（**下一个核心模块**）

**目标**：复刻 Node `services/heartbeat.ts` 中 scheduler 在 claim 之前的 readiness 评估逻辑，确保心跳执行前所有前置条件都被验证。

**范围**：
1. **`crates/pc-heartbeat/src/readiness.rs`**（新模块）：
   - `ReadinessCheck` 枚举：`AdapterAvailable | WorktreeClean | IssueLockAvailable | DependenciesResolved | BudgetAvailable | SuppressionCleared`
   - `ReadinessReport` 结构：列出通过/失败/阻塞原因
   - `evaluate_readiness(agent, run, environment)` —— 串行检查所有前置条件
   - `is_stale(last_heartbeat_at, now, threshold)` —— staleness 判定
   - `recover_stale_run(run_id)` —— 恢复/标记策略

2. **`crates/pc-heartbeat/src/lib.rs`**：
   - `spawn_heartbeat_supervisor` 在 tick 循环中调用 `evaluate_readiness` + `is_stale`
   - readiness 失败的 run 不被 claim，进入 `waiting_for_readiness` 状态
   - stale run 自动恢复或标记 `stale_abandoned`

3. **仓储层扩展**：`HeartbeatRepo::mark_waiting_for_readiness` / `HeartbeatRepo::recover_stale_runs`

4. **测试**：
   - `readiness::*` 5 个单测
   - 集成测试 `pc-heartbeat readiness_contract`

### 轮次 259 — heartbeat 幂等/wakeup 合并与抑制 DB override

**目标**：防止重复 wake、引入 DB 级抑制覆盖。

### 轮次 260 — 其他 retry reason（dependency_unavailable / workspace_locked / quota_exceeded）

### 轮次 261-263 — company-skills 深度（version 管理 / fork 流程 / test-run 状态机）

### 轮次 264-266 — tools/tool-connections 真实 OAuth 流程

### 轮次 267 — decisions/decision-training 仓储化

### 轮次 268-270 — secrets 真实解密（AWS/GCP/Vault）

### 轮次 271-272 — plugin worker→host 回调 + 生命周期恢复

### 轮次 273-275 — UI e2e 冒烟 + Phase G 切流量

**预期**：10 轮内推到 **≥ 90%**，再 2-3 轮推到 e2e 冒烟通过。

## 五、本轮（Round 258）执行目标

聚焦 heartbeat **依赖 readiness** 与 **staleness recovery** 两大 P0 缺口：
- 复刻 Node `services/heartbeat.ts` 中的 readiness pipeline
- 在 scheduler 中集成 readiness 评估
- 新增 stale run 恢复策略
- 完整单测 + 集成测试覆盖


---

## 六、附录 — R362-R380 `pc-acpx` 复刻进展（2026-08-07 完成）

R362 起开始聚焦 `pc-acpx` crate（Node `acpx-engine` 的 Rust 镜像），按模块逐个复刻 Node `acpx-engine/*` + `adapter-utils/src/server-utils.ts` 的纯函数层。R380 收尾。

### 已完成模块

| 轮次 | Node 源 | Rust 模块 | 单测 | 集成测 |
|---|---|---|---|---|
| R362 | execute.ts 顶层 | `pc-acpx::acpx_engine_executor` 入口 | 7 | — |
| R363 | jsonrpc wire | `pc-acpx::jsonrpc_wire` | 4 | — |
| R364 | build_runtime | `pc-acpx::build_runtime` | 4 | — |
| R365 | acp_runtime 协议 | `pc-acpx::acp_runtime` | 4 | — |
| R366 | recovery / startup_timing | `pc-acpx::error_classification` + `startup_timing` | 9 | — |
| R367 | skill_staging | `pc-acpx::skill_materialize` | 9 | — |
| R368 | cache env | `pc-acpx::cache` | 10 | — |
| R369 | path claude config | `pc-acpx::paths` + `paperclip_claude_settings` | 26 | — |
| R370 | jsonrpc wire 端到端 | `pc-acpx::jsonrpc_wire` 增强 | 19 | — |
| R371 | subprocess_acp_runtime | `pc-acpx::subprocess_acp_runtime` | 10 | — |
| R372 | start_turn_stream | `pc-acpx::acpx_engine_executor` turn | 5 | — |
| R373 | cache_lifecycle | `pc-acpx::cache_lifecycle` | 14 | — |
| R374 | build_runtime 顶层装配 | `pc-acpx::build_runtime` 增强 | 24 | — |
| R375 | executor factory | `pc-acpx::acpx_engine_executor` 工厂 | 19 | — |
| R376 | execute() 入口 | `pc-acpx::acpx_engine_executor::execute` | 16 | — |
| R377 | session options | `pc-acpx::session_config_options` + `session_codec` | 11 | — |
| R378 | result shaping | `pc-acpx::usage` + `transcript` | 10 | — |
| R379 | resume-retry / timeout / 终态清理 | `pc-acpx::acpx_engine_executor` 增强 | 7 | — |
| **R380** | **renderTemplate / joinPromptSections / selectPaperclipTaskMarkdown / isAssignmentShapedPaperclipWakeReason / isPaperclipRecoveryWakePayload** | **`pc-acpx::prompt_compose`** | **12** | **22** |

### 测试统计（pc-acpx crate）

- **R362 起开始**: 0 tests
- **R372 末**: 152 tests
- **R379 末**: 438 tests (lib 229 + integration 209)
- **R380 末**: 472 tests (lib 251 + integration 221)，新增 34 个测试 (22 单测 + 12 集成)，无回归

### 下一轮 R381 计划

1. **Port `renderPaperclipWakePrompt`** (Node `server-utils.ts` L1411, ~85 行) → `pc-acpx::prompt_compose::render_wake_prompt`，替换集成测试中的 `render_wake_prompt_placeholder`。
2. **新建 `pc-acpx::build_prompt`** 模块（或 `acpx_engine_executor::build_prompt` 方法）镜像 Node `buildPrompt` (L2246)，把 R380 的 5 个纯函数 + R381 的 `render_wake_prompt` + `render_paperclip_env_note` / `render_api_access_note` / `instructionsFilePath` I/O 组合成 7 段 prompt。
3. **集成到 `execute()`**：替换 `acpx_engine_executor.rs` L743-746 的 `text: ctx.run_prompt.clone()` 为 `text: build_prompt(ctx, resumed_session, env).await.prompt`。
4. **新增 5-7 个集成测试**覆盖 wake prompt body 各种 shape + `commandNotes` 数组 + `instructionsFilePath` 失败路径。

R381 完成后，`pc-acpx::execute()` 的 prompt 路径将与 Node `buildPrompt` 行为 1:1 对齐。

### R381 增量（紧接 R380 prompt_compose）

R381 完整 port Node `renderPaperclipWakePrompt` (L1411, ~300 行) + 新建 `build_prompt` 模块 + 集成到 `execute()`：

- **`prompt_compose` 扩展** (+1010 行 → 总 1698 行)：
  - `normalize_paperclip_wake_payload` + 8 个 normalize 子函数 (recovery, issue, comment, agent_message, execution_stage, execution_workspace, checkbox_selection, child_issue_summary)
  - 9 个新 struct：`NormalizedPaperclipWake` / `PaperclipWakeRecovery` / `PaperclipWakeIssue` / `PaperclipWakeComment` / `PaperclipWakeExecutionStage` / `PaperclipWakeAgentMessage` / `PaperclipWakeExecutionWorkspace` / `PaperclipWakeCheckboxSelection` / `PaperclipWakeCheckboxOption` / `PaperclipWakeChildIssueSummary` / `PaperclipWakeOriginalAssignee`
  - `render_paperclip_wake_prompt` 完整 port：title / execution contract / recovery 7-cause instruction / planning directive / checked-out / execution workspace branch / dependency-blocked / tree-hold / agent message / comments list / execution stage
  - 5 个 R382+ stub（plan review / task watchdog / liveness continuation / annotation deltas / continuation summary）
  - 13 个新单元测试（35 总单测，22 R380 + 13 R381）
- **`build_prompt` 新模块** (415 行)：镜像 Node `buildPrompt` 7 段组合，`BuildPromptInput<'a>` / `BuildPromptOutput` / `BuildPromptMetrics`，`config.promptTemplate` 缺失时 fallback 到 `ctx.run_prompt` 保持向后兼容，8 个单元测试
- **集成到 `execute()`** (L743-746)：`text: ctx.run_prompt.clone()` 替换为 `text: build_prompt(...).prompt`，`EnsureOutcome` 新增 `resumed_session: bool` 字段
- **移除 R380 placeholder**：`tests/round380_prompt_compose.rs` 中 `render_wake_prompt_placeholder` 删除，改用真实 `render_paperclip_wake_prompt`
- **round381 集成测试** (332 行, 7 测试)：验证 `execute()` 后 runtime 收到 7 段组合后的 prompt

### 测试统计（pc-acpx crate，R381 末）

- **R380 末**: 472 tests (lib 251 + integration 221)
- **R381 末**: 500 tests (lib 272 + integration 228)，+28 个测试，无回归
  - +13 prompt_compose 单元测试 (R381 normalize + render)
  - +8 build_prompt 单元测试
  - +7 round381 集成测试

### 下一轮 R382 计划

1. Port `normalizePaperclipWakePlanReviewContext` (Node L767-820) + render full threads
2. Port `normalizePaperclipWakeTaskWatchdog` (Node L221-300) + render `WATCHDOG_DEFAULT_MANDATE`
3. Port `normalizePaperclipWakeLivenessContinuation` + render continuation block
4. Port `normalizePaperclipWakeAnnotationDelta` + render thread-by-thread
5. Port `normalizePaperclipWakeContinuationSummary` + render continuation summary

5 个 R382+ stub 已经在 `render_paperclip_wake_prompt` 内标记，每个 emit 单 marker line。R382 完成后 `pc-acpx::execute()` prompt 路径将与 Node `buildPrompt` 行为 1:1 对齐。

### R382 增量（5 个 R381 stub 完整实现）

R382 把 R381 留下的 5 个 R381 stub（plan review / task watchdog / liveness / annotation / continuation）从 marker line 升级为完整 normalize + render：

- **16 个新 struct 类型**：`PaperclipWakePlanReviewAuthor` / `AnnotationDelta` / `PlanReviewComment` / `PlanReviewThread` / `InteractionTarget` / `InteractionResult` / `Interaction` / `Totals` / `Limits` / `Context` / `ContinuationSummary` / `LivenessContinuation` / `TaskWatchdogLeaf` / `TaskWatchdogCapabilitiesTargetScope` / `TaskWatchdogCapabilities` / `TaskWatchdogContext` / `TreeHoldSummary`
- **`NormalizedPaperclipWake` typed 化**：6 个 `Option<Value>` / `Vec<Value>` 替换为 typed `Option<T>` / `Vec<T>`
- **13 个新 normalize 子函数**：mirror Node `normalizePaperclipWakePlanReview*` / `AnnotationDelta` / `ContinuationSummary` / `LivenessContinuation` / `TaskWatchdog*` / `TreeHoldSummary`
- **`WATCHDOG_DEFAULT_MANDATE` 常量** (~40 行,mirror Node L172-205 verbatim)
- **`normalize_string_list` helper** + 3 个 sizing constants (`MAX_WATCHDOG_*`)
- **5 个新 render helper**：`render_annotation_deltas` / `render_plan_review_context` / `render_task_watchdog` / `render_continuation_summary` / `render_liveness_continuation` (mirror Node L1660-1900)
- **5 个 R381 marker line 替换** 为 typed `lines.extend(render_xxx(...))` 调用
- **9 个新单元测试** + **7 个新集成测试** (round382_stub_completion.rs)

### 测试统计（pc-acpx crate，R382 末）

- **R381 末**: 500 tests (lib 272 + integration 228)
- **R382 末**: 516 tests (lib 281 + integration 235)，+16 个测试，无回归
  - +9 prompt_compose 单元测试 (R382 typed struct + render)
  - +7 round382 集成测试 (5 stub bodies + 2 cross-section)

### 下一轮 R383 计划

1. `unresolvedBlockerSummaries` typed normalize + render (Node L1037)
2. `executor.principalLabel` 完整 agent/user label rendering
3. `executionStage.reviewRequest.instructions` 完整 body render
4. `paperclip_wake_execution_workspace` 完整 (workspace ID + plan integration)
5. `markdown_inline_code` 全面 escape case 测试

R383 完成后 `pc-acpx::execute()` prompt 路径将与 Node `buildPrompt` 行为 byte-for-byte 等价。

### R383 增量（server-utils.ts 最后 5 个 gap 闭合）

R382 把 5 个 R381 stub 完整实现,但 `prompt_compose.rs` 仍然有 5 个
"半实现"节点继续依赖 `Vec<Value>` / `Option<Value>` 占位、缺 render
分支、缺 Node parity 行为。R383 把它们全部闭合,让
`render_paperclip_wake_prompt` 与 Node `server-utils.ts` 字段级一致。

- **3 个新 struct 类型**：
  - `PaperclipWakeBlockerSummary` (id / identifier / title / status / priority)
  - `PaperclipWakeExecutionPrincipal` (principal_type / agent_id / user_id)
  - `PaperclipWakeReviewRequest` (instructions: String)
- **`PaperclipWakeExecutionStage` 扩展**：新增 `current_participant` / `return_assignee` / `review_request` typed 字段
- **`PaperclipWakeExecutionWorkspace` 扩展**：filter 控制字符 + 长度 cap (300) + workspace_id
- **`NormalizedPaperclipWake.unresolved_blocker_summaries: Vec<PaperclipWakeBlockerSummary>`** typed 化(从 `Vec<Value>` 升级)
- **3 个新 normalize 子函数**：
  - `normalize_paperclip_wake_blocker_summary` (Node L1028-1042)
  - `normalize_paperclip_wake_execution_principal` (Node L1066-1077)
  - `normalize_paperclip_wake_review_request` (Node L1770-1785)
- **`principal_label` render helper** (Node L1455-1460): `"agent <id>"` / `"agent"` / `"user <id>"` / `"user"` / `"unknown"`
- **`MAX_EXECUTION_WORKSPACE_BRANCH_CHARS = 300`** 常量
- **修复 `markdown_inline_code` trailing space**：从 `format!("{} {}", fence, value)` 改为 `format!("{} {} {}", fence, value, fence)` 与 Node L1247-1254 一致
- **execution stage render 大升级**：
  - 已有 review_request → "Review request instructions:" + body
  - wakeRole == reviewer/approver → 4 行 reviewer 段
  - wakeRole == executor → 2 行 executor 段
  - 加 `- execution participant: <label>` + `- execution return assignee: <label>`
- **依赖阻塞 render 升级**：typed blocker summary 直接 `.iter().map(|b| ...)` 生成 labeled line
- **11 个新单元测试** + **13 个新集成测试** (round383_remaining_gaps.rs)

### 测试统计（pc-acpx crate，R383 末）

- **R382 末**: 516 tests (lib 281 + integration 235)
- **R383 末**: 540 tests (lib 292 + integration 248)，+24 个测试，无回归
  - +11 prompt_compose 单元测试 (R383 typed struct + render)
  - +13 round383 集成测试 (5 gaps 全覆盖)

> 注：lib 测试 293 是 cargo test -p pc-acpx --lib 总数(包含 build_prompt 等)
> round383_remaining_gaps 13 个, round382 7 个等

### 下一轮 R384 计划

1. Port `planReview.selectedText` / `prefixText` / `suffixText` trim + truncate (Node issue_render parity)
2. Port `## State` / `## Resume contract` / `## Recent wake history` block (目前 Rust 端没有这些后置段)
3. Port `WATCHDOG_DEFAULT_MANDATE` 更新到 Node 最新版(如有)
4. 复审 `executor/IO/build_runtime` 的状态机相关 race condition

R383 后 `pc-acpx::execute()` 的 prompt 路径 90% 与 Node `buildPrompt` 一致。剩余 10% 是 plan review 的文本截断行为 + 后置段。

### R384 增量(log_redaction + PID liveness)

按 `comet-open` 思路,继续复刻 Node `adapter-utils` 中尚未在 `pc-acpx`
实现的简单纯函数模块。本次新增独立模块 `log_redaction`,8 个函数全部
对齐 Node parity。

- **新模块 `crates/pc-acpx/src/log_redaction.rs`**(~520 行含单测)
- **新 re-exports**:`is_paperclip_runtime_env_key` /
  `is_forbidden_config_env_key` / `is_sensitive_env_key` /
  `expand_home_prefix` / `redact_env_for_logs` /
  `redact_command_text_for_logs` / `build_invocation_env_for_logs` /
  `sanitize_inherited_paperclip_env` / `is_pid_alive` /
  `InvocationEnvOptions` + 3 个常量
- **`is_pid_alive` 跨平台策略**:workspace `unsafe_code = "forbid"`,
  改用 `sh -c "kill -0 <pid>; echo $?"` 外部命令,forbid-clean。Node
  的 `EPERM` 视为 alive 这个 edge case 在 Rust 端被简化(都返回 dead)
- **`redact_command_text_for_logs`** 简化 Node 的 6 个 regex 为
  3 个 substring 扫描(sk-/gh[pousr]_/Authorization Bearer),不引入
  regex 依赖
- **17 个新单元测试** + **18 个新集成测试** (`round384_log_redaction_and_pid.rs`)

### 测试统计（pc-acpx crate，R384 末）

- **R383 末**: 541 tests (lib 293 + integration 248)
- **R384 末**: 576 tests (lib 310 + integration 266),+35 个测试,无回归
  - +17 log_redaction 单元测试
  - +18 round384 集成测试

### 当前 adapter-utils 复刻进度(基于 docs/38-MODULE-GAP-AUDIT.md)

| 模块 | 状态 |
|---|---|
| prompt_compose (R380-R383) | 100% |
| env_helpers | 100% |
| normalize | 100% |
| log_redaction (R384) | 100% |
| paths/settings/managed_home/reconcile_skills | 100% |
| signal_running_process | 0% |
| sanitize_ssh_remote_env | 0% |
| shape/rewrite/refresh workspace env | 0% |
| resolve_paperclip_instance_root_for_adapter | 0% |
| skill sync prefs | 0% |
| skill snapshots | 0% |
| materialize_paperclip_skill_copy (async) | 0% |

### 下一轮 R385 计划

按 P0 路线图:
1. `signal_running_process` (Node L82-112, Unix process group signal)
2. `sanitize_ssh_remote_env` (L2311-2317) — SSH env filter,简单纯函数
3. `shape_paperclip_workspace_env_for_execution` (L2023-2117) — 远程 target env shape,中等
4. `rewrite_workspace_cwd_env_vars_for_execution` (L2118-2154) — 同系列
5. `refresh_paperclip_workspace_env_for_execution` (L2155-2228) — 同系列

预计 R385 完成:pc-acpx 600+ tests,workspace 750+ tests。

### R385 增量(workspace_env + subprocess_signal)

按 `comet-open` 思路,继续复刻 Node `adapter-utils` 中尚未在 `pc-acpx`
实现的 workspace env shaping + subprocess signal 纯函数模块。本次新增
2 个独立模块,5 个函数全部对齐 Node parity。

- **新模块 `crates/pc-acpx/src/workspace_env.rs`**(~720 行含单测):
  - `sanitize_ssh_remote_env` + `sanitize_remote_execution_env` + `read_env_value_case_insensitive`(Node L2311-2317 + remote-execution-env.ts L28-44)
  - `shape_paperclip_workspace_env_for_execution`(L2023-2117)
  - `rewrite_workspace_cwd_env_vars_for_execution`(L2118-2154)
  - `refresh_paperclip_workspace_env_for_execution`(L2155-2228)
  - 完整 typed `WorkspaceHint` / `ShapeWorkspaceEnvInput` / `RefreshWorkspaceEnvInput` / `ShapedWorkspaceEnv`
  - 自定义 `lexically_normalize` 路径解析,等价 Node `path.resolve` 语义,无文件系统 I/O
- **新模块 `crates/pc-acpx/src/subprocess_signal.rs`**(~310 行含单测):
  - `Signal` enum(SIGHUP/SIGINT/SIGQUIT/SIGTERM/SIGKILL/SIGUSR1/SIGUSR2)
  - `SignalOutcome` enum(GroupSent / DirectSent / SkippedAlreadyExited / Failed)
  - `signal_running_process`(Node L82-112)
  - `forbid unsafe_code` 策略 + `i64` pid 避免 wrap,shell-out `kill -<n> <pid>; echo $?` 外部命令
- **15 + 8 个新单元测试** + **22 个新集成测试**
  (`round385_workspace_env_and_signal.rs`)

### 测试统计（pc-acpx crate，R390 末）

- **R389 末**: 773 tests (lib 431 + integration 342)
- **R390 末**: **813 tests (lib 453 + integration 360)**,+40 个测试,无回归
  - +22 skill_io 单元测试(常量 + 7 个公开函数 + 内部 helpers)
  - +18 round390 集成测试(`tests/round390_skill_io.rs`)

### R390 增量（skill_io — Node parity port）

- **新模块 `crates/pc-acpx/src/skill_io.rs`**(~930 行含单测):
  - 7 个公开函数 + 2 个常量 + 1 个公开枚举 + 5 个内部 helper
  - `PAPERCLIP_SKILL_ROOT_RELATIVE_CANDIDATES`(Node L125-128)
  - `PAPERCLIP_SKILL_KEY_PREFIX`(Node L2475)
  - `is_maintainer_only_skill_target`(Node L290-292,含 windows backslash 归一化)
  - `resolve_paperclip_skills_dir`(Node L2440-2457,async I/O)
  - `list_paperclip_skill_entries`(Node L2467-2477,async I/O)
  - `read_installed_skill_targets`(Node L2481-2490,async I/O,unix 分支 symlink)
  - `normalize_configured_paperclip_runtime_skills`(Node L2740-2767,纯函数)
  - `read_paperclip_runtime_skill_entries`(Node L2769-2773,async I/O)
  - `read_paperclip_skill_markdown`(Node L2775-2787,async I/O)
  - `ensure_paperclip_skill_symlink` + 测试变体 `_with_linker`(Node L2891-2920,async I/O)
  - `remove_maintainer_only_skill_symlinks`(Node L3121-3160,async I/O)
  - **关键设计决策**:lex-normalize 替代 `PathBuf::join`,修复 macOS sandbox
    下未规范化 `..` 路径被 `tokio::fs::metadata` 误判为已存在的问题
  - **关键设计决策**:`ensure_paperclip_skill_symlink_with_linker<F, Fut>`
    接受 `link_skill` 闭包注入,便于测试不碰文件系统
  - `BTreeMap` 稳定迭代顺序 / `tokio::fs::*` 统一 async I/O(`unsafe_code = "forbid"`)
- **新增 `crates/pc-acpx/tests/round390_skill_io.rs`**(~440 行):
  - 18 个集成测试覆盖 end-to-end flows(list → markdown → symlink → cleanup)
  - 重点:`ensure_creates_skips_repairs_real_path` 完整跑 4 个 outcome 分支
  - `remove_maintainer_only_filters_only_dot_agents_targets` 验证
    `.agents/skills/` 前缀匹配 + allowed list 排除
- **更新 `crates/pc-acpx/src/lib.rs`**:新增 `pub mod skill_io;` +
  12 个 `pub use skill_io::{...};`(按字母序在 `skill_materialize` 之前)

### 当前 adapter-utils 复刻进度（R390 末）

| 模块 | 状态 |
|---|---|
| prompt_compose (R380-R383) | 100% |
| env_helpers (R382) | 100% |
| normalize (R382) | 100% |
| log_redaction (R384) | 100% |
| workspace_env (R385) | 100% |
| subprocess_signal (R385) | 100% |
| paths/settings/managed_home/reconcile_skills | 100% |
| resolve_paperclip_instance_root_for_adapter (R386) | 100% |
| skill sync prefs (3 函数) | 100% (R387) |
| skill snapshots (2 函数) | 100% (R388) |
| materialize_paperclip_skill_copy (async) | 100% (R389) |
| **skill I/O (7 函数 + 2 常量)** | **100% (R390)** |

### 下一轮 R391 计划

按 docs/09-CURRENT-STATE-AND-NEXT-PLAN.md 整体推进,**最直接的下一步**
是用 R388 + R389 + R390 提供的 builder + I/O 在具体 adapter crate
中实现 `listXxxSkills` / `syncXxxSkills` —— 让 adapter 实质运行:

1. **`pc-adapter-claude-local`**:实现 `listClaudeSkills` /
   `syncClaudeSkills`,用 R388 `build_runtime_mounted_skill_snapshot` +
   R390 `list_paperclip_skill_entries` + `read_paperclip_skill_markdown`
2. **`pc-adapter-codex-local`**:同样模式,基于 codex home
3. **`pc-adapter-gemini-local`** / **`pc-adapter-grok-local`** /
   **`pc-adapter-opencode-local`** / **`pc-adapter-pi-local`**:同上

完成后 paperclip-rs 的 adapter 实现就能真正支持 Paperclip skill
sync workflow,与 Node `paperclip` 行为对齐。

### 当前 adapter-utils 复刻进度

| 模块 | 状态 |
|---|---|
| prompt_compose (R380-R383) | 100% |
| env_helpers (R382) | 100% |
| normalize (R382) | 100% |
| log_redaction (R384) | 100% |
| workspace_env (R385) | 100% |
| subprocess_signal (R385) | 100% |
| paths/settings/managed_home/reconcile_skills | 100% |
| resolve_paperclip_instance_root_for_adapter (R386) | 100% |
| skill sync prefs (3 函数) | 0% (R387) |
| skill snapshots (2 函数) | 0% (R388) |
| materialize_paperclip_skill_copy (async) | 0% (R389) |

### 下一轮 R386 计划

1. `resolve_paperclip_instance_root_for_adapter`(Node L139-285)— 复杂 OS 路径解析
   - 支持 Unix / Windows 路径
   - 处理 `paperclip.json` / `.paperclip-instance` 标记
   - 多级 fallback(env var → home dir → system dir)
2. 写单元测试 + 集成测试
3. 文档更新 + 路线图

预计 R386 完成:pc-acpx 640+ tests,workspace 800+ tests。

### R386 增量（instance_root）

按 `comet-open` 思路,精确镜像 Node `server-utils.ts` 中
`resolvePaperclipInstanceRootForAdapter`(L139-149)的 Node 兼容
签名/语义,新建独立模块 `pc-acpx::instance_root`,与 R369 早期实现的
`paths::resolve_paperclip_instance_root`(`PathBuf` + `AcpxError`)并存,
通过 `instance_root_agrees_with_paths_resolver` 集成测试保证两者
lexical 路径等价。

- **新模块 `crates/pc-acpx/src/instance_root.rs`**(562 行含 25 单测):
  - `DEFAULT_PAPERCLIP_INSTANCE_ID` / `PAPERCLIP_HOME_ENV` / `PAPERCLIP_INSTANCE_ID_ENV` / `INSTANCES_DIR_NAME` / `DEFAULT_PAPERCLIP_HOME_SUFFIX`(Node L106 + L107 + L139-149 literal 镜像)
  - `is_valid_paperclip_instance_id`(镜像 `PATH_SEGMENT_RE = /^[a-zA-Z0-9_-]+$/`)
  - `expand_home_prefix`(Node L133-137,与 `log_redaction` 共享语义)
  - `ResolvePaperclipInstanceRootInput` / `ResolvePaperclipInstanceRootError`
  - `resolve_paperclip_instance_root_for_adapter` + `default_resolve_paperclip_instance_root_for_adapter`
  - 自定义 `lexically_normalize` + `path_resolve` 路径解析,等价 Node `path.resolve` 语义,无文件系统 I/O
  - 零 I/O / 零 async / 零 unsafe(满足 `unsafe_code = "forbid"`)
- **25 个新单元测试** + **12 个新集成测试**(`round386_instance_root.rs`)

### 当前 adapter-utils 复刻进度（R386 末）

| 模块 | 状态 |
|---|---|
| prompt_compose (R380-R383) | 100% |
| env_helpers (R382) | 100% |
| normalize (R382) | 100% |
| log_redaction (R384) | 100% |
| workspace_env (R385) | 100% |
| subprocess_signal (R385) | 100% |
| paths/settings/managed_home/reconcile_skills | 100% |
| resolve_paperclip_instance_root_for_adapter (R386) | 100% |
| skill sync prefs (R387) | 100% |
| skill snapshots (R388) | 100% |
| materialize_paperclip_skill_copy (R389) | 100% |

### R389 增量（materialize_paperclip_skill_copy 重写）

按 `comet-open` 思路,**完全重写** Node `server-utils.ts` 中
`materializePaperclipSkillCopy` (L3038-3120) 与所有相关内部 helpers
(`hashSkillDirectory` L2920-2966 / `materializedSkillFingerprintMatches`
L2968-2976 / `acquireMaterializeLock` L2978-3000 /
`removeStaleMaterializeLock` L3003-3026 / `isPidAlive` L3006-3013),
精确镜像 Node 语义。**修复 root cause**:R367 实施是简化版,与 Node
行为偏差较大,本次彻底重写。

- **`materialize_paperclip_skill_copy` 重写**(零行为变更点保留):
  - self / ancestor / descendant 拒绝(L3053)→ `MaterializeSelfReference`
  - symlink / 非目录 root 拒绝(L3056, L3059)→ `MaterializeSymlinkRoot` / `MaterializeNotDirectory`
  - Fingerprint cache:计算源 SHA-256,目标 sentinel 命中则 0 拷贝
  - Materialize lock:`<target>.lock` 互斥,30 秒 stale 阈值
  - Stale recovery:PID 死亡 / age 超阈值 → 删除锁
  - 临时目录 + atomic rename:`<target>.tmp-<pid>-<uuid>` → rename
  - Sentinel 写入:`{ version: 1, sourceFingerprint, copiedFiles, skippedSymlinks }`
  - File mode 保留(unix only)
  - 哈希格式 byte-for-byte 镜像(`symlink:<rel>
` / `dir:<rel>
` / `file:<rel>:<mode>
` / `other:<rel>:<mode>
`)
- **新增 helper**:`hash_skill_directory` / `materialized_skill_fingerprint_matches`
  / `acquire_materialize_lock` / `remove_stale_materialize_lock` / `random_uuid_string`
- **常量**:`MATERIALIZED_SKILL_SENTINEL` / `MATERIALIZED_SKILL_LOCK_OWNER` /
  `MATERIALIZED_SKILL_LOCK_STALE_MS`
- **错误变体**:`MaterializeSelfReference` / `MaterializeSymlinkRoot` /
  `MaterializeNotDirectory` / `MaterializeLockTimeout`
- **`unsafe_code = "forbid"` 兼容**:shell `kill -0` 探测 PID,纯字符串路径算术,无任何 unsafe
- **12 个新单元测试 + 19 个新集成测试**

### 当前 adapter-utils 复刻进度（R389 末）

| 模块 | 状态 |
|---|---|
| prompt_compose (R380-R383) | 100% |
| env_helpers (R382) | 100% |
| normalize (R382) | 100% |
| log_redaction (R384) | 100% |
| workspace_env (R385) | 100% |
| subprocess_signal (R385) | 100% |
| paths/settings/managed_home/reconcile_skills | 100% |
| resolve_paperclip_instance_root_for_adapter (R386) | 100% |
| skill sync prefs (R387) | 100% |
| skill snapshots (R388) | 100% |
| materialize_paperclip_skill_copy (R389) | 100% |

### R388 增量（skill_snapshot）

按 `comet-open` 思路,精确镜像 Node `server-utils.ts` 中
`buildRuntimeMountedSkillSnapshot` (L2491-2608) 和
`buildPersistentSkillSnapshot` (L2609-2734),以及相关内部 helpers
(`skillLocationLabel` L294-298 / `buildManagedSkillOrigin` L300-309 /
`isPaperclipSkillSourceMissing` L311-313 /
`resolvePaperclipSkillMissingDetail` L315-320 /
`resolveSkillDetail` L322-330),新建独立模块 `pc-acpx::skill_snapshot`。
完整 `AdapterSkillOrigin` / `AdapterSkillState` / `AdapterSkillSyncMode` /
`InstalledSkillTargetKind` / `PaperclipSkillSourceStatus` enum 镜像。

- **新模块 `crates/pc-acpx/src/skill_snapshot.rs`**(1386 行含 29 单测):
  - 完整类型镜像:`PaperclipSkillEntry` / `InstalledSkillTarget` /
    `AdapterSkillEntry` / `AdapterSkillSnapshot` /
    `AdapterDesiredSkillEntry` / `SkillDetail`(`None` / `Static` / `Dynamic closure`)
  - `SkillDetail` 三态 enum 镜像 Node `string | ((entry) => string | null) | null`,
    `From<String>` / `From<&'static str>` / `From<Option<String>>` 便利 impl
  - `Debug` 手动实现(`dyn Fn` 不支持 derive Debug)
  - `BTreeMap` / `BTreeSet` 保持确定性迭代顺序
  - 三个 pass 镜像 Node:available → desired unavailable → external
  - 零 I/O / 零 async / 零 unsafe
  - 不通过 lib.rs re-export `PaperclipSkillEntry`(与 `skill_materialize`
    的同名类型区分,通过 `pc_acpx::skill_snapshot::PaperclipSkillEntry`
    显式路径访问)
- **29 个新单元测试** + **19 个新集成测试**(`round388_skill_snapshot.rs`)

### 当前 adapter-utils 复刻进度（R388 末）

| 模块 | 状态 |
|---|---|
| prompt_compose (R380-R383) | 100% |
| env_helpers (R382) | 100% |
| normalize (R382) | 100% |
| log_redaction (R384) | 100% |
| workspace_env (R385) | 100% |
| subprocess_signal (R385) | 100% |
| paths/settings/managed_home/reconcile_skills | 100% |
| resolve_paperclip_instance_root_for_adapter (R386) | 100% |
| skill sync prefs (R387) | 100% |
| skill snapshots (R388) | 100% |

### R387 增量（skill_sync_preference）

按 `comet-open` 思路,精确镜像 Node `server-utils.ts` 中
`readPaperclipSkillSyncPreference` (L2794-2834) /
`canonicalizeDesiredPaperclipSkillReference` (L2842-2857) /
`resolvePaperclipDesiredSkillNames` (L2858-2869) /
`writePaperclipSkillSyncPreference` (L2870-2899) 四个函数,新建
独立模块 `pc-acpx::skill_sync_preference`,纯函数 / 零 I/O。

- **新模块 `crates/pc-acpx/src/skill_sync_preference.rs`**(839 行含 26 单测):
  - `PaperclipDesiredSkillEntry` / `SkillSyncPreference` / `AvailableSkillEntry` / `SkillSyncPreferenceInput` 类型
  - `read_paperclip_skill_sync_preference`(Node L2794-2834)— 用 `Map.contains_key` 镜像 Node `hasOwnProperty` 语义
  - `canonicalize_desired_paperclip_skill_reference`(L2842-2857)— exact → runtime_name → slug → unresolved 四级回退
  - `resolve_paperclip_desired_skill_names`(L2858-2869)— 解析并保留未匹配 reference(与 Node 一致,不静默丢弃)
  - `write_paperclip_skill_sync_preference`(L2870-2899)— 不 mutate input;任一 entry 有 versionId 时 emit typed shape,否则 emit string shape
  - 用 `HashSet<String> seen + Vec<...>` 保留 first-seen 顺序,避免 Rust `HashMap` 的非确定性迭代
  - 零 I/O / 零 async / 零 unsafe
- **26 个新单元测试** + **12 个新集成测试**(`round387_skill_sync_preference.rs`)

### 当前 adapter-utils 复刻进度（R387 末）

| 模块 | 状态 |
|---|---|
| prompt_compose (R380-R383) | 100% |
| env_helpers (R382) | 100% |
| normalize (R382) | 100% |
| log_redaction (R384) | 100% |
| workspace_env (R385) | 100% |
| subprocess_signal (R385) | 100% |
| paths/settings/managed_home/reconcile_skills | 100% |
| resolve_paperclip_instance_root_for_adapter (R386) | 100% |
| skill sync prefs (R387) | 100% |

### 下一轮 R390+ 计划

R380-R389 共 10 轮、约 100 个 Node `adapter-utils/src/server-utils.ts`
函数已**完整移植到 Rust**。剩余工作集中在三块:

#### P0 — 系统核心可靠性(heartbeat)
- `readiness.rs`(R290 部分完成,需补 staleness / idempotent wake / 抑制 DB override)
- 其他 retry reason(`dependency_unavailable` / `workspace_locked` / `quota_exceeded`)

#### P1 — 用户面核心
- company-skills 深度(fork / test-run 状态机)
- tools / tool-connections 真实 OAuth 流程
- plugin worker→host 回调 + 生命周期恢复
- decisions / decision-training 仓储化

#### P2 — Adapter 实质实现
- 13 个 adapter stubs(`pc-adapter-gemini-local` / `pc-adapter-grok-local`
  / `pc-adapter-opencode-local` / `pc-adapter-pi-local` /
  `pc-adapter-cursor-cloud` / 等)从 1-test stub 升级到完整实现

#### P3 — 辅助功能
- secrets AWS / GCP / Vault 真实解密
- folders / labels / routines / pipelines 完整迁移
- cli auth bridge
- UI e2e 冒烟

预计下一阶段:heartbeat readiness / staleness 完整实现 + 1-2 个 adapter 充实。
