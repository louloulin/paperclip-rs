# R744 — pc-decisions/lifecycle_pure 纯函数模块

## 目标

补足 Node `paperclip/server/src/services/decisions.ts` 中 lifecycle 相关方法的 pure helpers：
- `resumeDecision` / `deliverContinuation` / `sweepExpired` / `decide` replay 校验

把核心判断拆成不依赖 DB / signing / wakeup 的纯函数，与 `bundle_validation_pure` /
`wakeup_validation_pure` 同级。

## 新增内容

### `crates/pc-decisions/src/lifecycle_pure.rs`（新增 25.7 KB / 45 单测）

#### 公开 API

| 函数 / 类型 | 用途 | 对齐 Node |
|---|---|---|
| `should_resume_decision(execution_status)` | execution_status == "running" → 重新跑 effects | `resumeDecision` |
| `is_decision_expired(status, expires_at, now)` | status == "open" && expires_at <= now | `sweepExpired` TTL 分支 |
| `extract_continuation_pending(metadata)` | 读 `metadata.continuationPending` bool | `decide` / `deliverContinuation` |
| `is_pending_continuation(policy, metadata)` | wake_origin_agent + continuationPending | 同上 |
| `should_dispatch_continuation(status, exec, policy, meta)` | 仅在终态触发 continuation | `deliverContinuation` 入口 |
| `continuation_outcome_for(status, exec)` | status → "decided"/"expired"/"cancelled" | `deliverContinuation` outcome 参数 |
| `parse_sweep_batch_size(raw, default)` | Number.isFinite + Math.max(1, trunc) | `sweepExpired` 配置 |
| `parse_recovery_grace_ms(raw, default)` | isFinite + >= 0 | `sweepExpired` 配置 |
| `ExpirationReason` enum + `expiration_reason_for(...)` | "target_gone" / "ttl" 决策 | `sweepExpired` 过期原因 |
| `next_target_sweep_cursor(rows, expected_count)` | 满 batch 推进 / 否则重置 null | `sweepExpired` cursor 推进 |
| `is_after_cursor(decision_id, cursor)` | decision_id > cursor | `sweepExpired` cursor 比较 |
| `merge_unique_ids(ttl, target)` | ttl 在前 + 去重 | `new Map(...).values()` |
| `merge_continuation_metadata(meta, delivered_at)` | 写 deliveredAt + 清 pending | `deliverContinuation` |
| `merge_expired_metadata(meta, reason, cont_pending)` | 写 expiredReason + 可选 pending | `sweepExpired` mark expired |
| `merge_decided_metadata(meta, key, dismissed, reason, cont_pending)` | 写 idempotencyKey + dismissed | `decide` claimed update |
| `DecideReplay` enum + `detect_decide_replay(...)` | IdempotentReplay / OptionReplay / NotReplay | `decide` replay 校验 |
| `InputValidationError` enum + `validate_decision_inputs(fields, values)` | required + maxLength | `decide` inputs 内联校验 |

#### 设计要点

1. **零 DB**：所有函数只消费 row 字段（status / execution_status / expires_at /
   continuation_policy / metadata / chosen_option_id / idempotency_key / inputs），
   不依赖 sqlx / signing / wakeup。
2. **与现有 pure 模块对齐命名风格**：与 `bundle_validation_pure` /
   `wakeup_validation_pure` 同样使用 `#[forbid(unsafe_code)]` + 模块顶部 doc。
3. **错误类型用 enum**：InputValidationError / ExpirationReason / DecideReplay 都
   是 enum，调用方 `match` 显式处理分支。
4. **serde_json::Value 操作走 as_object 链**：避免 panic，遇到 None / 缺失字段给
   出合理默认值。
5. **f64 路径解析数字**：与 Node `Number(...)` + `isFinite()` 行为对齐，
   `parse_sweep_batch_size("3.7")` → 3（Math.trunc）。
6. **tests 全部命名 `r744_*`**：方便回归检索。

## 验证

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs
cargo test -p pc-decisions --lib lifecycle_pure
```

结果：

```
test result: ok. 45 passed; 0 failed; 0 ignored; 0 measured; 108 filtered out
```

```bash
cargo test -p pc-decisions --lib
```

结果：

```
test result: ok. 153 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 与 Node parity

| 行为 | Node | Rust | 一致 |
|---|---|---|---|
| status == "running" 才 resume | ✓ | ✓ | ✅ |
| expires_at <= now → ttl | ✓ | ✓ | ✅ |
| continuationPolicy + continuationPending 触发 | ✓ | ✓ | ✅ |
| 仅在终态投递 continuation | ✓ | ✓ | ✅ |
| Number.isFinite + Math.max(1, trunc) batch_size | ✓ | ✓ | ✅ |
| isFinite + >= 0 grace_ms | ✓ | ✓ | ✅ |
| target_gone / ttl 二选一 | ✓ | ✓ | ✅ |
| 满 batch → cursor 推进 | ✓ | ✓ | ✅ |
| ttl + target id 去重合并 | ✓ | ✓ | ✅ |
| 已 decided + 同人 + option 匹配 → replay | ✓ | ✓ | ✅ |
| 已 decided + 同人 + idempotencyKey 命中 → replay | ✓ | ✓ | ✅ |
| input required + maxLength | ✓ | ✓ | ✅ |

## 累计

| 项 | 之前 | R744 后 |
|---|---:|---:|
| pc-decisions lib tests | 108 | **153** |
| pc-decisions R744 新增 | — | **+45** |
| 累计 R712-R744 新增 | 261 | **+45 = 306 PASS** |
| 累计新代码行数 | ~9000 | **~9500** |

## 后续

- **R745** — pc-routines/attention 服务层补足
- **R746** — pc-routines/service.rs DB 服务层补足
- **R747** — pc-tool/service.rs DB 服务层补足
