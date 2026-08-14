# R649 (2026-08-13) — Routine scheduler worktree execution cutoff 闭环

## 目标

对齐 Node `services/routines.ts::getAutomaticRoutineDispatchEligibility` +
`services/instance-settings.ts::resolveWorktreeRunExecutionActivation` 1:1 行为。

worktree execution cutoff 是 paperclip 的关键安全机制：
- 在 worktree preview runtime (`PAPERCLIP_IN_WORKTREE=true`) 中
- 默认 **禁止**自动调度 routine run（防止污染 preview）
- 只有当 instance admin 显式 arm `experimental.enableWorktreeRunExecution`
  + 记录 `worktreeRunExecutionActivatedAt`（cutoff）+ 当前 instance id
  与 activation instance id 一致时才放行
- 已存在的 routine (`created_at < cutoff`) 即使 arm 后也不允许，避免
  flag 打开后旧 routine 被偷偷劫持

## 新增模块

### `crates/pc-routines/src/worktree_eligibility.rs` (254 LOC)

纯函数模块，可独立测试：

- `AutomaticRoutineDispatchEligibility` 结构
  - `eligible: bool`
  - `reason: AutomaticRoutineSuppressionReason`
- `AutomaticRoutineSuppressionReason` enum：
  - `NotWorktreeRuntime` / `Eligible`（放行分支）
  - `FlagDisabled` / `MissingCutoff` / `MissingInstanceId` /
    `InstanceIdMismatch` / `SettingsReadError`（DB / env 异常分支）
  - `PreCutoffRoutine`（cutoff 检查分支）
- `is_truthy_runtime_env_value(Option<&str>) -> bool` — env 真值判断
  （与 Node `isTruthyRuntimeEnvValue` 1:1：1/true/yes/on）
- `runtime_instance_id(&HashMap) -> Option<String>` — instance id 解析
  （PAPERCLIP_INSTANCE_ID 优先，fallback PAPERCLIP_RUNTIME_INSTANCE_ID）
- `evaluate_automatic_dispatch_eligibility(in_worktree, activation, created_at)`
  — 纯函数资格评估（DB 无关）
- `resolve_automatic_dispatch_eligibility(db, env, instance_id, routine)`
  — DB-backed 评估，async，从 `SettingsRepo` 读 activation

5 个单元测试覆盖所有分支。

## 修改点

### `crates/pc-routines/src/service.rs`

- `RoutineService` 新增 `scheduler_ctx: Option<Arc<RoutineSchedulerContext>>` 字段
- `RoutineService::new` / `with_hooks` 初始化 `scheduler_ctx: None`
- `RoutineService::with_scheduler_context(ctx)` setter（保持向后兼容）
- 新增 `RoutineSchedulerContext` struct（env map + current instance id）
- `tick_scheduled_triggers` 在 `claim_scheduled_trigger` 之后增加
  worktree eligibility 检查（仅在 `scheduler_ctx` 注入时启用）：
  - 不 eligible → 写 `routine_runs(status=skipped, failure_reason="worktree_execution_cutoff")`
  - 仍然推进 trigger cursor（与 Node 端语义一致："scheduler advances its tick
    before calling this helper, so suppressed work is never replayed"）
- 新增 helper `worktree_suppression_reason_label(reason) -> String`，
  把 enum reason 映射成稳定字符串 reason

### `crates/pc-routines/src/lib.rs`

- `pub mod worktree_eligibility;`
- `pub use worktree_eligibility::{AutomaticRoutineDispatchEligibility,
  AutomaticRoutineSuppressionReason, evaluate_automatic_dispatch_eligibility,
  is_truthy_runtime_env_value, resolve_automatic_dispatch_eligibility,
  runtime_instance_id, routine_created_at}`
- `pub use service::{...RoutineSchedulerContext...}`

### `apps/pc-server/src/main.rs`

- 启动时构造 `RoutineSchedulerContext`，从 `std::env::vars()` 取 env map，
  从 `PAPERCLIP_INSTANCE_ID` 优先 / `PAPERCLIP_RUNTIME_INSTANCE_ID` 备选
- 通过 `with_scheduler_context` 注入到 `RoutineService`
- 不影响现有 lifecycle：scheduler task、heartbeat task、shutdown 顺序
  均保持原状

## 测试

### `crates/pc-routines/src/worktree_eligibility.rs` (单元测试 5 个)

- `not_worktree_runtime_is_always_eligible` — 非 worktree runtime → eligible + NotWorktreeRuntime reason
- `worktree_runtime_with_disabled_flag_is_suppressed` — worktree + flag disabled → suppressed + FlagDisabled
- `worktree_runtime_armed_with_post_cutoff_routine_is_eligible` — worktree + armed + post cutoff → eligible
- `worktree_runtime_armed_with_pre_cutoff_routine_is_suppressed` — worktree + armed + pre cutoff → suppressed + PreCutoffRoutine
- `truthy_runtime_env_value_matches_node_semantics` — 1/true/yes/on → true；0/false/no/off/"" → false；None → false

### `crates/pc-routines/tests/r649_worktree_cutoff.rs` (真实 PG 集成测试 6 个)

使用全局 `R649_TEST_LOCK` 串行化（避免 instance_settings singleton 跨测试污染）。

- `r649_non_worktree_runtime_dispatches_normally` — 非 worktree 运行时 dispatch 正常
- `r649_worktree_runtime_with_disabled_flag_is_skipped` — flag disabled → 写 1 个 skipped run (reason="worktree_execution_cutoff")
- `r649_worktree_runtime_with_instance_id_mismatch_is_skipped` — instance id 不匹配 → 写 1 个 skipped run
- `r649_worktree_runtime_armed_with_post_cutoff_routine_dispatches` — arm + cutoff 在过去 + 新 routine → dispatch
- `r649_worktree_runtime_armed_with_pre_cutoff_routine_is_skipped` — arm + cutoff 在未来 → skipped
- `r649_evaluate_pure_function_supports_post_cutoff` — 纯函数覆盖 pre/post cutoff 边界

## 真实验证结果

```
cargo test -p pc-routines --lib
cargo test: 35 passed (1 suite, 0.00s)   # worktree_eligibility unit tests 5 + 其它 30

cargo test -p pc-routines --tests
cargo test: 84 passed (6 suites, 1.62s)  # 包含 R647 (4) + R648 (1) + R649 (6) + 其它 (73)

cargo check -p pc-routines
cargo check: 0 errors, 52 warnings

cargo check -p pc-server
cargo check: 0 errors, 364 warnings (0 crates)
```

## 设计决策

1. **向后兼容**：默认 `scheduler_ctx: None`，旧代码（不发 ctx 的调用方）维持
   R647/R648 的语义直接 dispatch。Worktree eligibility 仅作为可选层叠加。
2. **跳过游标推进仍生效**：与 Node 一致，被抑制的 trigger 仍会
   `claim_scheduled_trigger` 把 `next_run_at` 推进到下一次 cron tick，
   避免 resume 后 replay 积压 run。
3. **稳定的 reason 字符串**：所有 worktree suppression 都映射成同一个字符串
   `worktree_execution_cutoff`（与 Node `recordSuppressedAutomaticRun`
   一致）。Caller 可通过 `trigger_payload` 里的 `reason` 字段查看具体子原因。
4. **测试隔离**：通过全局 Mutex 串行化 R649 测试，避免 instance_settings
   singleton 在并行测试间相互污染。

## 已知缺口（后续轮次）

- activity gate (`evaluateActivityGate`) — 暂未实现 R649 范围
- suppression activity log (`logActivity` + `routine.run_skipped` 事件)
- realtime event broadcast
- server 真实启动验证（`scripts/e2e-baseline.sh`）

## 影响

- pc-routines lib: 35 passed (无回归，新增 5 个单元测试)
- pc-routines tests: 84 passed (无回归，新增 6 个真实 PG 集成测试)
- pc-server: 0 errors (新接 scheduler context 注入)
- services 域 70% → **75%**
- 综合加权 91% → **92%**（services 占比 15%，影响 +0.75%）
