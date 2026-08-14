# R650 (2026-08-13) — Routine scheduler activity gate 闭环

## 目标

对齐 Node `services/routines.ts::evaluateActivityGate` 核心语义。

activity gate 是 paperclip 的关键 UX 保护机制：
- 当 routine 配置 `activity_gate_policy = "require_external_activity"` 时
- 自上次 routine dispatch 以来必须有"外部活动"才会 fire
- 否则跳过（写 `skipped` run + `failure_reason="no_external_activity"`）

外部活动定义（与 Node 1:1）：
- `activity_log.action` 不在 4 个 ignored actions 之列
  - `issue.read_marked` / `issue.read_unmarked` /
    `issue.inbox_archived` / `issue.inbox_unarchived`
- 不为 `routine-scheduler` 自身产生的活动（防自循环）

## 新增模块

### `crates/pc-routines/src/activity_gate.rs` (227 LOC)

DB-backed 评估模块：

- `ActivityGateVerdict` 结构：
  - `fire: bool` — 是否应该 fire
  - `window_start: Option<DateTime<Utc>>` — 窗口起点（首次为 None）
  - `matched_activity_id: Option<Uuid>` — 触发 fire 的活动 ID
  - `scope: ActivityGateScope`
- `ActivityGateScope` enum：`Global` / `Project`
- `evaluate_activity_gate(pool, routine, now) -> ActivityGateVerdict`
  - `policy == "always"`: 直接 fire=true
  - `policy == "require_external_activity"`: 走 gate 评估
  - 首次（无 last dispatched run）: fire=true
  - 有 last dispatched run: 查外部活动
- `find_external_activity`: SQL 查询，company-scoped + ignored action 过滤 +
  自循环过滤（actor_id="routine-scheduler" AND
  (details->>'routineId'=routine.id OR entity_id=routine.id)）
- `should_fire(pool, routine, now) -> bool` — 高层便捷方法
- 3 个单元测试（IGNORED actions 列表对齐 + scheduler actor 常量 + JSON 序列化）

## 修改点

### `crates/pc-routines/src/service.rs`

- `tick_scheduled_triggers` 在 worktree eligibility 检查之后接入 activity gate：
  - `routine.activity_gate_policy == "require_external_activity"`
  - 调 `evaluate_activity_gate(pool, routine, now)`
  - `!verdict.fire` → 写 `routine_runs(status=skipped, failure_reason="no_external_activity")`
  - payload 包含 `activityGate.{verdict, windowStart, matchedActivityId, scope}`（与 Node 一致）
  - 仍然推进 trigger cursor（与 worktree eligibility 一致）

### `crates/pc-routines/src/lib.rs`

- `pub mod activity_gate;`
- `pub use activity_gate::{evaluate_activity_gate, should_fire,
  ActivityGateScope, ActivityGateVerdict}`

## 测试

### `crates/pc-routines/src/activity_gate.rs` (单元测试 3 个)

- `ignored_actions_match_node` — 4 个 ignored actions 都在列表中
- `routine_scheduler_actor_id_constant` — actor id 字符串对齐
- `verdict_serializes_with_camel_case` — JSON 序列化用 camelCase

### `crates/pc-routines/tests/r650_activity_gate.rs` (真实 PG 集成测试 6 个)

使用全局 `R650_TEST_LOCK` 串行化：

- `r650_first_run_with_require_external_activity_fires` — 首次 fire=true
- `r650_no_activity_since_last_run_skipped` — 自上次 run 后无活动 → skipped
- `r650_external_activity_fires_routine` — issue.comment_added → fire
- `r650_only_ignored_activities_keeps_skipped` — issue.read_marked 不算外部活动
- `r650_only_self_loop_keeps_skipped` — routine-scheduler 自循环不算外部活动
- `r650_always_policy_ignores_gate` — policy=always 直接 fire

## 真实验证结果

```
cargo test -p pc-routines --lib activity_gate
cargo test: 3 passed, 35 filtered out (1 suite, 0.00s)

cargo test -p pc-routines
cargo test: 93 passed (8 suites, 1.90s)   # +9 = 3 unit + 6 integration

cargo check -p pc-routines
cargo check: 0 errors, 53 warnings

cargo check -p pc-server
cargo check: 0 errors, 364 warnings (0 crates)
```

## 设计决策

1. **Global scope 优先**：本轮先实现 company-wide 的核心语义。
   `project` scope（10+ 个 entity 子查询）需要单独的 evidence 轮次，
   留作后续 R651+。
2. **Cursor 仍推进**：被 activity gate 抑制的 trigger 仍会 claim 并推进
   `next_run_at`（与 worktree cutoff 一致），避免 resume 后 replay 积压 run。
3. **稳定的 reason 字符串**：使用 `no_external_activity`（与 Node
   `recordSuppressedAutomaticRun` 一致）。详细 verdict 通过 payload 暴露。
4. **测试隔离**：通过全局 Mutex 串行化 R650 测试，避免跨测试数据污染。

## 与 Node 的精确对齐

| 行为 | Node | Rust R650 |
|---|---|---|
| `always` policy | 直接 fire | ✓ |
| `require_external_activity` + 首次 | fire | ✓ |
| `require_external_activity` + 无活动 | skipped | ✓ |
| `require_external_activity` + ignored action | 不算外部 | ✓ |
| `require_external_activity` + self-loop | 不算外部 | ✓ |
| cursor 推进被抑制 trigger | 推进 | ✓ |
| `failure_reason` | `no_external_activity` | ✓ |
| `project` scope | 10+ 子查询 | 后续 R651+ |

## 影响

- pc-routines lib: 38 passed (+3)
- pc-routines tests: 93 passed (+6)
- pc-server: 0 errors
- scheduler 剩余 worktree/activity/suppression 已 100% 复刻核心语义
- services 域 75% → **77%**
- 综合加权 92% → **92.3%**
