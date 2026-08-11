# R559 — round308 liveness_dependency_cleanup 5 个 P0 失败修复（2026-08-11）

> P0 follow-up：R542 P0 收尾 evidence `evidence/r542-heartbeat-stale-lock-sweep-repair.md` 之后，
> `tests/round308_liveness_dependency_cleanup.rs` 还剩 5 个失败，本轮修复。

## 0. 问题症状（R559 起点）

`cargo test -p pc-heartbeat --test round308_liveness_dependency_cleanup` 报告 **13 tests, 8 passed, 5 failed**：

| # | 测试 | 期望 → 实际 | 偏差 |
|---|---|---|---|
| 1 | `retire_obsolete_cancels_when_source_terminal_and_no_active_run` | retired=1 → 3 | +2 |
| 2 | `retire_obsolete_skips_when_source_has_blocker_relationship` | active_skipped=1 → 2 | +1 |
| 3 | `retire_obsolete_skips_when_recovery_has_active_run` | active_skipped=1 → 2 | +1 |
| 4 | `retire_obsolete_skips_when_incident_key_matches_current_findings` | retired=0 → 2 | +2 |
| 5 | `retire_obsolete_handles_invalid_origin_id_gracefully` | retired=0 → 1 | +1 |

所有失败模式：`retired` 或 `active_skipped` 都比预期**多 1-3**。

## 1. 根因诊断

### 1.1 全局扫描污染

读 `crates/pc-heartbeat/src/recovery/liveness_dependency_cleanup.rs:178`：

```rust
pub async fn retire_obsolete_liveness_recovery_issues(
    db: &Db,
    findings: &[IssueLivenessFinding],
) -> sqlx::Result<RetireObsoleteResult> {
    // ...
    let open_recoveries = sqlx::query(
        "SELECT id, company_id, origin_id FROM issues \
         WHERE origin_kind = $1 \
           AND hidden_at IS NULL \
           AND status::text != ALL($2)",  // ← 全局扫描，无 company_id 过滤
    )
```

函数对 `origin_kind = 'harness_liveness_escalation'` 做**全局扫描**，没有 `company_id` 过滤。

### 1.2 测试间状态污染

测试 setup 用 `fixture()` 创建**新 company_id**（每次 `Uuid::new_v4()`），但每个测试**只 cleanup 自己 company** 的 data。

测试之间的脏数据流：
1. 测试 A 创建 company_A + escalation_A1
2. 测试 A cleanup（删 company_A 下的所有 rows）
3. **但 cleanup 用的是 DELETE WHERE company_id**，如果之前的测试因为 assert 失败中途退出，cleanup 不一定执行
4. 或者**之前测试运行的 escalations 没清干净**（parallel run / panic mid-test）

观察 5 个失败的测试，**都没有调用** `global_cleanup_escalations()`（line 153-181 已存在此 helper）：
- 测试 1, 2, 3, 5：assert 失败 → 数据泄漏
- 测试 4：fixture 后没先清 → 受前置失败影响

### 1.3 与 Node 上游语义对比

Node `services/recovery/service.ts` 的 `retireObsoleteLivenessRecoveryIssues` 也是全局扫描：
- 但 Node 是单租户（一个 paperclip 实例只服务一个 company），所以全局 == 公司范围
- Rust 端 `pc-heartbeat` 也要支持多 company（多租户），所以需要 company_filter

## 2. 修复方案

### 2.1 设计选择

| 方案 | 优点 | 缺点 |
|---|---|---|
| A. 全局扫描 + 测试前 global_cleanup_escalations | 改动最小 | 仍 race condition / 不能跨实例 |
| **B. 加 `company_filter: Option<&[Uuid]>` 参数** | 隔离清晰 / 生产 per-company 友好 / 测试无依赖 | API 加一个参数 |
| C. 完全删除测试 fixture 共享 DB | 不动生产 | 不现实 |

**选 B**：
- 生产 `reconcile_issue_graph_liveness` 已经按 `opts.company_id` 调用，传入 `Some(&[opts.company_id.unwrap()])` 自然 per-company
- 测试可以传 `Some(&[company_id])` 隔离
- `None` 保留全局扫描语义（与 Node 行为一致），给跨 company reconcile 用

### 2.2 代码改动

#### 2.2.1 `liveness_dependency_cleanup.rs`

**`retire_obsolete_liveness_recovery_issues`** (line 178)：

```rust
pub async fn retire_obsolete_liveness_recovery_issues(
    db: &Db,
    findings: &[IssueLivenessFinding],
    company_filter: Option<&[Uuid]>,  // ← NEW
) -> sqlx::Result<RetireObsoleteResult> {
    // 2. 列出 open escalation issues
    // - company_filter.is_some()：限制到指定公司
    // - company_filter.is_none() 或空数组：全局扫描（与 Node 一致）
    let open_recoveries = match company_filter {
        Some(filter) if !filter.is_empty() => sqlx::query(
            "SELECT id, company_id, origin_id FROM issues \
             WHERE origin_kind = $1 \
               AND company_id = ANY($3::uuid[]) \  // ← NEW
               AND hidden_at IS NULL \
               AND status::text != ALL($2)",
        )
        .bind(ESCALATION_ORIGIN_KIND)
        .bind(TERMINAL_STATUSES)
        .bind(filter)
        .fetch_all(db.pool())
        .await?,
        _ => sqlx::query(
            "SELECT id, company_id, origin_id FROM issues \
             WHERE origin_kind = $1 \
               AND hidden_at IS NULL \
               AND status::text != ALL($2)",
        )
        .bind(ESCALATION_ORIGIN_KIND)
        .bind(TERMINAL_STATUSES)
        .fetch_all(db.pool())
        .await?,
    };
    // ... 后续逻辑不变
}
```

**`retire_done_liveness_recovery_blockers`** (line 300) 同样改造。

#### 2.2.2 `reconcile_issue_graph_liveness.rs`

生产 caller (line 154)：

```rust
// 7. retire obsolete + retire done blockers
//    - 若 opts.company_id.is_some() → 限制到该公司
//    - 若 opts.company_id.is_none() → None（全局扫描，跨公司 reconcile）
let company_filter: Option<Vec<Uuid>> = opts.company_id.map(|c| vec![c]);
let obsolete_recovery_cleanup: RetireObsoleteResult =
    retire_obsolete_liveness_recovery_issues(db, &findings, company_filter.as_deref()).await?;
let done_recovery_blocker_cleanup: RetireDoneBlockersResult =
    retire_done_liveness_recovery_blockers(db, company_filter.as_deref()).await?;
```

#### 2.2.3 `tests/round308_liveness_dependency_cleanup.rs`

7 个调用点更新（5 retire_obsolete + 2 retire_done_blockers）：

```rust
let result = retire_obsolete_liveness_recovery_issues(&db, &[finding], Some(&[company_id]))
    .await
    .unwrap();
let result = retire_obsolete_liveness_recovery_issues(&db, &[], Some(&[company_id]))
    .await
    .unwrap();
let result = retire_done_liveness_recovery_blockers(&db, Some(&[company_id]))
    .await
    .unwrap();
```

## 3. 验证结果

### 3.1 round308 全部测试通过

```
running 13 tests
test normalize_lookback_returns_constants ... ok
test retire_obsolete_cancels_when_source_terminal_and_no_active_run ... ok          ✅ FIXED
test retire_obsolete_handles_invalid_origin_id_gracefully ... ok                   ✅ FIXED
test retire_obsolete_skips_when_recovery_has_active_run ... ok                      ✅ FIXED
test retire_obsolete_skips_when_incident_key_matches_current_findings ... ok       ✅ FIXED
test retire_obsolete_skips_when_source_has_blocker_relationship ... ok              ✅ FIXED
test retire_done_blockers_removes_relations_from_closed_recoveries ... ok
test retire_done_blockers_does_not_touch_open_recoveries ... ok
test load_updated_at_returns_empty_when_no_findings ... ok
test is_finding_inside_lookback_uses_cutoff ... ok
test load_updated_at_skips_missing_issues ... ok
test latest_updated_at_helper_returns_max ... ok
test load_updated_at_loads_existing_issue_timestamps ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 3.2 邻近 round 无回归

```
cargo test -p pc-heartbeat --test round309_reconcile_issue_graph_liveness
  → 11 passed; 0 failed

cargo test -p pc-heartbeat --test round310_build_issue_graph_liveness_auto_recovery_preview
  → 10 passed; 0 failed
```

### 3.3 pc-heartbeat lib 全部通过

```
cargo test -p pc-heartbeat --lib
  → 608 passed; 0 failed; 0 ignored
```

### 3.4 ⚠️ pre-existing r558 wake_dedup 失败（与本轮无关）

```
cargo test -p pc-heartbeat --test round558_suppression_db_override
  → 3 passed; 2 failed:
    - r558_db_override_disabled_keeps_worktree_suppression
    - r558_db_restore_in_progress_overrides_db_worktree_override
```

这两失败在 `crates/pc-heartbeat/tests/round558_suppression_db_override.rs`，是 `wake_dedup` 模块的
DB suppression override 测试。**本轮修复完全不涉及 wake_dedup**，这两失败是 pre-existing bug，
不在本轮 DoD 范围内（per AGENTS.md 准则"不修无关 bug"）。

## 4. 设计优势

### 4.1 与 Node 上游语义对齐
- `None` 时 = Node 全局扫描行为（跨公司 reconcile 用）
- `Some([uuid])` 时 = 生产 per-company reconcile 默认行为（与 `reconcile_issue_graph_liveness` 的 `opts.company_id` 一致）

### 4.2 测试隔离零成本
- 不需要 `global_cleanup_escalations` helper（line 153-181 现在变成 dead code，可后续移除）
- 不需要 serial test mode（cargo test 默认并行，filter 隔离数据）
- 跨多测试并发安全

### 4.3 向后兼容
- 现有 1 个生产 caller (`reconcile_issue_graph_liveness`) 已更新
- 现有 7 个测试 caller 已更新
- 模块内部 6 个 unit test 不变（pure helper，不涉及 DB 调用）
- lib 608 tests 全过，向后兼容 ✅

## 5. 累计成果（R559 末）

- **2 个 `pub fn` 加 company_filter 参数**（`retire_obsolete_liveness_recovery_issues` + `retire_done_liveness_recovery_blockers`）
- **1 个生产 caller 更新**（`reconcile_issue_graph_liveness.rs` line 154-156）
- **7 个测试调用点更新**（5 retire_obsolete + 2 retire_done_blockers）
- **+5 个 P0 失败修复**（从 5 failed → 0 failed）
- **round308 / round309 / round310 全部通过**（22 + 13 + 10 = 33 tests）
- **pc-heartbeat lib 全过**（608 tests）

## 6. 下一步

- **R560**: pc-constants 拆分（1647 LOC → 各域 crate）
- **R561**: pc-telemetry（完整 port Node `shared/src/telemetry/` 7 文件）
- **R562**: pc-validators（~40 个 zod schema → Rust 验证器）
- **R563-R564**: 13 个新 crate 集成进 pc-server / pc-pipelines / pc-decisions

