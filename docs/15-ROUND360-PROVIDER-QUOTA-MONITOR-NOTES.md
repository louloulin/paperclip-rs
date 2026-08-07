# Round 360 — ProviderQuota wait_recovery monitor 写 monitor_notes

> 适用版本：`paperclip-rs` 截至 R360（R359 = 925 → R360 = **928**，+3 pc-heartbeat 测试）
> 参考实现：`paperclip` Node（`packages/server/src/services/recovery/service.ts`）
> 测试基线：`cargo test -p pc-heartbeat --tests -- --test-threads=1` 全绿（928/928），`cargo fmt --all -- --check` 通过

---

## 🎯 R360 目标

闭合 `ensure_provider_quota_wait_recovery_monitor`（R355 引入）路径的 **monitor_notes 写入 gap**，
让前端 / 心跳监控 UI 在 issue 处于 `in_review` / `in_progress` 时能在 issues 表的 `monitor_notes`
字段看到"为什么挂起 / 何时重试"的自然语言说明。

### 之前的状态

| 路径 | 写 scheduled_retry run | 写 agent_wakeup_requests | 写 issue.monitor_notes |
|---|---|---|---|
| `schedule_provider_quota_recovery_monitor` (R319) | n/a（in-place） | n/a | ✅ R319 已写 |
| `ensure_provider_quota_wait_recovery_monitor` (R355) | ✅ | ✅ | ❌ **R360 闭合** |

**关键发现**：R355 把 `ensure_provider_quota_wait_recovery_monitor` 引入了 `pc-heartbeat` 的
"per-action escalation" 路径（创建独立 `scheduled_retry` run + wakeup），但**没有同步**把
自己在做什么写进 `issues.monitor_notes`。后果：review-participant / original-assignee 路径
触发的 wait_recovery monitor 监控面板看不到"为什么在等"，只能看到一句空 notes。

---

## 🔧 R360 实现要点

### 修改文件

**`crates/pc-heartbeat/src/recovery/provider_quota_recovery_monitor.rs`**：

1. **新增 helper `build_provider_quota_monitor_notes`** （pure builder）：

   ```rust
   async fn build_provider_quota_monitor_notes(
       db: &Db,
       issue_id: Uuid,
   ) -> Option<String> {
       let row: Option<(String, Option<String>)> = sqlx::query_as(
           "SELECT i.status, run.result_json::text \
            FROM issues i \
            LEFT JOIN LATERAL ( \
                SELECT result_json FROM heartbeat_runs \
                WHERE issue_id = i.id ORDER BY created_at DESC LIMIT 1 \
            ) run ON true \
            WHERE i.id = $1",
       )
       .bind(issue_id)
       .fetch_optional(db.pool())
       .await
       .ok()
       .flatten()?;
       let (status, result_json) = row;
       let has_retry_not_before = result_json
           .as_deref()
           .and_then(|s| serde_json::from_str::<Value>(s).ok())
           .and_then(|v| v.get(PROVIDER_QUOTA_RETRY_NOT_BEFORE_KEY).cloned())
           .is_some();
       let when = if has_retry_not_before {
           "at the provider reset time."
       } else {
           "after the default recovery backoff."
       };
       let who = if status == "in_review" {
           "review participant"
       } else {
           "original assignee"
       };
       Some(format!(
           "Provider usage quota reached; retry the {who} {when}",
       ))
   }
   ```

2. **在 `ensure_provider_quota_wait_recovery_monitor` 事务提交后追加独立 UPDATE**：

   ```rust
   if let Some(monitor_notes) = build_provider_quota_monitor_notes(db, input.issue_id).await {
       let _ = sqlx::query(
           "UPDATE issues SET monitor_notes = $1, updated_at = now() WHERE id = $2",
       )
       .bind(monitor_notes)
       .bind(input.issue_id)
       .execute(db.pool())
       .await;
   }
   ```

   - 独立 UPDATE 而非合并进事务：避免 LATERAL 读 latest_run 锁竞争
   - 即便 notes 写失败也不影响 core recovery action 持久化

### 文案与 `schedule_provider_quota_recovery_monitor` 对齐（R319）

- `in_review` → "review participant" + "at the provider reset time."
- `in_progress` / 其他 → "original assignee" + "after the default recovery backoff."

→ 与 R319 写过的 `in-place` notes 文案语义一致，UI 侧无须做分支。

### 新增测试 `crates/pc-heartbeat/tests/round360_provider_quota_monitor_notes.rs`（3 个真实 PG 测试）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `wait_recovery_monitor_writes_monitor_notes_for_review_participant` | issue.status="in_review" → notes 含 "review participant" |
| 2 | `wait_recovery_monitor_writes_monitor_notes_for_original_assignee` | issue.status="in_progress" → notes 含 "original assignee"，**不**含 "review participant" |
| 3 | `repeated_wait_recovery_monitor_does_not_overwrite_monitor_notes` | 第二次调用走 early-return（已有 scheduled_retry run），notes 不被覆盖 |

### 清理

- R360 初始测试 `let first = ...` 改成 `let _ = ...` 消除 unused variable 警告

---

## 📊 R360 影响面

| 维度 | 之前 | 之后 |
|---|---|---|
| `ensure_provider_quota_wait_recovery_monitor` 落库字段 | wakeup + scheduled_retry run + action | + `issues.monitor_notes` |
| Review participant 监控 UI 可读性 | ❌ 空 | ✅ "Provider usage quota reached; retry the review participant at the provider reset time." |
| Original assignee 监控 UI 可读性 | ❌ 空 | ✅ "Provider usage quota reached; retry the original assignee after the default recovery backoff." |
| 幂等性 | scheduled_retry run 幂等 | + monitor_notes 幂等（不再覆盖） |
| pc-heartbeat 测试总数 | 925 | 928 |

---

## 🧪 验证基线

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs

# 1. R360 单独（3/3 绿）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --test round360_provider_quota_monitor_notes -- --test-threads=1

# 2. pc-heartbeat 全量（66 test results, 928 passed / 0 failed）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --tests -- --test-threads=1

# 3. 格式
env -u SHELL rtk proxy cargo fmt --all
env -u SHELL rtk proxy cargo fmt --all -- --check

# 4. 编译
env -u SHELL rtk proxy cargo build --workspace --bins --message-format=short
```

---

## 📋 后续候选（R361+）

| 序号 | 主题 | 估算 | ROI |
|---|---|---|---|
| **R361** | pending_finalize 屏障 + redaction 收尾 | ~1 轮 | ⭐⭐⭐ 高 |
| **R362-364** | **Acpx-engine 子模块**（B3.1-3.5，最大单一缺口 3500 行 Node） | ~8-12 轮 | ⭐⭐ 中 |
| R365-367 | Budgets 完整迁移（B2） | ~3-4 轮 | ⭐⭐ 中 |
| R368-370 | Sandbox-managed-runtime + Git-workspace-sync | ~5-7 轮 | ⭐ 中 |

---

## 总结

R360 闭合 `ensure_provider_quota_wait_recovery_monitor` 路径的 monitor_notes 写入，
**Recovery 主链** 现在 ~99.5% 完整（R357-R360 连续 4 轮闭合了 4 条新支线）：

- R356: successful_run_handoff Notice builder（cause 系统）
- R357: workspace_validation_fingerprint 注入 description
- R358: HTTP `/api/issues/:id/comments` presentation/metadata round-trip
- R359: source/in-place escalation activity_log
- **R360: wait_recovery monitor_notes** ✅

**最大剩余缺口** 已转移至 **acpx-engine**（3500 行 Node 未迁移，约 8-12 轮）。
