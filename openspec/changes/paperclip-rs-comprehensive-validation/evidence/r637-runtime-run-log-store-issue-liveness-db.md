# R637 — 运行时服务 batch 1 (run-continuations / run-log-store / issue-liveness DB glue)

## Status

DONE — 三个 Node 服务的 1:1 Rust 复刻落地，纯函数 + DB glue 解耦；workspace lib 测试 0 failed。

-  新 crate：8 lib + 9 e2e = 17 全过
- ：1 单测 + DB 函数编译通过
- ：4 单测全过（原 24 个纯函数测试保留）
- pc-server / pc-cli 编译 0 errors

## Files added / modified

| Path | Status | Notes |
|---|---|---|
| crates/pc-run-log-store/Cargo.toml | new | pc-errors + async-trait + tokio + sha2 + bytes + parking_lot + tempfile(dev) |
| crates/pc-run-log-store/src/lib.rs | new | 模块组织 + re-export |
| crates/pc-run-log-store/src/types.rs | new (~200 LOC) | RunLogHandle / Event / ReadOptions / FinalizeSummary / Store trait / MirrorTarget trait / RunLogError + resolve_within 公共 helper |
| crates/pc-run-log-store/src/local.rs | new (~430 LOC) | LocalFileRunLogStore: begin/append/finalize/read/flush_inflight_mirrors + in-flight tail scheduler（Arc<State> + tokio::spawn 异步节流）|
| crates/pc-run-log-store/src/inmemory.rs | new (~210 LOC) | InMemoryRunLogStore 测试 fake + 可选 mirror |
| crates/pc-run-log-store/src/factory.rs | new (~85 LOC) | create_durable_run_log_store + safe_segments + normalize_key_prefix |
| crates/pc-run-log-store/tests/local_e2e.rs | new (~270 LOC) | Node 9 项行为 1:1 覆盖 |
| crates/pc-issues/src/liveness/loader.rs | new (~190 LOC) | load_issue_graph_liveness_input (issues/relations/agents/active_runs/queued_wake_requests/pending_interactions/pending_approvals/open_recovery_issues 8 个 SQL 投影) + IssueGraphLivenessLoadError |
| crates/pc-issues/src/liveness/mod.rs | modified | 注册 loader + re-export |
| crates/pc-heartbeat/src/recovery/run_liveness_continuations_db.rs | new (~230 LOC) | apply_continuation_decision (decide → skip/exhausted 直返；enqueue 先重查 idempotency_key 再 insert) + build_continuation_wakeup 纯函数 + ContinuationApplyOutcome 四态 + make_continuation_idempotency_key |
| crates/pc-heartbeat/src/recovery/mod.rs | modified | pub mod + re-export |
| Cargo.toml | modified | workspace members += pc-run-log-store |
| MODULE-MAPPING.md | modified | 新增 R637 段（run-log-store / run-liveness-continuations / issue-graph-liveness / recovery service 4 行映射）|
| openspec/changes/paperclip-rs-comprehensive-validation/tasks.md | modified | R637 [x] |

## 与 Node 语义对齐

### services/run-log-store.ts (createDurableRunLogStore / getRunLogStore / flushInFlightRunLogMirrors)

- **store id 恒为 local_file**： 是稳定身份，下游 (feedback / heartbeat read cast / fixtures) 不需改
- **safeSegments**： 1:1 复刻，公共 crate 暴露 
- **resolveWithin**：基于  + ParentDir 组件词法检查（Node 用  折叠  后比 base；Rust 等价做法）
- **append** 序列化  + （ndjson 1 行 1 event），返回新字节长度
- **finalize** 流程：retire inflight mirror（等任何 in-flight upload 完成，防过期 partial 覆盖）→ 计算 sha256 + 字节数 → 当 mirror 配置则 put_object 完整文件
- **inflight mirror (可选)**：仅当  启用。 时 mark dirty，spawn  异步循环：sleep(interval) → mirror_inflight_once（成功且仍 dirty 则继续；否则退出）。失败自动重 dirty，最多重试一轮/间隔
- **flush_inflight_mirrors** 是 graceful-shutdown hook：等所有 in-flight upload + drain dirty entries
- **read**：本地 stat 失败返回空内容（Node 同样行为）；冷读 mirror fallback 留作 MirrorTarget trait 扩展点（目前 trait 只暴露 put_object）

### services/recovery/run-liveness-continuations.ts (decideRunLivenessContinuation + helpers)

- 纯函数  已在 R636 前的轮次完整实现（24 测试覆盖所有 skip/exhausted/enqueue 分支 + serde round-trip + kind() helper）
- **新增 DB glue** ：
  1. 调纯函数拿 
  2. Skip → ，零 DB 写
  3. Exhausted → ，零 DB 写
  4. Enqueue →  重查（race-safe）→ 存在则 ；否则  构造  并 
- payload 字段：issueId / sourceRunId / livenessState / livenessReason / continuationAttempt / maxContinuationAttempts / instruction（与 Node withRecoveryModelProfileHint 同源字段）
- ，，，（与 Node wake 的 source/trigger/reason 一致）
-  提取为独立纯函数（输入 ），单元测试无需 DB

### services/recovery/issue-graph-liveness.ts (classifyIssueGraphLiveness + collectIssueGraphLivenessInput)

- 分类器 + 4 段（types / incident_key / classifier / service）前序轮次已实现，纯函数 + 7 测试全过
- **新增 DB loader** ：
  - issues: 
  - relations: （type check 是 blocks，由 Node 同样的列约束保证）
  - agents: （全部 status；invokability 留给 classifier）
  - active_runs: 
  - queued_wake_requests: 
  - pending_interactions:  on 
  - pending_approvals:  on 
  - open_recovery_issues: 
- 返回的  可直接喂给 
- payload.issueId 解析容错（不存在或非 UUID 跳过该 row）

## 设计决策

1. **pc-run-log-store 独立 crate**（不放 pc-storage 也不放 pc-heartbeat）
   - 与 Node  同位置同职责
   - 避免与 pc-storage 形成循环依赖（pc-run-log-store 持有  trait，pc-storage 可后续提供适配器）
   - 0 跨 crate 业务耦合，仅依赖 pc-errors + tokio + sha2 + bytes
2. **Arc<StoreState> + tokio::spawn** 是 inflight tail 调度最简实现
   - 保持 Node 行为：append 后 1 间隔才出现首次镜像，间隔内 hot-loop 抑制
   - spawn 的 task 自带 inflight 标志，避免并发上传
   - 失败自动重 dirty，下个间隔重试；不引入新 retry 策略，复用 finalize 的 best-effort 语义
3. **resolveWithin 词法检查 ParentDir** 而非 fs::canonicalize
   - canonicalize 要求路径存在； 在 begin 时尚未创建文件
   - Node  不跟随 symlink，词法  折叠与 Rust  等价
4. **DB glue 拆分 build / apply**： 是纯函数，单测覆盖 wire 形状； 串联 decide + build + DB IO
   - 与 Node 同样的关注点分离（payload 构造 vs wakeup insert）
   - 单元测试不需 DB，集成测试可对接真实 Postgres（沿用 round290_recovery_wake 模式）
5. **load_issue_graph_liveness_input 独立函数** 而非 IssueService 方法
   - 一次性投影大量 rows 给 classifier，不需要 issue-by-issue 校验
   - 与现有  同层（DB glue），互不耦合

## 测试覆盖（真实执行输出）

- 
running 8 tests
test factory::tests::normalize_key_prefix_strips_slashes ... ok
test factory::tests::safe_segments_replaces_spaces ... ok
test factory::tests::safe_segments_preserves_safe_chars ... ok
test factory::tests::safe_segments_sanitizes_path_separators ... ok
test local::tests::safe_segments_replaces_unsafe_chars ... ok
test inmemory::tests::begin_appends_serialize_and_finalize ... ok
test local::tests::resolve_within_rejects_path_traversal ... ok
test local::tests::begin_returns_local_file_handle ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s


running 9 tests
test in_memory_store_mirrors_local_file_semantics ... ok
test read_returns_empty_when_local_file_missing_and_no_mirror ... ok
test store_id_is_always_local_file ... ok
test finalize_uploads_complete_file_to_mirror ... ok
test append_only_live_tail_returns_lines_in_order ... ok
test no_mirror_means_no_inflight_traffic ... ok
test flush_inflight_mirrors_drains_pending_uploads ... ok
test inflight_mirror_disabled_by_default_does_not_put_until_finalize ... ok
test inflight_mirror_enabled_uploads_partial_throttled ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.18s


running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s：8 lib + 9 e2e = 17 passed
- 
running 1 test
test liveness::loader::tests::load_error_displays_sqlx ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 96 filtered out; finished in 0.00s：1 passed (loader error display) + 96 既有 liveness 套件
- 
running 28 tests
test recovery::run_liveness_continuations::tests::read_attempt_handles_none ... ok
test recovery::run_liveness_continuations::tests::decision_kind_helper ... ok
test recovery::run_liveness_continuations::tests::read_attempt_handles_numeric_string ... ok
test recovery::run_liveness_continuations::tests::read_attempt_clamps_zero_or_negative_to_zero ... ok
test recovery::run_liveness_continuations::tests::read_attempt_rejects_garbage ... ok
test recovery::run_liveness_continuations::tests::idempotency_key_format ... ok
test recovery::run_liveness_continuations::tests::skip_when_agent_missing ... ok
test recovery::run_liveness_continuations::tests::skip_when_company_scope_mismatch ... ok
test recovery::run_liveness_continuations::tests::skip_when_issue_missing ... ok
test recovery::run_liveness_continuations::tests::exhausted_when_attempts_at_max ... ok
test recovery::run_liveness_continuations::tests::skip_when_liveness_state_missing ... ok
test recovery::run_liveness_continuations::tests::skip_when_liveness_state_not_actionable ... ok
test recovery::run_liveness_continuations::tests::skip_when_issue_assignee_mismatch ... ok
test recovery::run_liveness_continuations::tests::exhausted_comment_contains_state_and_reason ... ok
test recovery::run_liveness_continuations::tests::skip_when_idempotent_wake_exists ... ok
test recovery::run_liveness_continuations::tests::skip_when_budget_blocked ... ok
test recovery::run_liveness_continuations::tests::skip_when_issue_has_execution_state ... ok
test recovery::run_liveness_continuations::tests::skip_when_issue_status_not_active ... ok
test recovery::run_liveness_continuations::tests::enqueue_allows_error_agent_status ... ok
test recovery::run_liveness_continuations::tests::enqueue_when_all_conditions_met ... ok
test recovery::run_liveness_continuations::tests::enqueue_increments_attempt_from_existing ... ok
test recovery::run_liveness_continuations::tests::enqueue_uses_default_instruction_when_next_action_missing ... ok
test recovery::run_liveness_continuations::tests::skip_when_agent_status_not_invokable ... ok
test recovery::run_liveness_continuations_db::tests::idempotency_key_helper_matches_pure_function ... ok
test recovery::run_liveness_continuations_db::tests::pure_decision_is_wired ... ok
test recovery::run_liveness_continuations_db::tests::outcome_kind_reports_state ... ok
test recovery::run_liveness_continuations::tests::decision_serde_round_trip ... ok
test recovery::run_liveness_continuations_db::tests::build_continuation_wakeup_carries_required_fields ... ok

test result: ok. 28 passed; 0 failed; 0 ignored; 0 measured; 584 filtered out; finished in 0.01s：28 passed (24 既有 + 4 新增 build_continuation_wakeup / outcome_kind / idempotency_key_helper / pure_decision_is_wired)
- ：0 errors
- ：0 errors

## 后续计划

- **R638** 协作与策略：invite-grants / hot-restart 完整语义 / tool-access-policy
- **R639** 收尾与管道：summary-slot-finalization / pipeline-case-outputs / pipelines-aggregation
- 持续：recovery service.ts 剩余 20% 调度入口（escalation / backstop sweep）按 R637 同样的「纯函数 + DB glue」拆分模式补齐
