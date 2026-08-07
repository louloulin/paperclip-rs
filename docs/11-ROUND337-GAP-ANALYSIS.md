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

## Round 345 增量（2026-08-07）：local process cleanup

### R345：`cleanupSourceResolvedRunProcess`

完成 Node 第 1576 行的本地进程清理端口：

- 新增纯异步模块 `pc_heartbeat::recovery::cleanup_source_resolved_run_process`
  - 适配 `claude_local / codex_local / cursor / gemini_local / hermes_local / opencode_local / pi_local`
  - 探测 `kill -0`（pid 与 negative pgid），仅在进程存活时进入终止流程
  - SIGTERM → 轮询等待 → SIGKILL（force_after_ms=2000）
  - 返回 `attempted / outcome / adapter_type / pid / process_group_id / error`，可被 fold 落库为可审计 payload
- `fold_source_resolved_stale_run` 现读取 `process_pid / process_group_id / agent_id` 并把 cleanup 结果写入 `sourceResolvedWatchdogFold.cleanup`
- 数据库查询 `agents.adapter_type` 只在 fold 内部出现，进程模块不依赖 `Db`，高内聚低耦合

### 验证

- `round345_cleanup_source_resolved_run_process`：4/4 通过（真实 `sleep 30` 子进程被成功 TERM/KILL）。
- `round339_is_terminal_and_fold`：`source_issue_done_with_evidence_folds_run` 现断言
  - `child.try_wait().unwrap().is_some()` 真实子进程退出
  - `result_json.sourceResolvedWatchdogFold.cleanup.outcome ∈ {terminated, termination_sent_still_running}`
- `pc-heartbeat` 全部 54 个集成测试文件通过

### 完成度判断

Recovery 主链目前覆盖：递归防护 → false-positive 去重 → terminal fold → blocked 短路 → closed evaluation 自动 dismiss → evidence 采集 → evaluation 创建/升级 → run lifecycle event → agent finalize → recovery action 收敛 → 本地进程清理（含 PID/process group）。

剩余主要是：evidence redaction、并发优化、actor/HTTP 边界与 UI 覆盖。

## Round 346 增量（2026-08-07）：evidence redaction

### R346：`redactCurrentUserText` + `redactWatchdogEvidenceText`

完成 Node 第 1470 行的脱敏纯函数端口：

- 新增模块 `pc_heartbeat::recovery::redact_watchdog_evidence_text`
  - 输入 `CurrentUserRedactionOptions { enabled, user_names, home_dirs, replacement }`
  - 行为对齐 Node：屏蔽 home directory 路径最后一段与用户名（word boundary）
  - `enabled=false` / 空输入直通；不依赖 `regex crate`（不支持 look-around），自实现 byte-level word boundary
- 4/4 测试通过；与 Node 实际输出一致：`alice -> a*****`、`/Users/alice -> /Users/a*****`、`bob=alice&bobby -> b***=alice&bobby`

### 完成度判断

后续接入点：`collect_stale_run_evidence` 的 event message 与 safe_tail、`build_stale_run_evaluation_description` 渲染。下一轮引入 instance settings（`censorUsernameInLogs`）读取并接入。

## Round 347 增量（2026-08-07）：redaction 接入 description builder

### R347：把 R346 redaction 接入 `build_stale_run_evaluation_description`

- 新增可选字段 `BuildStaleRunEvaluationDescriptionInput.redaction: Option<CurrentUserRedactionOptions>`
- 新增 `apply_redaction(input, options)` 私有 helper：调用 `redact_watchdog_evidence_text`，`None` 直通
- 渲染 safe_tail（"Last Output Excerpt" 段）和 recent_events 的 event message 时统一应用 redaction
- 默认所有现有调用方均传 `redaction: None`，保持行为兼容
- 新增 `round347_redact_evidence_integration`：4 项测试覆盖
  - 纯函数 helper 行为（用户名、家目录、disabled）
  - DB 模块 `collect_stale_run_evidence` 返回原始数据（无 redaction），保持职责单一
  - description builder 在 `redaction=Some(opts)` 时把 safe_tail 与 event message 屏蔽为 `a*****`

### 验证

- 56 个集成测试文件全部通过；`pc-server --bins --no-run` 编译通过
- 红 → 绿：先因缺字段 `redaction` 失败，补齐签名后 4/4 通过

### 后续计划

- `create_or_update_stale_run_evaluation_full` 接入 instance_settings.censor_username_in_logs 读取，把当前 `None` 升级为真实 options。
- 把同样的 redaction 接到 `collect_stale_run_evidence` 内（让 event message 在 DB 层就先脱敏，更适合写入 issues.description），但需要先评估是否与“DB 模块保持纯粹职责”冲突——倾向于让上层 builder 做最终脱敏，DB 返回原始数据。
- `build_execution_review_participant_recovery_comment` 接入 escalate_db 的 comment 写入。
- `tokio::try_join!` 并发优化 collect_stale_run_evidence。
- Heartbeat actor 与 pc-http 路由契约补全。

## Round 348-349 增量（2026-08-07）：instance settings 驱动脱敏

### R348：设置到脱敏选项的端到端验证

- 新增 `round348_redaction_from_instance_settings`，覆盖 builder 显式接收 redaction options，以及从真实 PostgreSQL `instance_settings.general` 读取 `censorUsernameInLogs / usernames / homeDirs` 后驱动脱敏。
- 验证中直接读取 `singleton_key = 'singleton'`，避免测试夹具被额外设置写操作影响。
- 修复了该轮测试文件遗漏 `ensure_instance_settings` 函数声明导致的语法错误；专项测试 2/2 通过。

### R349：主编排真实接线

- 新增 `load_watchdog_redaction_options` 模块：
  - `watchdog_redaction_options_from_general` 只负责 JSON 到 `CurrentUserRedactionOptions` 的纯转换；关闭开关时返回 `None`。
  - `load_watchdog_redaction_options` 只负责读取 singleton setting，缺失行时安全返回 `None`。
- `create_or_update_stale_run_evaluation_full::handle_create` 在收集 evidence 后读取设置，并把 options 传入 `build_stale_run_evaluation_description`。
- evidence collector 继续返回原始数据；脱敏只发生在最终 description 渲染边界，避免数据采集与展示策略耦合。
- 新增 `round349_full_orchestrator_settings_redaction`：真实写入 instance settings、heartbeat run event 与 stale run，调用完整主编排创建 evaluation issue，并断言 description 中：
  - `alice` 被转换为 `a*****`；
  - `/Users/alice/work` 被转换为 `/Users/a*****/work`；
  - 原始用户名不再出现。

### 验证

- R349 严格走红绿循环：接线前测试在 `description.contains("a*****的一项断言")` 处失败，接线后 1/1 通过。
- `cargo test -p pc-heartbeat --tests -- --test-threads=1`：全部单元与 58 个集成测试文件通过。
- `cargo fmt --all` 已执行；仓库仍存在历史 warning，本轮未扩大范围清理无关告警。

### 当前差距与后续优先级

Recovery stale-run 主链现已包含 instance settings 驱动的 evidence redaction，核心业务闭环进一步接近 Node。剩余差距按价值排序：

1. **Recovery comment 写入链路**：`buildExecutionReviewParticipantRecoveryComment` 与 unavailable comment 的纯函数已有，仍需完整接入 `escalate_db` 的持久化与幂等路径。
2. **Evidence 查询并发**：`collect_stale_run_evidence` 的 events、children、blockers 当前顺序执行；可用 `tokio::try_join!` 对齐 Node `Promise.all`，降低 heartbeat 扫描延迟。
3. **更完整的 sensitive-text redaction**：当前仅实现当前用户名和 home directory；Node 组合的 token、命令及其他敏感文本清洗仍需独立模块迁移。
4. **Actor/HTTP 边界**：Heartbeat actor 的 Db/服务装配、pc-http recovery 路由契约和错误映射仍低于 Node。
5. **UI 与运维可观测性**：evaluation、cleanup、redaction 的 API/UI 展示与指标仍未逐项对齐。

完成度仍记为 **Recovery 核心主链约 99%**，该数字表示 stale-run recovery 关键路径，而不是整个 Node paperclip 已逐行完成 Rust 等价复刻。

## Round 350 增量（2026-08-07）：source escalation comment override

### Node 对照结论

Node `escalateStrandedAssignedIssue` 接受可选 `input.comment`，execution-review participant 的 recovery/unavailable builder 只是该通用入口的两个调用方。Node 会把业务说明正文与统一的 recovery action、owner、next action 信息组合后写入 source issue，并用 `Recovery action: <id>` marker 去重。

Rust 原实现仅持久化 `decide_escalation` 生成的通用正文，导致 R330/R331 虽然完成纯函数，但无法通过 source escalation 写入真实 issue comment。

### R350 实现

- 新增兼容入口 `escalate_stranded_assigned_issue_with_comment`：
  - 原 `escalate_stranded_assigned_issue` 保持签名不变，内部以 `comment = None` 委托新入口。
  - comment override 仅替换 source escalation 的业务说明前缀，不允许调用方覆盖 recovery marker、owner 和 next-action 结构。
  - recovery action marker 继续由计划层生成并用于 DB 去重，保持低耦合和幂等性。
- 新增私有纯变换 `apply_comment_override`，DB helper 不感知 execution-review 具体文案。
- R330 `build_execution_review_participant_recovery_comment` 与 R331 unavailable builder 现在均可通过同一通用入口真实持久化。

### 真实验证

- 新增 `round350_escalation_comment_override`，真实 PostgreSQL 覆盖 2 个分支：
  - 自动恢复失败正文写入，并追加 recovery action marker 与 owner。
  - participant unavailable 正文写入；重复升级返回 `Skipped`，comment 总数保持 1。
- 严格红绿循环：测试先因新入口不存在编译失败，实现后 2/2 通过。
- `pc-heartbeat` 全部单元测试及 59 个集成测试文件通过。

### 剩余差距

1. Node source escalation 的 `activity_log` 详情、comment presentation/metadata 尚未完整映射。
2. execution-review reconciliation 主循环尚未把自动失败/不可调用分支直接路由到新 comment override 入口；当前已具备独立的 builder 与 DB 原子能力。
3. provider quota、configuration incomplete、successful handoff 等 source escalation 的特化 notice/presentation 仍需逐模块补齐。
4. evidence 查询并发、敏感文本扩展、actor/HTTP/UI 边界仍是后续重点。

## Round 351 增量（2026-08-07）：sweep 真实接线 execution-review participant 评论

### Node 对照结论

Node `reconcileStrandedAssignedIssues` 主循环中，execution-review participant 分支直接调 `escalateStrandedAssignedIssue({comment: buildExecutionReviewParticipantUnavailableComment|RecoveryComment})`。Rust 之前只在纯函数层完成这两个 builder，sweep 内调用的是无 override 的通用入口，因此真实数据库写入的仍是通用 stranded 文案。

### R351 实现

- 在 `scheduler_db::reconcile_and_escalate_stranded_for_company` 内新增纯选择器 `execution_review_escalation_comment`，仅当 `issue.status == "in_review"` 且 `latest_run.context_snapshot.retryReason == "execution_review_participant_recovery"` 且 `latest_run.status` 属未成功终态时返回 builder 结果；其他分支继续走通用 stranded 文案。
- 选择器直接复用 R330 的 `build_execution_review_participant_recovery_comment` 与现有 `EscalationRunView`，没有重新生成模板，保持与 Node 文案一致。
- sweep 改用 R350 通用 `escalate_stranded_assigned_issue_with_comment`，业务正文与 R350 marker/owner 行为自动叠加。

### 真实验证

- 新增 `round351_review_sweep_comment_wiring`：in_review execution_state 含 participant agent、failed run retryReason 为 `execution_review_participant_recovery`，断言真实写入的 comment 以 `"Paperclip retried the pending execution-review participant once"` 开头，且含 `Latest retry failure details were withheld` 与 `Recovery action:`。
- TDD 红灯：接线前 comment 仍以通用 `"Paperclip exhausted automatic recovery for the assigned issue and escalated to \`blocked\`"` 开头；接线后 1/1 通过。
- `pc-heartbeat`：488 个单元测试及 60 个集成测试文件全部通过。
- 通用 sweep（`round295_sweep_escalate`）与 escalation（`round294_escalate`）回归保持原行为，证明新选择器对其他 cause 透明。

### 剩余差距

1. `participantLatestRun` 不可用分支（unavailable）以及 `configuration_incomplete`、已 `didAutomaticRecoveryFail` 但 run 非终止状态的分支暂未单独走 sweep 短路。
2. Node 的 review sweeper 中 `providerQuota` / `classificationIncomplete` 路径与 `enqueueStrandedIssueRecovery` 的 fallback 优先级尚未一一映射。
3. activity log、comment metadata/presentation 仍待补齐。

## Round 351b 增量（2026-08-07）：participant unavailable 真实接线

### 实现

- sweep 真实读取 execution-review `currentParticipant.agentId` 对应 agent，并通过统一的 invokable 状态判定识别 offline、缺失或跨公司的 participant。
- unavailable 文案支持 `latestRun = null`，不再伪造 heartbeat run；无 run 时仍创建 `execution_review_participant_recovery` source-scoped action，并由 participant 作为 recovery owner。
- scheduler 的 latest run 读取改为可选输入：有 run 时保留原始 evidence，没有 run 时使用空 `SchedulerRunInput`，允许 Node 的“无 live reviewer run + participant 不可调用”分支完成 blocked/escalation 闭环。
- 保持 R350 通用 comment override、recovery marker、owner 与幂等逻辑不变。

### 真实验证

- `round351_review_sweep_comment_wiring` 2/2 通过：自动恢复失败文案、offline 且无 latest run 的 unavailable 文案均通过真实 PostgreSQL 写入验证。
- `cargo test -p pc-heartbeat --tests -- --test-threads=1` 全部通过；`cargo fmt --all -- --check` 通过。

## Round 352 增量（2026-08-07）：execution-review configuration_incomplete

### Node 对照结论

Node 在 participant latest run 为终止失败且 adapter failure 分类为 `configuration_incomplete` 时，优先记录配置修复要求并阻断 issue，不进入普通 participant retry、provider quota monitor 或重复 requeue 路径。

### 实现

- 新增纯函数 `build_configuration_incomplete_comment`，复用统一 failure summary，避免把敏感 adapter 错误全文直接写入 issue comment。
- review sweep 复用已有 adapter failure classifier；命中配置不完整时跳过前置普通 scheduler dispatch，在 escalation 阶段传递 `ConfigurationIncomplete` cause 与 participant recovery owner。
- recovery action 使用 `configuration_validation` kind / `manual_repair_required` wake policy，真实 comment 保留 recovery marker、owner 与 next action 结构。

### 真实验证

- 新增真实 PostgreSQL 场景：终止的 `adapter_failed` + `missing API key` participant run 写入配置特化 comment，issue 变为 `blocked`，action cause 为 `configuration_incomplete`，且 wake 数为 0。
- `pc-server --bins` 编译通过；R352 专项测试通过，完整 heartbeat 回归在本轮继续执行。

### 当前剩余差距

1. provider quota 在 review participant 分支已具备 monitor 路径，但 notification/comment presentation 与 Node activity metadata 还未逐字段复刻。
2. `didAutomaticRecoveryFail` 的非终止 run 边界、成功 handoff、continuation interaction 优先级仍需与 Node 主循环做更细粒度的状态矩阵对照。
3. activity log actor、comment presentation/metadata、HTTP/API/UI 读写契约仍是 Recovery 之外的主要差距。

## Round 353 增量（2026-08-07）：recovery comment presentation / metadata

### Node 对照结论

Node 的 `escalateStrandedAssignedIssue` 不只写 comment body，还会为 system recovery notice 写入：

- compact `system_notice` presentation（warning tone、compact density、默认折叠详情）；
- versioned recovery metadata，包含 action、cause、previous status、owner 和 latest run；
- metadata 与 body marker 一起参与重复通知抑制。

Node execution-review provider quota 分支本身是 monitor-only：命中 quota 时创建/更新 monitor，不立即 blocked，也不应额外写 escalation notice；因此本轮没有人为增加 quota comment。

### R353 实现

- 新增纯模块 `build_recovery_comment_display`，集中构建 presentation、metadata 与 cause title，DB 层不重复拼装 JSON 结构。
- `IssueCommentRow` 增加 `presentation` / `metadata` 字段；保留原 `create_comment` API，并新增 `create_comment_with_display` 供 recovery notice 使用。
- source escalation 唯一写库点统一写入 display 数据；旧 comment body、recovery marker、owner 和幂等行为保持不变。
- HTTP 层复用 `IssueCommentRow` 序列化，因此 issue comments 查询可直接返回新增字段。

### 真实验证

- 在真实 PostgreSQL 的 `configuration_incomplete` review participant 场景中断言：comment body、issue blocked、action cause、无 wake、presentation kind/tone、metadata version/section 全部符合预期。
- `cargo test -p pc-heartbeat --tests -- --test-threads=1`：491 个单元测试及全部集成测试通过。
- `cargo check -p pc-server --bins`：通过。
- `cargo fmt --all -- --check`：通过。

### 下一轮重点

1. 将同一 display contract 补到 recovery issue in-place comment，并覆盖无 latest run 的 metadata 形态。
2. 对照 Node `noticeMetadataReferencesRecoveryAction`，让 metadata 引用也能作为 dedup marker，而不是只检查 body。
3. 完成 provider quota monitor 的 retry-at / monitor state / wake backstop 双端状态矩阵。

## Round 354 增量（2026-08-07）：in-place recovery comment presentation/metadata + metadata-aware dedup

### Node 对照结论

Node 在 `escalateStrandedRecoveryIssueInPlace` 路径下同样会写入 recovery notice presentation/metadata，但相对 source escalation 有两点差异：

- 没有 `Recovery action` 行（in-place 不创建新的 recovery action，action 仍归属于 source issue）；
- `noticeMetadataReferencesRecoveryAction` 通过 `sections[].rows[].{type=key_value,label="Recovery action"}` 来识别一次 system comment 是否已经引用某个 action，使 metadata 本身成为幂等性载体（不完全依赖 body 文本）。

Rust 之前的 in-place 升级在 DB 层只写 body markdown，缺 presentation/metadata；同时 `apply_in_place_escalation` 完全没有 marker 判断，重复调用会写第二条 system comment。

### R354 实现

- `build_recovery_issue_in_place_escalation_comment` 抽出 `IN_PLACE_ESCALATION_MARKER` 公开常量（body 稳定前缀），供 dedup 判定复用。
- `build_recovery_comment_display::RecoveryCommentDisplayInput.recovery_action_id` 改为 `Option<Uuid>`，in-place 场景不再传伪造值；`build_recovery_notice_metadata` 在 `recovery_action_id = None` 时跳过 `Recovery action` 行，但保留 `Cause` / `Previous status` / `Recovery owner` 三行（owner 默认 `board`）。
- 新增纯函数 `metadata_references_recovery_action(metadata, action_id)`，完全对齐 Node `noticeMetadataReferencesRecoveryAction` 的 `sections[].rows[]` 形状判定。
- `apply_in_place_escalation` 现在在写 comment 前先检查最近 50 条 system comment 是否含 `IN_PLACE_ESCALATION_MARKER`，命中则静默跳过；修复了 in-place 重复调用时的二次写入漏洞。
- `comment_already_references_marker` 升级为查询最近 50 条 system comment 的 `body` 与 `metadata`，source escalation 同时支持 body marker 与 metadata `Recovery action` 引用双重判定。
- 主入口 `escalate_stranded_assigned_issue_with_comment` 的 `RecoveryInPlace` 分支同样构建 presentation/metadata，确保主入口与 `escalate_stranded_recovery_issue_in_place` 行为对齐。

### 真实验证

- 新增 `round354_in_place_recovery_comment_display.rs`（真实 PostgreSQL）：
  - `in_place_with_run_writes_presentation_and_metadata_without_action_row`：含 latest run 时 metadata 4 行（无 `Recovery action`），含 run_link 行；
  - `in_place_without_run_omits_run_link_row`：无 latest run 时 metadata 仅 3 行（Cause + Previous status + Recovery owner=board），断言无 `run_link` 行；
  - `in_place_repeat_does_not_double_write`：第二次调用仍走 `RecoveryInPlace`，但 `apply_in_place_escalation` 通过 body marker 跳过第二次写入，`comment_id` 为 None；
  - `source_escalation_repeat_does_not_double_write`：source escalation 第二次调用 system comment 总数保持 1，且首条 comment 的 `metadata_references_recovery_action(action_id)` 为 true。
- 扩展 `round351_review_sweep_comment_wiring::configuration_incomplete_review_participant_writes_configuration_comment`：断言 `metadata_references_recovery_action(Some(metadata), action_id)` 为 true，确认 metadata 引用 action id 真正落到 PG 行。
- `cargo test -p pc-heartbeat --tests -- --test-threads=1`：904 个测试全部通过（490 单元 + 414 集成），比 R353 增加 4 个 round354 集成测试，0 失败。
- `cargo check -p pc-server --bins`：通过。
- `cargo fmt --all -- --check`：通过。

### 当前剩余差距

1. provider quota monitor 的 retry-at / monitor state / wake backstop 双端状态矩阵仍未与 Node 完全对照（monitor 创建/解析路径已实现，但状态转换分支覆盖不足）。
2. `successful_run_missing_state` cause 的特化 notice/presentation 仍是 fallback 通用路径（其他 cause 都已特化）。
3. `workspace_validation_failed` cause 特化路径的 retry reason 字段走通用 `execution path recovery failed`，未注入 fingerprint 摘要。
4. activity log actor、HTTP/API 序列化、UI 展示仍然是 Recovery 之外的主要差距。
5. recovery issue comment 的 metadata 没有 `Recovery action` 行，因此 in-place 路径目前仅依赖 body marker dedup；如果未来需要 metadata 级 dedup（例如多端 UI 重渲染），需要为 in-place 引入幂等键（例如源 issue id + fingerprint）。

## Round 355 增量（2026-08-07）：ensure_provider_quota_wait_recovery_monitor 接线

### Node 对照结论

Node `services/recovery/service.ts` 的 `ensureSourceScopedStrandedRecoveryAction` 之后，无条件紧跟一个判断：

```
isProviderQuotaWait = cause === "provider_quota"
                  && !recoveryAction.ownerAgentId
                  && Boolean(recoveryAction.returnOwnerAgentId);
if (isProviderQuotaWait) await ensureProviderQuotaWaitRecoveryMonitor({...});
```

即只要 recovery action 因 provider_quota 被创建且没有 owner agent，Node 会自动创建一个 scheduled_retry heartbeat_run + 一个 queued wakeup，把 action.monitor_policy 的 `{type:"wait_recovery", scheduledRunId, retryAt}` 写回。

Rust 端已有 `ensure_provider_quota_wait_recovery_monitor` 模块（`provider_quota_recovery_monitor.rs`，round318 已 db unit test 覆盖），但 `persist_source_scoped_recovery_action` 没有调用它，导致 `monitor_only` action 的 `scheduledRunId` 永远为空，外部 system 无法触发 retry。

### R355 实现

- 在 `persist_source_scoped_recovery_action`（`orchestrator.rs`）upsert 完成后，判断 `cause == "provider_quota" && owner_agent_id == None && return_owner_agent_id == Some`，
  调用新薄壳 `ensure_provider_quota_monitor_for_action` → `ensure_provider_quota_wait_recovery_monitor`，
  把 monitor_policy 的 scheduledRunId / retryAt 真正写入。
- 写入后再重新读取 action 行，让 caller 看到 `monitor_policy.scheduledRunId`（之前会被 stale `in_memory` action 掩盖）。

### 真实验证

`round355_provider_quota_wait_monitor_wiring.rs`（3 个全过）：

- `action_creation_for_provider_quota_with_invokable_assignee_creates_scheduled_retry`：显式 `recovery_cause_override=ProviderQuota` → action cause/provider_quota + wake_policy=monitor_only + return_owner_agent_id=Some(agent) + scheduled_retry run + queued wakeup + monitor_policy.scheduledRunId 与实际 scheduled_run 一致 + action.timeout_at 已设置。
- `auto_classification_via_run_error_code_triggers_monitor_wiring`：仅置 `error_code=provider_quota`、不传 override → 自动分类到 ProviderQuota，验证接线自动触发。
- `repeat_invocation_is_idempotent`：第二次调用不增加 scheduled_retry run 与 queued wakeup 计数。

`cargo test -p pc-heartbeat --tests -- --test-threads=1`：**907 passed, 0 failed**（R354 → R355 +3 个集成测试）。
`cargo check -p pc-server --bins`：通过；`cargo fmt --all -- --check`：通过。

### 剩余差距

1. Provider quota wake backstop（rounded `enqueue_source_scoped_stranded_recovery_wake` 在 ProviderQuota + 无 owner 场景下不创建 wake，与 Node 一致）已经覆盖；下次再深入 monitor 的 `wakeup_request_id` 交叉调用 `agent_wakeup_requests.wakePolicy.type` 字段验证。
2. successful_run_missing_state cause 的特化 presentation/comment 仍是 fallback 通用路径。
3. workspace_validation_failed cause 的 retry reason 注入 fingerprint 摘要尚未实现。
4. HTTP/API 序列化、actor 权限、UI 渲染等仍是 Recovery 之外的主要差距。
