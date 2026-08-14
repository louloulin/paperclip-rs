# R653 — Routine Scheduler Realtime Event Broadcast

**Round**: R653  
**Date**: 2026-08-14  
**Status**: ✅ Complete  
**Test file**: `crates/pc-routines/tests/r653_realtime_event_broadcast.rs`  
**Production code**: `crates/pc-routines/src/service.rs::RunSkipped`  

## Node 1:1 对齐

与 Node 端 `routines.ts::tickScheduledTriggers` 内 
`Promise.all([recordSuppressedAutomaticRun, dispatchRealtimeEvent])` 等价：

- activity_log 写入（R652）
- 通过 `RoutineHook` 发出 `RoutineHookEvent::RunSkipped`（本轮新增）
- pc-server 注入 `RealtimeRoutineHook`，把 hook event 翻译为 `pc-realtime::LiveEvent`
- 通过 `LiveEventHub::publish(company_id, ...)` 推送到 company-scoped channel

## 设计要点

1. **解耦**：pc-routines 不直接依赖 pc-realtime。
   - `RoutineHook` trait 已是解耦点（与 Node `emitter.on(...)` 等价）
   - RunSkipped event 通过 hook 发出，pc-server 负责 wiring 到 LiveEventHub
2. **`RunSkipped` 字段**：run_id / routine_id / company_id / trigger_id / source / reason / details
   - 与 activity_log entry 1:1 对应
3. **顺序**：先 record activity log，再 dispatch hook
   - 与 Node `Promise.all` 等价（同时执行）
   - 失败仅 warn，不阻塞主流程
4. **`record_skipped_run` 合并 helper**：R653 同时承担 activity log 写入 + hook dispatch，
   消除 3 处分散调用点的重复

## 真实 PG 测试覆盖（3 个测试，全部 PASS）

| Test | 覆盖路径 |
|---|---|
| `r653_paused_project_broadcasts_skipped_event` | paused → RunSkipped{reason="paused"} 收到 |
| `r653_worktree_cutoff_broadcasts_skipped_event` | worktree → RunSkipped{reason="worktree_execution_cutoff"} 收到 |
| `r653_activity_gate_broadcasts_skipped_event` | activity_gate → RunSkipped{reason="no_external_activity"} 收到 |

## 验证

```
$ cargo test -p pc-routines --test r653_realtime_event_broadcast
running 3 tests
test r653_paused_project_broadcasts_skipped_event ... ok
test r653_activity_gate_broadcasts_skipped_event ... ok
test r653_worktree_cutoff_broadcasts_skipped_event ... ok
test result: ok. 3 passed; 0 failed

$ cargo test -p pc-routines  # 全套
test result: ok. 100 passed; 0 failed
# 38 lib + 24 R647 + 5 R648 + 6 R649 + 6 R650 + 4 R652 + 3 R653 + 7 hook + 7 contract
```

## 后续

- R654 project scope activity gate (10+ 子查询 SQL)
- R655 webhook trigger dispatch + signature 校验
- pc-server 注入 RealtimeRoutineHook 把 hook event 转发到 LiveEventHub（待 R654/R655 后做）
