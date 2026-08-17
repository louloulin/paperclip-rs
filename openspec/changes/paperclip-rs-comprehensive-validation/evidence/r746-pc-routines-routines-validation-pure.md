# R746 — pc-routines/routines_validation_pure 纯函数模块

## 目标

把 `pc-routines/src/service.rs` 中 `CreateRoutine::normalize` /
`RoutinePatch::validate` / `CreateRoutineTrigger::validate` 的策略规则
抽到独立 `routines_validation_pure` 模块，使 service 层的策略可独立单测。

## 新增内容

### `crates/pc-routines/src/routines_validation_pure.rs`（新增 16.7 KB / 41 单测）

#### 公开 API

| 函数 / 常量 | 用途 | 对齐 Node |
|---|---|---|
| `ALLOWED_PRIORITIES` | low/medium/high/urgent | `routineCreateSchema` enum |
| `ALLOWED_STATUSES` | draft/active/paused/archived | `routineCreateSchema` enum |
| `ALLOWED_CONCURRENCY` | allow/skip/queue | 同上 |
| `ALLOWED_CATCHUP` | skip_missed / enqueue_missed_with_cap | 同上 |
| `ALLOWED_ACTIVITY_GATE` | always / require_external_activity | 同上 |
| `ALLOWED_TRIGGER_KINDS` | schedule / webhook | `triggerKind` enum |
| `DEFAULT_*` 常量（priority/status/concurrency/catchup/activity_gate/scope/tz）| `unwrap_or(else "...")` 默认值 | service 内联 |
| `is_*_allowed(value)` | 谓词 | service 内联 |
| `default_*(input)` | 默认值回退 | service 内联 |
| `validate_priority/status/concurrency/catchup/activity_gate/trigger_kind` | 字符串校验 | service 内联 |
| `validate_title_non_empty(title)` | trim + 非空 | `routineCreateSchema` |
| `validate_company_id_not_nil(id)` | uuid nil 守门 | `companyId required` |
| `validate_trigger_schedule_inputs(cron, tz)` | schedule 必填 cron | `CreateRoutineTrigger::validate` |
| `validate_trigger_webhook_inputs(cron)` | webhook 不能含 cron | 同上 |
| `validate_trigger_patch_cron/timezone` | patch Some(non-empty)/Some("")/None 三态 | `UpdateRoutineTrigger::validate` |
| `normalize_trigger_schedule(kind, cron, tz)` | schedule: 保留 + 默认 tz; webhook: 清掉 | `CreateRoutineTrigger::into_record` |

#### 设计要点

1. **零 DB**：所有函数只消费字符串 / Uuid / Option，不依赖 sqlx / secrets / hooks。
2. **不引入 pc_errors**：返回 `Result<(), &'static str>`，调用方在 service 层
   包成 `validation()` / `unprocessable()`。这样纯模块可在任何 context 复用。
3. **谓词 + 默认值 + 校验 三段**：每类字段都有 `is_*_allowed` / `default_*` /
   `validate_*` 三个 helper，与 service 内联逻辑对齐。
4. **Some/Some("")/None 三态语义**：patch 路径下 `Some(Some(""))` 表示"清空并设
   为空"，需要拒绝；`Some(Some("non-empty"))` 表示"更新"；`None` 表示"不动"。
5. **tests 全部命名 `r746_*`**。

## 验证

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs
cargo test -p pc-routines --lib routines_validation_pure
```

结果：

```
test result: ok. 41 passed; 0 failed; 0 ignored; 0 measured; 123 filtered out
```

```bash
cargo test -p pc-routines --lib
```

结果：

```
test result: ok. 164 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 与 Node parity

| 行为 | Node | Rust | 一致 |
|---|---|---|---|
| priority 枚举：low/medium/high/urgent | ✓ | ✓ | ✅ |
| status 枚举：draft/active/paused/archived | ✓ | ✓ | ✅ |
| concurrency 枚举：allow/skip/queue | ✓ | ✓ | ✅ |
| catchUp 枚举：skip_missed / enqueue_missed_with_cap | ✓ | ✓ | ✅ |
| activityGate 枚举：always / require_external_activity | ✓ | ✓ | ✅ |
| trigger.kind 枚举：schedule / webhook | ✓ | ✓ | ✅ |
| 默认值（priority=medium, status=active, ...） | ✓ | ✓ | ✅ |
| title 非空 | ✓ | ✓ | ✅ |
| companyId 不为 nil | ✓ | ✓ | ✅ |
| schedule trigger 必须 cron | ✓ | ✓ | ✅ |
| webhook trigger 不能 cron | ✓ | ✓ | ✅ |
| timezone 默认 UTC | ✓ | ✓ | ✅ |

## 累计

| 项 | 之前 | R746 后 |
|---|---:|---:|
| pc-routines lib tests | 123 | **164** |
| pc-routines R746 新增 | — | **+41** |
| 累计 R712-R746 新增 | 331 | **+41 = 372 PASS** |
| 累计新代码行数 | ~10000 | **~10500** |

## 后续

- **R747** — pc-tool/service.rs DB 服务层补足
- **R748** — pc-feedback/redaction 服务层补足
- **R749** — pc-companies/search_rate_limit 补足
- **R750** — pc-routines/activity_gate pure helper 抽取
