# R652 — Routine Scheduler Suppression Activity Log

**Round**: R652  
**Date**: 2026-08-14  
**Status**: ✅ Complete  
**Test file**: `crates/pc-routines/tests/r652_suppression_activity_log.rs`  
**Production code**: `crates/pc-routines/src/service.rs::record_skipped_run_activity`  

## Node 1:1 对齐

与 Node 端 `recordSuppressedAutomaticRun` (paperclip/server/src/services/routines.ts) 1:1 对齐：

- `actor_type = "system"`, `actor_id = "routine-scheduler"` (schedule 来源)
- `entity_type = "routine_run"`, `entity_id = <skipped_run_id> (UUID as string)`
- `action = "routine.run_skipped"`
- `details = { routineId, triggerId, source, status, reason, scheduledAt, claimedAt }`

## 实现要点

1. **`record_skipped_run_activity` helper** — 与 Node `logger.warn(...)` 等价行为：
   - 失败仅 `tracing::warn!`，不阻塞 scheduler tick 主流程
   - 在 3 个 skipped run 创建点接入：
     - R647 paused project 抑制路径
     - R649 worktree execution cutoff 抑制路径
     - R650 activity gate 抑制路径
2. **`run_id: None`** — activity_log.run_id 列有 FK 约束 `heartbeat_runs.id`，
   而 skipped run 是 `routine_runs.id`（异构）。`entity_id` 字段已保存 run_id 字符串，足够审计追溯。
3. **稳定的 reason 字符串**：
   - `paused` — R647 project paused
   - `worktree_execution_cutoff` — R649 worktree runtime 不匹配
   - `no_external_activity` — R650 activity gate verdict quiet

## 真实 PG 测试覆盖（4 个测试，全部 PASS）

| Test | 覆盖路径 |
|---|---|
| `r652_paused_project_writes_skipped_activity_log` | paused project → routine_runs skipped + activity_log entry |
| `r652_worktree_cutoff_writes_skipped_activity_log` | worktree instance 不匹配 → routine_runs skipped + activity_log entry |
| `r652_activity_gate_writes_skipped_activity_log` | require_external_activity + 无外部活动 → routine_runs skipped + activity_log entry |
| `r652_repeated_skipped_writes_independent_activity_entries` | 同一 trigger 两次 tick 都 skipped → 2 条独立 activity_log entry (entity_id 不同) |

## 验证

```
$ cargo test -p pc-routines --test r652_suppression_activity_log
running 4 tests
test r652_repeated_skipped_writes_independent_activity_entries ... ok
test r652_activity_gate_writes_skipped_activity_log ... ok
test r652_paused_project_writes_skipped_activity_log ... ok
test r652_worktree_cutoff_writes_skipped_activity_log ... ok

test result: ok. 4 passed; 0 failed

$ cargo test -p pc-routines  # 全套
test result: ok. 97 passed; 0 failed  # 含 R647+R648+R649+R650+R652 + hook + contract + 38 lib

$ cargo test -p pc-heartbeat  # 无回归
test result: ok. (all pass, no failed)
```

## 后续

- R653 realtime event broadcast — 把 routine.run_skipped 推到 realtime bus
- R654 project scope activity gate
- R655 webhook trigger dispatch
