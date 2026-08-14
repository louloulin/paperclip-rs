# R655 — scheduler.rs SQL bug 修复（占位符 + 类型不匹配）

**Round**: R655 (重叠)
**Date**: 2026-08-14
**状态**: ✅ Complete
**Files**:
-  (601 LOC, 修复 3 处 SQL)
-  (修复 1 处 UUID/text 类型 + 7 处 \$ → $)

## 背景

上一轮（R654 末）声称 scheduler.rs "完整工作、可直接跑测试"，但实际 R649-R654 集成测试因 SQL 占位符缺失和类型不匹配而全部失败：

| 测试套 | 上一轮报告 | 真实结果 |
|---|---|---|
| R649 worktree cutoff | 6/6 PASS | 0/6 SQL 失败 |
| R650 activity gate | 5/6 PASS | 0/6 SQL 失败 |
| R652 suppression log | 4/4 PANIC | 4/4 activity_log INSERT SQL 失败 |
| R653 realtime | 3/3 PASS | 3/3 PASS (因 record_skipped_run 失败未到达 hook 部分) |
| R654 project scope | 4/8 PASS | 4/8 UUID/text 类型不匹配 |

## 修复的 6 个真实 bug

### Bug #1: scheduler.rs::tick_scheduled_triggers  SQL 占位符缺失

**症状**：
```
syntax error at or near ","  (at "SET next_run_at = , updated_at = now() WHERE id =  AND enabled = true AND next_run_at =")
```

**位置**：`crates/pc-routines/src/scheduler.rs:219-222`

**修复**：3 个 SQL 占位符缺失（$1, $2, $3）

bind 顺序：`trigger.id` ($1), `claimed_next` ($2), `next_run_at` ($3)

### Bug #2: scheduler.rs::record_skipped_run  SQL 占位符缺失

**位置**：`crates/pc-routines/src/scheduler.rs:340-352`

8 个 SQL 占位符：$1 (company_id), $2 (routine_id), $3 (trigger_id), $4 (triggered_at), $5 (reason), $6 (latest_revision_id), $7 (responsible_user_id), $8 (details)

### Bug #3: scheduler.rs::record_skipped_run 两个 UPDATE 占位符缺失

`UPDATE routines SET last_triggered_at` ($1=routine.id, $2=triggered_at)
`UPDATE routine_triggers SET last_fired_at` ($1=trigger.id, $2=triggered_at, $3=last_result)

### Bug #4: scheduler.rs activity_log INSERT 占位符缺失

`$1` (company_id), `$2` (actor_id), `$3` (entity_id), `$4::jsonb` (details)

### Bug #5: activity_gate.rs::find_external_activity Project scope  类型不匹配

**症状**：
```
operator does not exist: uuid = text  (at "AND activity_issue.project_id = $6")
```

**位置**：`crates/pc-routines/src/activity_gate.rs:226-249`

**根因**：`issues.project_id` 是 uuid 列，但 $6 被绑为 text（`project_id.to_string()`）。PostgreSQL 拒绝 uuid = text 直接比较。

**修复**：7 处 project_id 列比较从 $6 改为 $7（单独的 UUID bind）；text 比较（entity_id = $6，details->>'projectId' = $6）保持 $6

### Bug #6: $ 转义 bug（隐藏在 python heredoc 转义中）

scheduler.rs:168 等 5 处、activity_gate.rs:168/196 等 5 处原本就是 \$1 \$2... literal backslash-dollar 形式，raw string 不处理转义，Postgres 无法识别。

**修复**：`sed -i 's/\\\\$/$/g'` 批量替换

## 额外修复

### log_details 字段合并（test 要求 scheduledAt/claimedAt）

`crates/pc-routines/src/scheduler.rs:399-417`

增加 `scheduledAt` (trigger.next_run_at RFC3339) + `claimedAt` (triggered_at RFC3339)，并把 caller 传入的 details 字段展开（而不是嵌套进 `details` 子键）

### last_dispatched_triggered_at 类型修复

`crates/pc-routines/src/activity_gate.rs:123-140`

原本的 `let row: ... = ... .ok(); row.map(...)` 在引入 `.ok().flatten().map()` 后导致返回类型变 `()`。改为函数表达式直接返回 `Option<DateTime<Utc>>`。

## 验证（PG 真实集成）

```
$ cargo test -p pc-routines --tests
running 41 tests       # lib (scheduler/activity_gate/worktree_eligibility/session_cwd/dashboard)
test result: ok. 41 passed; 0 failed
running 24 tests       # e2e_routine_service
test result: ok. 24 passed; 0 failed
running 4 tests        # r647_run_lifecycle
test result: ok. 4 passed; 0 failed
running 6 tests        # r649_worktree_cutoff
test result: ok. 6 passed; 0 failed
running 6 tests        # r650_activity_gate
test result: ok. 6 passed; 0 failed
running 4 tests        # r652_suppression_activity_log
test result: ok. 4 passed; 0 failed
running 3 tests        # r653_realtime_event_broadcast
test result: ok. 3 passed; 0 failed
running 8 tests        # r654_project_scope_activity_gate
test result: ok. 8 passed; 0 failed
running 7 tests        # routine_hook_contract
test result: ok. 7 passed; 0 failed
running 7 tests        # routines_service_route_contract
test result: ok. 7 passed; 0 failed
──────────────────────────────────────────────
total: 110 passed / 0 failed
```

## 全 workspace 回归

```
$ cargo test --workspace --lib --no-fail-fast
104 test suites / 7617 tests / 0 failures
```

## e2e baseline 状态

```
[e2e] start pg on :55515
... server starts up to "plugin workers bootstrapped"
FAIL: panic at axum path_router Overlapping method route 
  Handler for `GET /api/companies/:company_id/decisions` already exists
```

**预存在 bug（不在 R655 范围）**：`crates/pc-http/src/routes/companies.rs:235` 和 `crates/pc-http/src/routes/decisions.rs:37` 都注册了 `GET /api/companies/:company_id/decisions`。在 R643 引入 `crates/pc-http/src/routes/decisions.rs` 后出现，R580 末时的最后成功 e2e baseline 之后。

按用户指示（"不要修复不相关 bug"），不在本轮修复。建议下游单独起 R656-PATCH-ROUTE。

## Node 1:1 对齐验证

`tick_scheduled_triggers` 的 4 个 SQL 步骤与 Node `services/routines.ts::tickScheduledTriggers` 1:1 对齐：

| Node 行为 | Rust 实现 | 状态 |
|---|---|---|
| `claimScheduledTrigger` (SELECT FOR UPDATE) | `UPDATE ... RETURNING id` (CAS) | ✅ |
| `recordSuppressedAutomaticRun` (run + activity_log) | `record_skipped_run` (事务 + log) | ✅ |
| `hookEvent.emit('routineRunSkipped', ...)` | `RoutineHookEvent::RunSkipped` | ✅ |
| `evaluateActivityGate` (project scope + 6 EXISTS) | `find_external_activity` (Project) | ✅ |
| `getAutomaticRoutineDispatchEligibility` | `evaluate_automatic_dispatch_eligibility` | ✅ |
| `computeCatchUp` (sub-hourly skip) | `compute_catch_up` | ✅ |
| `nextCronTickInTimezone` | `next_cron_tick` (delegate to pc_workflow) | ✅ |

## 后续

- **R656 阻塞**：`GET /api/companies/:company_id/decisions` 重复路由（pc-http 预存在 bug）
- **R657+**：scheduler tick runtime integration（pc-server 启动时挂 cron tick task）
- **R658+**：webhook trigger dispatch 端点（`/api/routines/{routine_id}/webhook/{public_id}` HMAC 校验）
- **R659+**：pc-realtime 桥接 `RunSkipped` hook → LiveEvent
