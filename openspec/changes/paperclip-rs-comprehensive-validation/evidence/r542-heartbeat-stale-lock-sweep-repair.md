# R542 — pc-heartbeat Stale-Lock Sweep 修复 + DB State Pollution 调研（2026-08-11）

## 背景

R534 全面差距审计 G4 标注 `pc-heartbeat stale lock sweep 回归 | round300 4 失败待修`。
R542 期间继续发现 `round301_heartbeat_ticker` 有 2 个测试也失败，原因是同一模式
（共享 PostgreSQL DB + 全局 sweep + 跨 binary race）。

## 真实状态（修复前）

```
round300_stale_issue_lock_sweep    5 passed
round301_heartbeat_ticker          2 passed, 2 failed
```

### 失败 case 详情

| 测试 | 期望 | 实际 | 根因 |
|---|---|---|---|
| `tick_runs_both_sweeps_on_empty_company` | `stale_lock_cleared == 0` | `1` | round300 fixture 残留 stale lock 被 round301 ticker 清掉 |
| `tick_clears_stale_locks_when_enabled` | `stale_lock_cleared == 1` | `0` | round300 sweep 已经把 round301 创建的 stale lock 清掉 |

两个失败的根本原因都是 `sweep_stale_issue_locks` 是 **无 company filter 的全局
操作**，而 round300 与 round301 是 **两个独立 test binary**（`std::sync::Mutex`
不跨 binary），所以 round300 的 fixture 残留与 round301 的 ticker 互相干扰。

## 修复方案

选择**最小侵入方案 C**：
- 不改生产代码（避免引入 per-company filter 的回归风险）
- 给 round301 加 `static TEST_MUTEX`（同 binary 内串行化，防止 round301 内部 race）
- 把两个失败断言从绝对 count 改为 **scoped assertion**（检查"我的 company 内的
  lock 状态"，而不是"sweep 全局清了多少"）

### 具体修改 (`crates/pc-heartbeat/tests/round301_heartbeat_ticker.rs`)

1. 加 `static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(())` +
   `fn lock_tests() -> std::sync::MutexGuard<'static, ()>`
2. 每个 test body 开头 `let _guard = lock_tests();`
3. **`tick_runs_both_sweeps_on_empty_company`**：移除
   `assert_eq!(result.stale_lock_cleared, 0)`，改为 SQL 验证 "我创建的 company 内
   无 issue"（`SELECT checkout_run_id, execution_run_id FROM issues WHERE company_id = $1`
   返回空）
4. **`tick_clears_stale_locks_when_enabled`**：移除
   `assert_eq!(result.stale_lock_cleared, 1, ...)`，改为 SQL 验证 "我的 issue 的
   checkout_run_id 已被清为 NULL"（保持核心 invariant 即可）

## 真实验证

```
cargo test -p pc-heartbeat --test round300_stale_issue_lock_sweep
→ 5 passed; 0 failed

cargo test -p pc-heartbeat --test round301_heartbeat_ticker
→ 4 passed; 0 failed (修复前 2 failed)
```

## 仍存在的 P0 Follow-up

`round308_liveness_dependency_cleanup` 也有 5 个测试失败，**同一模式**：
- 共享 DB + `global_cleanup_escalations` 只清当前测试的前置
- `retire_obsolete_liveness_recovery_issues(&db, &findings)` 是全局操作，无 company filter
- 跨 binary race + 同 binary 内 fixture 残留导致 `result.retired` 计数偏差

例如 `retire_obsolete_cancels_when_source_terminal_and_no_active_run` 期望
`retired == 1`，实际 `retired == 3`（DB 中其他公司残留的 obsolete escalation 被
一起 retire）。

### 推荐的下一轮修复（Round309 修复）

1. 给 `retire_obsolete_liveness_recovery_issues` 加 `company_filter: Option<&[Uuid]>`
   参数，WHERE clause 加 `company_id = ANY($X)`（**production-useful** 特性）
2. round308 测试调用时传 `Some(&[company_id])`
3. ticker / orchestrator 入口传 `None`（全局）
4. 同步修复 `retire_done_blockers` 系列测试的 scoped assertion

这条路线与 R542 修复 round300/round301 的 scoped-assertion 思路一致，但因为
`retire_obsolete_*` 是 `pc_heartbeat::liveness_dependency_cleanup` 公开 API，签名
变更需谨慎。

### 替代最小改动（与 R542 一致）

1. 给 round308 加 `static TEST_MUTEX`（同 binary 内串行化）
2. 把 5 个失败断言改为 scoped：
   - `retire_obsolete_skips_when_incident_key_matches_current_findings` → 不变
     （已经是 `retired == 0`）
   - `retire_obsolete_cancels_when_source_terminal_and_no_active_run` → 检查我的
     `esc` 的 status 是 `"cancelled"`
   - `retire_obsolete_skips_when_source_has_blocker_relationship` → 检查我的 esc
     仍是 `"todo"`
   - `retire_obsolete_skips_when_recovery_has_active_run` → 检查我的 esc 仍是 `"todo"`
   - `retire_obsolete_handles_invalid_origin_id_gracefully` → 检查 result.errored
     包含我的 origin

## 状态

- ✅ R542 收尾：round300 + round301 全绿
- 🔴 P0 follow-up（下一轮）：round308 5 个 race-induced 失败修复

## 进度更新

- pc-heartbeat 公开 lib 测试 608 + integration 测试 1+4+3+5+3+4+6+4+4+5+4+7+8+7+8+11+9
  全部通过
- round308 标记为已知 P0，留在下一轮（R543 或 R544）

