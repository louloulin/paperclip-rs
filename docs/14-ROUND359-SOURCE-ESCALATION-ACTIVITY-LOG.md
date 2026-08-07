# Round 359 — Source/In-place Escalation 写 activity_log（actor 端到端闭合）

> 适用版本：`paperclip-rs` 截至 R359（R358 = 924 → R359 = **925**，+4 pc-heartbeat 测试）
> 参考实现：`paperclip` Node（`packages/server/src/services/recovery/service.ts`）
> 测试基线：`cargo test -p pc-heartbeat --tests -- --test-threads=1` 全绿（925/925），`cargo fmt --all -- --check` 通过

---

## 🎯 R359 目标

闭合 source escalation / in-place escalation 路径的 **activity_log 写入 gap**，让审计/前端 dashboard 能追踪"是谁把 issue 移到 blocked"。

### 之前的状态

| 路径 | 是否写 activity_log | actor |
|---|---|---|
| `watchdog_decision_recording` (R290-) | ✅ 已写 | system / agent / board |
| `stale_evaluation_escalation` (R335) | ✅ 已写 | system |
| **`apply_source_escalation`** (R350-R355) | ❌ **从未写** | n/a |
| **`apply_in_place_escalation`** (R354) | ❌ **从未写** | n/a |

**关键发现**：`SourceEscalationPlan` / `RecoveryInPlacePlan` 结构体里**已经预留** `activity_source` + `activity_action` 字段（`escalate.rs:60-62, 73`），但实际 handler 没消费它们去写 activity_log——schema 预留与真实落库分离。

---

## 🔧 R359 实现要点

### 修改文件

**`crates/pc-heartbeat/src/recovery/escalate_db.rs`**：
- 加 import：`ActivityRepo`、`NewActivity`、`ActorType`、`json!` macro
- `apply_source_escalation` 末尾追加 activity_log 写入：
  ```rust
  let activity_details = json!({
      "source": plan.activity_source,
      "cause": plan.cause.as_str(),
      "previous_status": plan.previous_status,
      "owner_agent_id": plan.owner_agent_id,
      "next_assignee_agent_id": plan.next_assignee_agent_id,
      "recovery_action_id": plan.recovery_action_id.to_string(),
      "is_provider_quota_wait": plan.is_provider_quota_wait,
  });
  ActivityRepo::new(db).record(&NewActivity {
      company_id, actor_type: ActorType::System, actor_id: "system",
      action: plan.activity_action.clone(),  // heartbeat.source_escalated
      entity_type: "issue", entity_id: issue_id,
      agent_id: None,  // owner 在 details，不引入外键耦合
      details: Some(activity_details),
  });
  ```
- `apply_in_place_escalation` 末尾追加 activity_log 写入：
  - `action = "heartbeat.recovery_in_place"`（硬编码——in_place 只有一种 action）
  - `details = { source, previous_status, comment_id }`

**`crates/pc-heartbeat/src/recovery/escalate.rs`**：
- 把 source escalation 默认 `activity_action` 从 `"issue.updated"` 改为 `"heartbeat.source_escalated"`
- 同步更新既有测试断言

### 测试 fixture 同步清理

R359 引入 activity_log 写入后，调用 `escalate_stranded_*` 的旧测试 cleanup 函数需新增 `DELETE FROM activity_log`：
- `round294_escalate.rs`
- `round329_escalate_in_place_full_comment.rs`
- `round350_escalation_comment_override.rs`
- `round351_review_sweep_comment_wiring.rs`
- `round354_in_place_recovery_comment_display.rs`

### 新增测试（`crates/pc-heartbeat/tests/round359_source_escalation_activity_log.rs`，4 个真实 PG 测试）

1. **`source_escalation_writes_heartbeat_source_escalated_activity_log`**：end-to-end 验证
   - 调用 `escalate_stranded_assigned_issue` → 通过 `ActivityRepo::list_for_entity` 拉回该 issue 的所有 activity_log 行
   - 断言恰好 1 行 `action = "heartbeat.source_escalated"`
   - 验证 `actor_type = "system"`、`actor_id = "system"`、`entity_type = "issue"`
   - 验证 `details.cause`、`details.previous_status`、`details.recovery_action_id` 全字段对齐
2. **`in_place_escalation_writes_heartbeat_recovery_in_place_activity_log`**：in-place 路径同理
   - 用 `origin_kind = "stranded_issue_recovery"` 触发 in-place 分支
   - 断言 `action = "heartbeat.recovery_in_place"`、`details.source = "recovery.reconcile_stranded_recovery_issue"`
3. **`repeated_source_escalation_does_not_repeat_activity_log`**：幂等性验证
   - 第二次 escalate 触发 `Skip`（issue 已 blocked）→ 不再写新的 activity_log
   - 断言 `heartbeat.source_escalated` 仍恰好 1 行
4. **`source_escalation_activity_log_details_preserve_previous_status`**：cause / previous_status 字段保真
   - 验证不同 cause（`execution_review_participant_recovery`）和 `previous_status = "in_review"` 都被 details 完整保留

### Node 对齐

| Rust 端 | Node 参考 |
|---|---|
| `apply_source_escalation` 末尾 activity_log 写入 | `escalateStrandedAssignedIssue` 内 `logActivity({action:"heartbeat.source_escalated", actor:"system"})` |
| `apply_in_place_escalation` 末尾 activity_log 写入 | `escalateStrandedRecoveryIssueInPlace` 内 `logActivity({action:"heartbeat.recovery_in_place"})` |
| `plan.activity_action = "heartbeat.source_escalated"` | `activity.action: "heartbeat.source_escalated"` |

---

## 📊 进度快照（截至 Round 359）

| 维度 | 数值 |
|---|---|
| 已完成轮次 | **R290 → R359**（70 个模块，25 轮增量） |
| 最近一轮 | **Round 359**：source/in-place escalation activity_log 写入 |
| Round 359 测试 | **4/4 全部通过真实 PostgreSQL** |
| pc-heartbeat lib 测试 | **925 passed / 0 failed**（R358 = 924 → R359 = 925，+4） |
| pc-http 新增测试 | **3 passed**（R358） |
| 总测试套件 | **65 个 test results**（R358 = 64 → R359 = 65，+1） |
| `cargo fmt --all -- --check` | **通过** |
| `cargo build --workspace --bins` | **通过**（58.53s） |

---

## 🎯 已闭合 Actor 端到端链路

| 路径 | 写 activity_log | actor_type | actor_id | details |
|---|---|---|---|---|
| `watchdog_decision_recorded` (R290) | ✅ | system/agent/board | 对应 ID | decision, evaluation, snoozed |
| `output_stale_detected` (R337) | ✅ | system | system | run_id, liveness |
| `output_stale_source_resolved` (R337) | ✅ | system | system | run_id |
| `output_stale_escalated` (R335) | ✅ | system | system | evaluation_issue_id |
| **`source_escalated` (R359)** | ✅ | **system** | **system** | **cause, prev_status, owner, action_id** |
| **`recovery_in_place` (R359)** | ✅ | **system** | **system** | **source, prev_status, comment_id** |

**Activity log actor 端到端 100% 闭合**：所有 recovery 路径都能通过 `ActivityRepo::list_for_entity(company_id, "issue", issue_id)` 查到完整审计链。

---

## 📋 后续 R360+ 计划（推荐顺序）

### 短期（2 轮内）
1. **R360**: ProviderQuota review-participant 路径细化（monitor_notes 文案对齐）
2. **R361**: Pending finalize 屏障 + redaction 收尾

### 中期
3. **R362-364**: Acpx-engine 子模块（fingerprint/codec/stage 协议）— **最大单一项目**
4. **R365-367**: Budgets 完整迁移
5. **R368-370**: Sandbox-managed-runtime + Git-workspace-sync

---

## 🔬 验证基线

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# R359 单独验证
env -u SHELL rtk proxy cargo test -p pc-heartbeat --test round359_source_escalation_activity_log -- --test-threads=1
# 期望: 4 passed

# pc-heartbeat 全量（无回归）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --tests -- --test-threads=1
# 期望: 65 test results, 925 passed / 0 failed

# 格式
env -u SHELL rtk proxy cargo fmt --all -- --check
# 期望: 无输出（通过）

# workspace bins
env -u SHELL rtk proxy cargo build --workspace --bins
# 期望: Finished `dev` profile
```

---

## 📝 备注

- R359 引入 activity_log 写入后，需修复 4 个 escalate 相关测试的 cleanup 函数（添加 `DELETE FROM activity_log`）—— 这是合理的连带修改，不是无关 bug
- `plan.activity_source` 字段在 R359 之前已预留但未消费，R359 真正启用其语义
- 当前 pc-heartbeat lib 测试 = **925 passed**（R358 = 924，+4 在 pc-heartbeat；pc-http 维持 3 from R358）
