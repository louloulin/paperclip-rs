# Round 337 Gap 分析 — `scan_silent_active_runs` 主循环接通 full 版本

## 📊 进度快照（截至 Round 337）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | 290→337（48 个模块） |
| 最近一轮 | **Round 337**：`scan_silent_active_runs` 主循环接通 full 版 |
| Round 337 测试 | **7/7 全部通过真实 PostgreSQL** |
| pc-heartbeat 测试文件 | **47 个集成测试文件** |
| pc-heartbeat lib 测试 | **475 passed**（up from 474） |
| 总测试数 | **824 passed**（up from 808） |
| pc-server --bins | **编译通过**（24.13s） |

## 📈 完成度趋势

```
Round 257: ~81%   →   Round 290: ~83%   →   Round 319: ~85.0%
Round 320: ~85.3% →   Round 322: ~85.8% →   Round 325: ~86.7%
Round 326: ~87.5% →   Round 329: ~89%   →   Round 332: ~90.5%
Round 333: ~92%   →   Round 334: ~93.5% →   Round 335: ~94.5%
Round 336: ~96%   →   Round 337: ~96.5% ✨
```

## 🔧 Round 337 关键决策

### 新增文件
- `crates/pc-heartbeat/src/recovery/resolve_stale_run_owner_agent.rs`（~210 行）
- `crates/pc-heartbeat/tests/round337_scan_silent_active_runs_full.rs`（~440 行，7 测试）

### 修改文件
- `crates/pc-heartbeat/src/recovery/mod.rs`：注册 `resolve_stale_run_owner_agent` 模块 + re-export
- `crates/pc-heartbeat/src/recovery/scan_silent_active_runs_db.rs`：
  - 主循环改为调用 `create_or_update_stale_run_evaluation_full` 而非 minimal 版
  - 新增 `fetch_running_agent_view()` / `fetch_source_issue_view_for_run()` helpers
  - 新增 `RunningAgentView` / `StaleRunSourceIssueInfo` 内部 view structs

### 关键实现要点

**主循环新行为（Round 337）**：
1. SELECT silent candidates（不变）
2. snooze 检查（不变）
3. **新增**: fetch running_agent view（含 reports_to）
4. **新增**: fetch source_issue view（含 assignee_agent_id）
5. **新增**: resolve_stale_run_owner_agent_id（Node 第 1828 行对齐）
6. **切换**: 调用 `create_or_update_stale_run_evaluation_full` 而非 minimal

**`resolve_stale_run_owner_agent_id`（对齐 Node 第 1808 行）**：
- 候选顺序：
  1. `sourceIssue.assigneeAgentId.reportsTo`（若有）
  2. `runningAgent.reportsTo`
  3. role=cto
  4. role=ceo
- 取第一个 invokable + 同 company 的 agent
- 全部不可 invoke → None
- budget 模块未完全迁移，暂 stub 返回 false（无 block）

**`fetch_running_agent_view`**：
- SELECT id/company_id/name/reports_to/status/adapter_type FROM agents WHERE id = $1
- 校验 company_id == run_company_id（与 Node `getAgent` + company 检查对齐）
- 返回 None 当 agent 不存在或 company 不匹配

**`fetch_source_issue_view_for_run`**：
- 从 run.context_snapshot 提取 issueId（复用已有 `extract_issue_id_from_context`）
- SELECT id/company_id/status/identifier/assignee_agent_id FROM issues WHERE id AND company_id AND hidden_at IS NULL
- 返回 `(Option<StaleSourceIssueView>, Option<StaleRunSourceIssueInfo>)`

### 重要设计决策

1. **`SourceIssueView` 保持不变**：避免破坏 R335 / R336 的现有测试构造。owner 解析需要的 `assignee_agent_id` 通过新建 `StaleRunSourceIssueInfo` 内部 struct 携带（view + assignee_agent_id 拆分）。
2. **minimal 版 `create_or_update_stale_run_evaluation` 保留**：仍 exported，可被其他 module 直接复用 simple json description 路径
3. **`RunningAgentView` / `StaleRunSourceIssueInfo` 内部不导出**：仅 scan_silent_active_runs_db 模块内部使用
4. **保留 `fetch_running_agent_view` company 检查**：防止 run.company_id 与 agent.company_id 不一致时的脏数据
5. **保留 snooze 检查在 full 之前**：snooze 优先级最高（避免无效 full 评估）

### 测试覆盖（Round 337）

| 测试 | 场景 | 关键断言 |
|---|---|---|
| `scan_with_critical_owner_and_source_creates_full` | critical + owner + source | Created + high + source comment + wake |
| `scan_critical_no_source_no_owner_creates_only` | critical + 无 owner | Created + high + 无 wake |
| `scan_dismissed_false_positive_skips` | dismissed_false_positive | Skipped |
| `scan_snoozed_skips` | active snooze | snoozed++ |
| `scan_existing_critical_escalates` | 已有 evaluation + critical | Escalated + high + source comment |
| `scan_suspicious_no_owner_creates_minimal` | suspicious level | Created + medium |
| `scan_no_candidates_returns_empty` | 无 candidate | 空 result |

## 🗂️ 关键文件速查

- `crates/pc-heartbeat/src/recovery/scan_silent_active_runs_db.rs` —— **本轮修改** 主循环接通 full
- `crates/pc-heartbeat/src/recovery/create_or_update_stale_run_evaluation_full.rs` —— R336
- `crates/pc-heartbeat/src/recovery/resolve_stale_run_owner_agent.rs` —— **本轮新增**
- `crates/pc-heartbeat/tests/round337_scan_silent_active_runs_full.rs` —— **本轮新增** 7 测试

## ⚠️ 关键陷阱与约定（必须保持）

### 工程约束
1. **TDD 严格**：先写失败测试 → 看红 → 实现 → 看绿 ✓ Round 337 严格遵守
2. **真实 PostgreSQL 验证**：每次模块完成必须跑 ✓ Round 337 7/7 通过
3. **不重命名已有文件**、**不修无关 bug**、**不 git commit** ✓
4. **中文汇报**每次 ✓
5. **高内聚低耦合**：pure 函数无副作用；DB 模块仅做 I/O ✓

### 新增约定
- **`SourceIssueView` 不可变**（被 R335 / R336 锁定）：新增 source_issue 字段请用新内部 struct 包装
- **owner 解析必须先 fetch running_agent**：避免 reports_to 为 None 时误用默认逻辑
- **snooze 优先级最高**：在 fetch agent 之前就 short-circuit

## 🔍 Round 337 后剩余 Gap 分析

### 高 ROI（推荐 Round 338+ 优先）
1. **`is_recovery_origin_issue` 递归短路**（Node 第 2073 行）：当 source_issue 本身是 recovery issue（origin_kind in [...]）→ 写 `output_stale_recovery_recursion_refused` activity log + Skipped
2. **`is_terminal_issue_status` + `fold_source_resolved_stale_run` 接入主循环**（Node 第 2077 行）：source_issue status=done/cancelled 时触发 fold 流程（auto-close evaluation）
3. **`findClosedStaleRunEvaluation` + auto-dismiss**（Node 第 2103 行附近）：现有 evaluation 已 done 但无 watchdog decision → 自动记录 `dismissed_false_positive`
4. **advisory lock 并发优化**（Node 第 2118 行）：auto-dismiss 用 `pg_advisory_xact_lock` 序列化
5. **`buildExecutionReviewParticipantRecoveryComment` 接入 escalate_db**（旧 R330 只实现 pure 函数）

### 中 ROI
6. **`collect_stale_run_evidence` 完整版**：safe_tail + recent_events + childIssues + blockers（description builder 已支持，data collector 待实现）
7. **`latestSameRunSourceTerminalEvidence`**：terminal source issue 的 evidence 收集
8. **`format_duration` 共享**：description / comment / activity log 共用

### 低 ROI
9. **HeartbeatRunActor 注入 Db**：kameo actor → recovery lib
10. **HeartbeatRunContextView struct**：从 HeartbeatRow 投影（减少 view struct 数量）
11. **UI routes (pc-http) 覆盖率补全**：recovery UI 端点

## 📋 Round 338 候选优先级

**首要（推荐 Round 338）**：
- **`is_recovery_origin_issue` + `output_stale_recovery_recursion_refused` activity log**（Node 第 2073 行）
- 估计代码量：~100 行（1 模块 + 2-3 测试）
- 收益：避免 source_issue 是 recovery issue 时自我递归

**次要（Round 339）**：
- **`fold_source_resolved_stale_run` 主循环接入**（Node 第 2077 行）
- 估计代码量：~150 行
- 收益：source_issue terminal 时自动 fold（不创建新 evaluation）

**第三（Round 340）**：
- **`findClosedStaleRunEvaluation` + auto-dismiss**（Node 第 2103 行）
- 估计代码量：~200 行（含 advisory lock）
- 收益：现有 evaluation 关闭时自动 dismiss，避免 re-fire

## 📊 完成度更新

| 模块 | 之前 | 当前 |
|---|---|---|
| `resolve_recovery_owner_agent` (stranded) | ✓ R315 | ✓ |
| `resolve_stale_run_owner_agent` (stale run) | ❌ | ✓ R337 |
| `scan_silent_active_runs` (minimal) | ✓ R290 | ✓ |
| `scan_silent_active_runs` (full path) | ❌ | ✓ R337 |
| `create_or_update_stale_run_evaluation` (minimal) | ✓ R290 | ✓ |
| `create_or_update_stale_run_evaluation` (full) | ❌ | ✓ R336 |
| `is_recovery_origin_issue` recursion check | ❌ | ❌ |
| `fold_source_resolved_stale_run` 主循环 | ❌ | ❌ |
| `findClosedStaleRunEvaluation` auto-dismiss | ❌ | ❌ |

**Recovery 子系统总进度：~93%**（up from ~91%）

## 🎯 下一轮目标

**Round 338**：
- 实现 `is_recovery_origin_issue`（pure 函数 + DB check）
- 在 `create_or_update_stale_run_evaluation_full` 入口检查
- 若 source_issue 是 recovery issue → 写 `output_stale_recovery_recursion_refused` activity log + Skipped
- 测试 5-7 个 case（real PostgreSQL 验证）

**预估收益**：完成度 ~96.5% → ~97%

---

# Round 338 — `is_recovery_origin_issue` 递归短路

## 📊 进度快照（截至 Round 338）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | 290→338（49 个模块） |
| 最近一轮 | **Round 338**：`is_recovery_origin_issue` 递归短路 + activity log |
| Round 338 测试 | **7/7 全部通过真实 PostgreSQL** |
| pc-heartbeat 测试文件 | **48 个集成测试文件** |
| pc-heartbeat lib 测试 | **478 passed**（up from 475） |
| 总测试数 | **834 passed**（up from 824） |
| pc-server --bins | **编译通过**（21.70s） |

## 📈 完成度趋势

```
Round 336: ~96%   →   Round 337: ~96.5%   →   Round 338: ~97% ✨
```

## 🔧 Round 338 关键决策

### 新增文件
- `crates/pc-heartbeat/src/recovery/is_recovery_origin_issue.rs`（~150 行）
- `crates/pc-heartbeat/tests/round338_is_recovery_origin_issue.rs`（~390 行，7 测试）

### 修改文件
- `crates/pc-heartbeat/src/recovery/mod.rs`：注册 `is_recovery_origin_issue` 模块 + re-export
- `crates/pc-heartbeat/src/recovery/create_or_update_stale_run_evaluation_full.rs`：
  - 增加 `source_issue_origin_kind: Option<String>` 字段到 `CreateOrUpdateStaleRunEvaluationInput`
  - 入口处增加 `is_recovery_origin_issue_str` 检查（Node 第 2073 行对齐）
- `crates/pc-heartbeat/src/recovery/scan_silent_active_runs_db.rs`：
  - `fetch_source_issue_view_for_run` SQL 增加 `origin_kind` 字段
  - `StaleRunSourceIssueInfo` struct 增加 `origin_kind: String` 字段
  - 主循环传递 `source_issue_origin_kind` 到 full 版
- `crates/pc-heartbeat/tests/round336_create_or_update_stale_run_evaluation_full.rs`：6 处测试构造补充 `source_issue_origin_kind: None`

### 关键实现要点

**`is_recovery_origin_issue_str`** (Node 第 1329 行对齐)：
- 接收 `origin_kind: &str`
- 返回 `true` 当 `origin_kind ∈ RECOVERY_ORIGIN_KINDS`
- RECOVERY_ORIGIN_KINDS = `[harness_liveness_escalation, issue_productivity_review, stranded_issue_recovery, stale_active_run_evaluation]`（Node `recovery/origins.ts:1` 完全对齐）

**`log_recovery_recursion_refused_activity`** (Node 第 2058-2075 行对齐)：
- 写 `heartbeat.output_stale_recovery_recursion_refused` activity_log 行
- 字段：company_id / actor_type=system / actor_id=system / agent_id / run_id / entity_type=heartbeat_run / entity_id=run_id
- details：source / sourceIssueId / sourceIssueIdentifier / sourceIssueOriginKind / existingEvaluationIssueId

**主流程新行为**：
1. fetch source_issue 时同时取 origin_kind
2. 调用 full 版时传入 `source_issue_origin_kind`
3. full 版入口检查：
   - source_issue 是 recovery issue → 写 activity log + Skipped（**先于** dismissed_false_positive 检查）
   - 否则继续原有流程

### 测试覆盖（Round 338）

| 测试 | 场景 | 关键断言 |
|---|---|---|
| `is_recovery_origin_issue_str_matches_all_kinds` | pure unit | 4 个 recovery origin_kind 都被识别 |
| `is_recovery_origin_issue_str_rejects_non_recovery` | pure unit | 普通 origin_kind 被拒绝 |
| `recovery_origin_kinds_contains_expected_values` | pure unit | 4 个常量值正确 |
| `stranded_recovery_source_refuses_recursion` | source 是 stranded_issue_recovery | Skipped + activity log + 不创建 eval |
| `stale_eval_source_refuses_recursion` | source 是 stale_active_run_evaluation | Skipped + activity log + 不创建 eval |
| `normal_source_continues_normal_flow` | source 是 todo | Created + 无 recursion_refused activity |
| `recursion_check_takes_priority_over_dismissed` | source 是 recovery + dismissed | Skipped（recursion 优先） + recursion activity 写入 |

## 🔍 Round 338 后剩余 Gap 分析

### 高 ROI（推荐 Round 339+）
1. **`is_terminal_issue_status` + `fold_source_resolved_stale_run` 接入主循环**（Node 第 2077 行）：source_issue status=done/cancelled 时自动 fold 现有 evaluation issue
2. **`findClosedStaleRunEvaluation` + auto-dismiss**（Node 第 2103 行）：现有 evaluation 已 done 但无 watchdog decision → 自动记录 dismissed_false_positive
3. **`latestSameRunSourceTerminalEvidence`**：terminal source issue 的 evidence 收集（fold 前的条件检查）
4. **advisory lock 并发优化**（Node 第 2118 行）：auto-dismiss 用 `pg_advisory_xact_lock` 序列化

### 中 ROI
5. **`collect_stale_run_evidence` 完整版**：safe_tail + recent_events + childIssues + blockers
6. **`format_duration` 共享 helper**
7. **`buildExecutionReviewParticipantRecoveryComment` 接入 escalate_db**

### 低 ROI
8. **HeartbeatRunActor 注入 Db**
9. **UI routes (pc-http) 覆盖率补全**

## 📋 Round 339 候选优先级

**首要（推荐 Round 339）**：
- **`fold_source_resolved_stale_run` 接入主循环**（Node 第 2077 行）
- 估计代码量：~150 行（1 pure 模块 + DB helper + 测试）
- 收益：source_issue terminal 时自动 fold，避免创建新 evaluation

**次要（Round 340）**：
- **`findClosedStaleRunEvaluation` + auto-dismiss**（Node 第 2103 行）
- 含 advisory lock 并发优化（pc_tx 模块）
- 估计代码量：~200 行
- 收益：现有 evaluation 关闭时自动 dismiss，避免 re-fire

## 📊 完成度更新

| 模块 | 之前 | 当前 |
|---|---|---|
| `resolve_recovery_owner_agent` (stranded) | ✓ R315 | ✓ |
| `resolve_stale_run_owner_agent` (stale run) | ✓ R337 | ✓ |
| `scan_silent_active_runs` 主循环接通 full | ✓ R337 | ✓ |
| `create_or_update_stale_run_evaluation` (minimal) | ✓ R290 | ✓ |
| `create_or_update_stale_run_evaluation` (full) | ✓ R336 | ✓ |
| **`is_recovery_origin_issue` 递归短路** | ❌ | ✓ R338 |
| `is_terminal_issue_status` 检查 | ❌ | ❌ |
| `fold_source_resolved_stale_run` 主循环 | ❌ | ❌ |
| `findClosedStaleRunEvaluation` auto-dismiss | ❌ | ❌ |

**Recovery 子系统总进度：~95%**（up from ~93%）

## 🎯 下一轮目标

**Round 339**：
- 实现 `is_terminal_issue_status` (pure)
- 实现 `latestSameRunSourceTerminalEvidence` (DB query)
- 实现 `fold_source_resolved_stale_run` (Node 第 1665 行对齐) — 完整 fold 流程
- 在 `create_or_update_stale_run_evaluation_full` 入口检查（Node 第 2077 行）

**预估收益**：完成度 ~97% → ~97.5%

---

# Round 339 — `is_terminal_issue_status` + fold path 主循环接入

## 📊 进度快照（截至 Round 339）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | 290→339（50 个模块） |
| 最近一轮 | **Round 339**：fold_source_resolved_stale_run 主循环接入 |
| Round 339 测试 | **8/8 全部通过真实 PostgreSQL** |
| pc-heartbeat 测试文件 | **49 个集成测试文件** |
| pc-heartbeat lib 测试 | **485 passed**（up from 478） |
| 总测试数 | **849 passed**（up from 834） |
| pc-server --bins | **编译通过**（21.71s） |

## 📈 完成度趋势

```
Round 337: ~96.5% →   Round 338: ~97%   →   Round 339: ~97.5% ✨
```

## 🔧 Round 339 关键决策

### 新增文件
- `crates/pc-heartbeat/src/recovery/is_terminal_issue_status.rs`（~50 行）
- `crates/pc-heartbeat/src/recovery/latest_same_run_source_terminal_evidence.rs`（~120 行）
- `crates/pc-heartbeat/tests/round339_is_terminal_and_fold.rs`（~620 行，8 测试）

### 修改文件
- `crates/pc-heartbeat/src/recovery/mod.rs`：注册 2 个新模块 + re-exports
- `crates/pc-heartbeat/src/recovery/create_or_update_stale_run_evaluation_full.rs`：
  - 增加 fold check（在 dismissed 之后、silence/level 计算之前）
  - 增加 `log_source_resolved_fold_activity` helper（Node 第 1797 行对齐）
- `crates/pc-heartbeat/src/recovery/stale_run_auto_dismiss.rs`：
  - `fold_source_resolved_stale_run` 增加"existing evaluation 写 source-resolved comment"（Node 第 1755 行对齐）

### 关键实现要点

**`is_terminal_issue_status_str`** (Node 第 1325 行对齐)：
- pure：`status == "done" || status == "cancelled"`
- 7 个单元测试覆盖 done/cancelled/todo/in_progress/blocked/in_review/empty

**`latest_same_run_source_terminal_evidence`** (Node 第 1522 行对齐)：
- DB query: `SELECT id, created_at, action FROM activity_log WHERE company_id AND run_id AND action='issue.updated' AND entity_type='issue' AND entity_id=$3::text AND details->>'status'=$4 [AND created_at >= $5] ORDER BY created_at DESC LIMIT 1`
- 返回 `LatestSameRunSourceTerminalEvidence { kind, id, created_at, action }` 或 None
- **重要陷阱**：activity_log.entity_id 是 text 列，UUID 需要 `.to_string()` 后传入

**主循环新行为**：
1. is_recovery_origin_issue 检查 (R338)
2. dismissed_false_positive 检查
3. compute silence_started_at
4. **NEW**: if source_issue terminal + has evidence → fold (returns Folded)
5. compute silence/level
6. find existing → handle_existing or handle_create

**`fold_source_resolved_stale_run` 增强**：
- 事务内：update heartbeat_run → status + finished_at + resultJson + update wakeup_request + clear source issue execution
- 事务后：if existing_evaluation → update status='done' + write comment "Source-resolved watchdog fold. ..."
- 事务内：insert heartbeat_run_watchdog_decisions (dismissed_false_positive)
- 新增 helper `log_source_resolved_fold_activity`（Node 第 1797 行对齐）

### 测试覆盖（Round 339）

| 测试 | 场景 | 关键断言 |
|---|---|---|
| `is_terminal_issue_status_str_matches_node_behavior` | pure unit | done/cancelled 是 terminal，其他都不是 |
| `latest_terminal_evidence_returns_recent_activity` | 有 evidence | 返回 Some(evidence) |
| `latest_terminal_evidence_returns_none_when_no_activity` | 无 evidence | None |
| `source_issue_done_with_evidence_folds_run` | done + evidence | Folded + run status='succeeded' + decision + activity |
| `source_issue_cancelled_with_evidence_folds_run` | cancelled + evidence | Folded + run status='cancelled' |
| `source_issue_in_progress_does_not_fold` | 非 terminal | 不 fold → Created |
| `source_issue_done_without_evidence_does_not_fold` | terminal 但无 evidence | 不 fold → Created |
| `fold_closes_existing_evaluation_with_comment` | fold + existing eval | eval status='done' + comment |

## 🔍 Round 339 后剩余 Gap 分析

### 高 ROI（推荐 Round 340+）
1. **`findClosedStaleRunEvaluation` + auto-dismiss**（Node 第 2103 行）：现有 evaluation 已 done 但无 watchdog decision → 自动记录 dismissed_false_positive（含 advisory lock 并发优化）
2. **`isAgentInvokable` 检查**：在 dismissed 之后（Node 第 2109 行附近）—— 我们的 resolve_stale_run_owner_agent_id 已包含
3. **`collectStaleRunEvidence` 完整版**：safe_tail + recent_events + childIssues + blockers
4. **`blocked` source_issue short-circuit**（Node 第 2099 行）：source_issue.status === 'blocked' → Skipped（避免 idle output 误报）
5. **`appendRecoveryRunEvent`** + **`finalizeAgentAfterSourceResolvedRun`**：fold 时写 heartbeat_run_events + 更新 agent.status

### 中 ROI
6. **`cleanupSourceResolvedRunProcess`**：process kill / terminate（仅 SESSIONED_LOCAL_ADAPTERS，本地进程）
7. **`activeRecoveryAction.resolve`**：fold 时清理 active recovery action
8. **`getCurrentUserRedactionOptions`** + **`redactWatchdogEvidenceText`**：description / evidence redaction

### 低 ROI
9. **HeartbeatRunActor 注入 Db**
10. **UI routes (pc-http) 覆盖率补全**

## 📋 Round 340 候选优先级

**首要（推荐 Round 340）**：
- **`findClosedStaleRunEvaluation` + auto-dismiss 主循环接入**（Node 第 2103 行）
- 现有 `auto_dismiss_closed_evaluation` 在 `stale_run_auto_dismiss.rs` 已实现 (R319)
- 估计代码量：~50 行（接入 + 3 测试）
- 收益：现有 evaluation 关闭时自动 dismiss，避免 re-fire

**次要（Round 341）**：
- **`blocked` source_issue short-circuit**（Node 第 2099 行）
- 估计代码量：~30 行 + 2 测试
- 收益：避免 idle output 误报（source 是 blocked 时不创建 evaluation）

## 📊 完成度更新

| 模块 | 之前 | 当前 |
|---|---|---|
| `resolve_recovery_owner_agent` (stranded) | ✓ R315 | ✓ |
| `resolve_stale_run_owner_agent` (stale run) | ✓ R337 | ✓ |
| `scan_silent_active_runs` 主循环接通 full | ✓ R337 | ✓ |
| `create_or_update_stale_run_evaluation` (minimal) | ✓ R290 | ✓ |
| `create_or_update_stale_run_evaluation` (full) | ✓ R336 | ✓ |
| `is_recovery_origin_issue` 递归短路 | ✓ R338 | ✓ |
| **`is_terminal_issue_status`** | ❌ | ✓ R339 |
| **`fold_source_resolved_stale_run` 主循环** | ❌ | ✓ R339 |
| `auto_dismiss_closed_evaluation` 主循环 | ❌ | ❌ (R319 实现但未接入) |
| `blocked` source short-circuit | ❌ | ❌ |

**Recovery 子系统总进度：~97.5%**

## 🎯 下一轮目标

**Round 340**：
- 在 `create_or_update_stale_run_evaluation_full` 接入 `auto_dismiss_closed_evaluation`（在 fold 之后、dismissed 之前）
- 现有 evaluation 已 done 但无 watchdog decision → 自动记录 dismissed_false_positive
- 复用 R319 实现的 advisory lock 并发优化

**预估收益**：完成度 ~97.5% → ~98%

---

# Round 340 — `auto_dismiss_closed_evaluation` 主循环接入

## 📊 进度快照（截至 Round 340）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | 290→340（51 个模块） |
| 最近一轮 | **Round 340**：auto_dismiss 主循环接入（Node 第 2103 行） |
| Round 340 测试 | **5/5 全部通过真实 PostgreSQL** |
| pc-heartbeat 测试文件 | **50 个集成测试文件** |
| pc-heartbeat lib 测试 | **485 passed** |
| 总测试数 | **854 passed**（up from 849） |
| pc-server --bins | **编译通过**（9.76s） |

## 📈 完成度趋势

```
Round 338: ~97%   →   Round 339: ~97.5% →   Round 340: ~98% ✨
```

## 🔧 Round 340 关键决策

### 新增文件
- `crates/pc-heartbeat/tests/round340_auto_dismiss_main_loop.rs`（~330 行，5 测试）

### 修改文件
- `crates/pc-heartbeat/src/recovery/create_or_update_stale_run_evaluation_full.rs`：
  - 增加 auto_dismiss 检查（在 fold 之后、silence/level 计算之前）
  - 复用 R319 实现的 `auto_dismiss_closed_evaluation`（含 advisory lock）

### 关键实现要点

**`auto_dismiss_closed_evaluation`** 已在 R319 实现（含 advisory lock 序列化并发）。Round 340 仅做主循环接入：
- 在 fold check 之后调用
- 如果返回 `Dismissed { decision_id }` → 返回 `Skipped`
- 如果返回 `Skipped` (NoClosedEvaluation / HasExistingDecision) → 继续原流程

**主循环新行为（Round 340）**：
1. is_recovery_origin_issue 检查 (R338)
2. has_dismissed_false_positive_decision 检查
3. compute silence_started_at
4. **R340**: auto_dismiss_closed_evaluation（若有 closed evaluation + 无 watchdog decision → Skipped）
5. compute silence/level
6. find existing → handle_existing or handle_create

### 测试覆盖（Round 340）

| 测试 | 场景 | 关键断言 |
|---|---|---|
| `closed_evaluation_triggers_auto_dismiss_and_skips` | 主路径 | Skipped + dismissed 写入 |
| `existing_snooze_decision_prevents_auto_dismiss` | 已有 decision | 继续正常流程 |
| `auto_dismissed_run_skipped_on_next_cycle` | 下一轮 | 仍 Skipped（hasDismissed 命中） |
| `no_closed_evaluation_skips_auto_dismiss` | 无 closed eval | 正常 Created |
| `concurrent_auto_dismiss_only_one_succeeds` | 并发 | advisory lock 保护，仅 1 个 dismissed |

## 🔍 Round 340 后剩余 Gap 分析

### 高 ROI（推荐 Round 341+）
1. **`blocked` source_issue short-circuit**（Node 第 2099 行）：source_issue.status === 'blocked' → Skipped（避免 idle output 误报）
2. **`appendRecoveryRunEvent` + `finalizeAgentAfterSourceResolvedRun`**：fold 时写 heartbeat_run_events + 更新 agent.status
3. **`collectStaleRunEvidence` 完整版**：safe_tail + recent_events + childIssues + blockers（description builder 已支持，data collector 待实现）

### 中 ROI
4. **`cleanupSourceResolvedRunProcess`**：process kill / terminate（仅 SESSIONED_LOCAL_ADAPTERS，本地进程）
5. **`activeRecoveryAction.resolve`**：fold 时清理 active recovery action
6. **`getCurrentUserRedactionOptions`** + **`redactWatchdogEvidenceText`**：description / evidence redaction

### 低 ROI
7. **HeartbeatRunActor 注入 Db**
8. **UI routes (pc-http) 覆盖率补全**

## 📋 Round 341 候选优先级

**首要（推荐 Round 341）**：
- **`blocked` source_issue short-circuit**（Node 第 2099 行）
- 估计代码量：~30 行 + 2 测试
- 收益：避免 idle output 误报（source 是 blocked 时不创建 evaluation）

**次要（Round 342）**：
- **`appendRecoveryRunEvent` + `finalizeAgentAfterSourceResolvedRun`** 接入 fold path
- 估计代码量：~80 行 + 2 测试
- 收益：fold 时记录 lifecycle event + 同步 agent.status

## 📊 完成度更新

| 模块 | 之前 | 当前 |
|---|---|---|
| `resolve_recovery_owner_agent` (stranded) | ✓ R315 | ✓ |
| `resolve_stale_run_owner_agent` (stale run) | ✓ R337 | ✓ |
| `scan_silent_active_runs` 主循环接通 full | ✓ R337 | ✓ |
| `create_or_update_stale_run_evaluation` (minimal) | ✓ R290 | ✓ |
| `create_or_update_stale_run_evaluation` (full) | ✓ R336 | ✓ |
| `is_recovery_origin_issue` 递归短路 | ✓ R338 | ✓ |
| `is_terminal_issue_status` | ✓ R339 | ✓ |
| `fold_source_resolved_stale_run` 主循环 | ✓ R339 | ✓ |
| **`auto_dismiss_closed_evaluation` 主循环** | ❌ (R319 only impl) | ✓ R340 |
| `blocked` source short-circuit | ❌ | ❌ |

**Recovery 子系统总进度：~98%**

## 🎯 下一轮目标

**Round 341**：
- 实现 `source_issue.status === 'blocked'` 短路（Node 第 2099 行）
- 在 `create_or_update_stale_run_evaluation_full` 入口检查
- blocked 时返回 `Skipped`，不创建 evaluation
- 测试 3 个 case：blocked → Skipped；非 blocked → 正常；与 fold 的交互

**预估收益**：完成度 ~98% → ~98.5%

---

# Round 341 — `blocked` source_issue short-circuit

## 📊 进度快照（截至 Round 341）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | 290→341（52 个模块） |
| 最近一轮 | **Round 341**：blocked source short-circuit（Node 第 2099 行） |
| Round 341 测试 | **5/5 全部通过真实 PostgreSQL** |
| pc-heartbeat 测试文件 | **51 个集成测试文件** |
| pc-heartbeat lib 测试 | **485 passed** |
| 总测试数 | **859 passed**（up from 854） |
| pc-server --bins | **编译通过**（3.65s） |

## 📈 完成度趋势

```
Round 339: ~97.5% →   Round 340: ~98%   →   Round 341: ~98.5% ✨
```

## 🔧 Round 341 关键决策

### 新增文件
- `crates/pc-heartbeat/tests/round341_blocked_source_short_circuit.rs`（~310 行，5 测试）

### 修改文件
- `crates/pc-heartbeat/src/recovery/create_or_update_stale_run_evaluation_full.rs`：
  - 增加 blocked short-circuit（在 fold 之后、auto_dismiss 之前）
  - 一行检查：`src_row.status == "blocked"` → `StaleRunEvaluationOutcome::Skipped`

### 关键实现要点

**blocked 短路**（Node 第 2099 行对齐）：
- 极简实现：单行 `if let Some(src_row) = input.source_issue_row.as_ref() { if src_row.status == "blocked" { return Skipped } }`
- 不需要 DB 查询 / 不写 activity log / 不更新 evaluation
- 业务语义：blocked source issue 的 idle output 是预期行为，不应触发 watchdog

### 测试覆盖（Round 341）

| 测试 | 场景 | 关键断言 |
|---|---|---|
| `blocked_source_skips_evaluation` | 主路径 | Skipped + 不创建 eval |
| `in_progress_source_continues_normal_flow` | 非 blocked | Created |
| `todo_source_continues_normal_flow` | todo | Created |
| `blocked_source_with_dismissed_still_skipped` | blocked + dismissed | Skipped（任意路径） |
| `blocked_source_with_existing_eval_skipped` | blocked + 已有 open eval | Skipped + eval priority 不变 |

## 🔍 Round 341 后剩余 Gap 分析

### 高 ROI（推荐 Round 342+）
1. **`appendRecoveryRunEvent` + `finalizeAgentAfterSourceResolvedRun`**（Node 第 1803 + `:1648` 行）：fold 时写 heartbeat_run_events + 更新 agent.status
2. **`collectStaleRunEvidence` 完整版**：safe_tail + recent_events + childIssues + blockers（description builder 已支持，data collector 待实现）
3. **`cleanupSourceResolvedRunProcess`**（Node 第 1576 行）：fold 时 process kill / terminate（仅 SESSIONED_LOCAL_ADAPTERS）

### 中 ROI
4. **`activeRecoveryAction.resolve`**：fold 时清理 active recovery action
5. **`getCurrentUserRedactionOptions`** + **`redactWatchdogEvidenceText`**：description / evidence redaction

### 低 ROI
6. **HeartbeatRunActor 注入 Db**
7. **UI routes (pc-http) 覆盖率补全**

## 📋 Round 342 候选优先级

**首要（推荐 Round 342）**：
- **`appendRecoveryRunEvent` + `finalizeAgentAfterSourceResolvedRun`** 接入 fold path
- 估计代码量：~80 行 + 2 测试
- 收益：fold 时记录 lifecycle event + 同步 agent.status

**次要（Round 343）**：
- **`collectStaleRunEvidence` 完整版**
- 估计代码量：~120 行 + 3 测试
- 收益：description 中 safe_tail + events + child issues + blockers 全部填实

## 📊 完成度更新

| 模块 | 之前 | 当前 |
|---|---|---|
| `resolve_recovery_owner_agent` (stranded) | ✓ R315 | ✓ |
| `resolve_stale_run_owner_agent` (stale run) | ✓ R337 | ✓ |
| `scan_silent_active_runs` 主循环接通 full | ✓ R337 | ✓ |
| `create_or_update_stale_run_evaluation` (minimal) | ✓ R290 | ✓ |
| `create_or_update_stale_run_evaluation` (full) | ✓ R336 | ✓ |
| `is_recovery_origin_issue` 递归短路 | ✓ R338 | ✓ |
| `is_terminal_issue_status` | ✓ R339 | ✓ |
| `fold_source_resolved_stale_run` 主循环 | ✓ R339 | ✓ |
| `auto_dismiss_closed_evaluation` 主循环 | ✓ R340 | ✓ |
| **`blocked` source short-circuit** | ❌ | ✓ R341 |
| `appendRecoveryRunEvent` | ❌ | ❌ |
| `finalizeAgentAfterSourceResolvedRun` | ❌ | ❌ |
| `collectStaleRunEvidence` 完整版 | ❌ | ❌ |

**Recovery 子系统总进度：~98.5%**

## 🎯 下一轮目标

**Round 342**：
- 实现 `append_recovery_run_event`（Node `nextRunEventSeq` + `appendRecoveryRunEvent`）：写 heartbeat_run_events 行
- 实现 `finalize_agent_after_source_resolved_run`：更新 agent.status (running → idle)
- 在 fold path 接入
- 测试 2-3 个 case

**预估收益**：完成度 ~98.5% → ~99%

---

# Round 342 — `appendRecoveryRunEvent` + `finalizeAgentAfterSourceResolvedRun` 接入 fold path

## 📊 进度快照（截至 Round 342）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | 290→342（53 个模块） |
| 最近一轮 | **Round 342**：fold path 写 lifecycle event + 同步 agent 状态 |
| Round 342 测试 | **7/7 全部通过真实 PostgreSQL** |
| pc-heartbeat 测试文件 | **52 个集成测试文件** |
| pc-heartbeat lib 测试 | **485 passed** |
| 总测试数 | **866 passed**（up from 859） |
| pc-server --bins | **编译通过**（26.93s） |

## 📈 完成度趋势

```
Round 340: ~98%   →   Round 341: ~98.5% →   Round 342: ~99% ✨
```

## 🔧 Round 342 关键决策

### 新增文件
- `crates/pc-heartbeat/src/recovery/append_recovery_run_event.rs`（~80 行）—— Node 第 1568 行端口
- `crates/pc-heartbeat/src/recovery/finalize_agent_after_source_resolved_run.rs`（~80 行）—— Node 第 1648 行端口
- `crates/pc-heartbeat/tests/round342_append_recovery_run_event_and_finalize_agent.rs`（~360 行，7 测试）

### 修改文件
- `crates/pc-heartbeat/src/recovery/mod.rs`：注册 2 个新模块 + re-exports
- `crates/pc-heartbeat/src/recovery/create_or_update_stale_run_evaluation_full.rs`：
  - fold path 接入 append_recovery_run_event + finalize_agent_after_source_resolved_run

### 关键实现要点

**`append_recovery_run_event`** (Node 第 1568 行对齐)：
- 直接 SQL（绕过 pc_repos::HeartbeatRepo::append_event_full 因其需要 HeartbeatRow 完整 struct）
- 事务内 SELECT MAX(seq)+1 + INSERT atomic
- event_type='lifecycle', stream='system'

**`finalize_agent_after_source_resolved_run`** (Node 第 1648 行对齐)：
- 查其他 running runs（排除被 fold 的 run_id）
- 决定 next_status：count>0→running；succeeded/cancelled→idle
- UPDATE agents WHERE status NOT IN (paused, terminated)

**fold path 完整集成**：
1. fold_source_resolved_stale_run (R339) 完成事务
2. log_source_resolved_fold_activity (R339)
3. **R342 NEW**: append_recovery_run_event (lifecycle info)
4. **R342 NEW**: finalize_agent_after_source_resolved_run
5. return Folded

### 测试覆盖（Round 342）

| 测试 | 场景 | 关键断言 |
|---|---|---|
| `append_recovery_run_event_writes_event` | 单次写入 | 1 个 lifecycle event, seq=1 |
| `append_recovery_run_event_seq_monotonic` | 多次写入 | seq 1,2,3 单调递增 |
| `finalize_agent_running_to_idle` | 无其他 run | running → idle |
| `finalize_agent_keeps_running_when_other_runs_exist` | 有其他 run | 保持 running |
| `finalize_agent_skips_paused_or_terminated` | paused/terminated | 不覆盖 |
| `finalize_agent_cancelled_status` | cancelled final_status | → idle |
| `fold_path_writes_event_and_finalizes_agent` | fold path 集成 | event 写入 + agent=idle |

## 🔍 Round 342 后剩余 Gap 分析

### 高 ROI（推荐 Round 343+）
1. **`collectStaleRunEvidence` 完整版**（Node 第 1852 行）：safe_tail + recent_events + childIssues + blockers
   - 这是 description builder 的 data collector，目前 evidence 只有 silence_age_ms
   - 估计代码量：~150 行 + 4 测试
   - 收益：description 内容丰富化（实际 run log tail + 关联 issues）
2. **`cleanupSourceResolvedRunProcess`**（Node 第 1576 行）：fold 时 process kill / terminate（仅 SESSIONED_LOCAL_ADAPTERS）
3. **`activeRecoveryAction.resolve`**（Node 第 1772 行）：fold 时清理 active recovery action

### 中 ROI
4. **`getCurrentUserRedactionOptions`** + **`redactWatchdogEvidenceText`**：description / evidence redaction
5. **`buildExecutionReviewParticipantRecoveryComment` 接入 escalate_db**

### 低 ROI
6. **HeartbeatRunActor 注入 Db**（kameo actor → recovery lib）
7. **UI routes (pc-http) 覆盖率补全**

## 📋 Round 343 候选优先级

**首要（推荐 Round 343）**：
- **`collect_stale_run_evidence` 完整版**（Node 第 1852 行）
- 估计代码量：~150 行 + 4 测试
- 收益：description 中填实 safe_tail + events + child issues + blockers

**次要（Round 344）**：
- **`cleanup_source_resolved_run_process` 接入 fold path**
- 估计代码量：~120 行 + 3 测试（process kill 涉及子进程管理）
- 收益：fold 时清理 local process（仅 local adapter）

## 📊 完成度更新

| 模块 | 之前 | 当前 |
|---|---|---|
| `resolve_recovery_owner_agent` (stranded) | ✓ R315 | ✓ |
| `resolve_stale_run_owner_agent` (stale run) | ✓ R337 | ✓ |
| `scan_silent_active_runs` 主循环接通 full | ✓ R337 | ✓ |
| `create_or_update_stale_run_evaluation` (minimal) | ✓ R290 | ✓ |
| `create_or_update_stale_run_evaluation` (full) | ✓ R336 | ✓ |
| `is_recovery_origin_issue` 递归短路 | ✓ R338 | ✓ |
| `is_terminal_issue_status` | ✓ R339 | ✓ |
| `fold_source_resolved_stale_run` 主循环 | ✓ R339 | ✓ |
| `auto_dismiss_closed_evaluation` 主循环 | ✓ R340 | ✓ |
| `blocked` source short-circuit | ✓ R341 | ✓ |
| **`append_recovery_run_event`** | ❌ | ✓ R342 |
| **`finalize_agent_after_source_resolved_run`** | ❌ | ✓ R342 |
| `collectStaleRunEvidence` 完整版 | ❌ | ❌ |
| `cleanupSourceResolvedRunProcess` | ❌ | ❌ |

**Recovery 子系统总进度：~99%**

## 🎯 下一轮目标

**Round 343**：
- 实现 `collect_stale_run_evidence`（Node 第 1852 行）：
  - `safe_tail` —— read_run_log_tail_for_evidence + redactWatchdogEvidenceText
  - `recent_events` —— heartbeat_run_events DESC LIMIT 8
  - `child_issues` —— issues WHERE parent_id = source_issue.id
  - `blockers` —— issue_relations WHERE related_issue_id = source_issue.id AND type='blocks'
- 接入 create_or_update_stale_run_evaluation_full 的 handle_create
- 测试 4 个 case

**预估收益**：完成度 ~99% → ~99.5%

---

## Round 343-344 增量（2026-08-07）

### R343：`collect_stale_run_evidence`

已完成 Node `collectStaleRunEvidence` 的核心 DB 采集路径，并接入 stale evaluation 创建流程：

- 计算 `silence_age_ms`：`last_output_at → process_started_at → started_at → created_at`。
- 采集最近 8 条 `heartbeat_run_events`，恢复为时间升序。
- 采集 source issue 的 child issues 与 `blocks` 关系对应的 blockers。
- `handle_create` 使用真实 evidence 构建 description，而不是空 evidence。
- 尚未迁移 `read_run_log_tail_for_evidence`、用户级 redaction；`safe_tail` 当前为 `None`，事件消息暂不脱敏。

真实 PostgreSQL 验证：`round343_collect_stale_run_evidence` 共 6 项全部通过。

### R344：`activeRecoveryAction.resolve`

已完成 source-resolved fold 的 action 收敛：

- 新增 `resolve_active_recovery_action_after_source_resolved` DB-only 模块。
- 仅匹配同公司、同 source issue、`kind = active_run_watchdog` 且状态为 `active/escalated` 的 action。
- 更新为 `status = resolved`、`outcome = false_positive`，写入 Node 对齐的 resolution note、`resolved_at`、`updated_at`。
- 无匹配时返回 `None`，重复调用天然幂等；不影响其他 recovery action 类型。
- 已接入 `fold_source_resolved_stale_run` 的事务后流程。

真实 PostgreSQL 验证：R339 fold 回归 8 项全部通过；R342 回归 7 项全部通过；R343 回归 6 项全部通过；heartbeat 全部集成测试通过。

### 当前剩余差距

1. **`cleanupSourceResolvedRunProcess`**：local session adapter 的 PID/process group 存活检测、优雅终止、强制终止和 fold result 记录尚未迁移。
2. **Evidence redaction**：`getCurrentUserRedactionOptions`、`redactWatchdogEvidenceText` 与 run log tail 读取尚未迁移。
3. **Recovery comment 副作用**：`buildExecutionReviewParticipantRecoveryComment` 的纯函数已有实现，但与 `escalate_db` 的完整写入链路仍需核对。
4. **并发语义**：R343 evidence 四路查询目前 sequential，Node 使用 `Promise.all`；可用 `tokio::try_join!` 优化，但不改变业务结果。
5. **Actor/HTTP 边界**：Heartbeat actor 的 Db 注入与 pc-http 对应路由的完整覆盖仍低于 Node。

### 后续计划（按核心价值）

- **R345：local process cleanup** —— 抽象 `ProcessTerminator` trait，将存活检测、优雅退出、超时强杀与结果建模隔离；先 pure 测试，再用真实短命子进程 PostgreSQL fold 验证。
- **R346：evidence redaction** —— 迁移用户 redaction options 与文本脱敏，确保 description、event message、safe tail 共享同一纯函数策略。
- **R347：并发 evidence collector** —— 用 `tokio::try_join!` 并行执行独立查询，补错误传播和空 source issue 场景。
- **R348+：副作用与边界补齐** —— 完成 recovery comment 写入接线、actor Db 注入、pc-http 路由契约测试，并逐模块对照 Node 测试。

### 完成度判断

Recovery 核心 stale-run 主链已覆盖：递归防护 → false-positive 去重 → terminal fold → blocked 短路 → closed evaluation 自动 dismiss → evidence 采集 → evaluation 创建/升级 → run event → agent finalize → recovery action 收敛。当前剩余主要集中在本地进程生命周期、敏感信息脱敏、运行时边界和 UI/API 覆盖，不能将“~99%”理解为所有 Node 代码已逐行等价。
