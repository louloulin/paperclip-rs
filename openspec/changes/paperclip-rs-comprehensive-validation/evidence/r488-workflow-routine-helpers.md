# R488 — pc-workflow 深化（routine helpers 三个纯函数）

> 时间：2026-08-11  
> 范围：`crates/pc-workflow/src/schedule.rs` + `crates/pc-workflow/src/routine.rs`  
> 对齐：Node `server/src/services/routines.ts` L67-100（第 67-100 行的几个纯函数）

## 1. 目标

继续 `pc-workflow` 模块深化，把 Node `routines.ts` 中"亚小时级判定 + routine 结果文本
+ webhook 时间戳归一化"三个纯函数 1:1 复刻到 Rust。这是 R487 的延续。

## 2. 实现

### 2.1 `is_sub_hourly_cron_expression`（schedule.rs）

```rust
pub fn is_sub_hourly_cron_expression(
    expression: &str,
    time_zone: &str,
    after: DateTime<Utc>,
) -> bool
```

- 与 Node `routines.ts:67` 1:1 对齐
- 取 24 个后续 tick，若 24h 窗口内能拿到 → true（sub-hourly）
- 用途：routine catch-up 策略 `enqueue_missed_with_cap` 决定单次补跑 vs 窗口扫描

### 2.2 `next_result_text`（routine.rs）

```rust
pub fn next_result_text(status: &str, issue_id: Option<&str>) -> String
```

- 与 Node `routines.ts:87` 1:1 对齐
- 6 个 known status + 1 个 fallback（未来 status 原样透传）
- 用途：写入 `routine_triggers.last_result` 列

### 2.3 `normalize_webhook_timestamp_ms`（routine.rs）

```rust
pub fn normalize_webhook_timestamp_ms(raw_timestamp: &str) -> Option<i64>
```

- 与 Node `routines.ts:100` 1:1 对齐
- `> 1e12` 视为 ms，否则视为秒 → 乘 1000
- 用途：webhook 签名校验入口，归一化 timestamp 后与 `Date.now()` 比 `replayWindowSec`

## 3. 高内聚低耦合

| 函数 | 依赖 | 协作 |
|---|---|---|
| `is_sub_hourly_cron_expression` | `next_cron_tick_in_timezone`（同模块）| 纯计算 |
| `next_result_text` | 无（仅 `&str`）| 纯字符串 |
| `normalize_webhook_timestamp_ms` | 无（仅 `&str` → `i64`）| 纯数值 |

零新增外部依赖；零外部破坏性变更；纯增量 API。

## 4. 测试覆盖（13 个新单测）

| 函数 | 测试数 | 场景 |
|---|---|---|
| `is_sub_hourly_cron_expression` | 5 | 每分钟=true / 每5分钟=true / 每小时=false / 每天=false / 非法=false |
| `next_result_text` | 4 | issue_created+id / issue_created 无id / 5个 known statuses / 未知 status 透传 |
| `normalize_webhook_timestamp_ms` | 4 | 秒→毫秒 / 已是毫秒 / 非法字符串 / NaN/Infinity |

合计 13 个新测试。

## 5. 验证基线

```text
$ cargo test -p pc-workflow --lib
test result: ok. 39 passed; 0 failed
                          ↑ 从 26 → 39 (+13 个新测试)

$ cargo clippy -p pc-workflow --lib --tests -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.42s
                          ↑ 0 warnings（pedantic 模式下 cast_possible_truncation 已加 #[allow] + 注释解释）

$ cargo fmt -p pc-workflow --check
                          ↑ no diff

$ cargo check --workspace
Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.96s
                          ↑ 0 errors（pre-existing pc-http warnings 与本轮无关）
```

## 6. Node 1:1 对齐验证

| 场景 | Node | Rust | 一致 |
|---|---|---|---|
| `isSubHourly("* * * * *", "UTC", now)` | true | true | ✅ |
| `isSubHourly("*/5 * * * *", "UTC", now)` | true | true | ✅ |
| `isSubHourly("0 * * * *", "UTC", now)` | false | false | ✅ |
| nextResultText("issue_created", "iss-42") | "Created execution issue iss-42" | 同 | ✅ |
| nextResultText("coalesced", null) | "Coalesced into an existing live execution issue" | 同 | ✅ |
| nextResultText("unknown", null) | "unknown"（透传）| "unknown" | ✅ |
| `normalizeWebhookTimestampMs("1700000000")` | 1700000000000 | 1700000000000 | ✅ |
| `normalizeWebhookTimestampMs("NaN")` | null | null | ✅ |

## 7. 完成判据

- [x] Rust 源码写到 `crates/pc-workflow/src/{schedule,routine}.rs`（高内聚低耦合）
- [x] `cargo clippy -p pc-workflow -- -D warnings` 通过（pedantic 模式下）
- [x] `cargo test -p pc-workflow --lib` 通过（39 passed，含 13 个新测试）
- [x] `cargo fmt -p pc-workflow --check` 无 diff
- [x] `cargo check --workspace` 通过
- [x] 中文说明完整（本 evidence 文件）
- [x] 与 Node `routines.ts` L67-100 三个纯函数 1:1 对齐

## 8. pc-workflow 整体进度

| 指标 | R487 前 | R487 后 | R488 后 | Δ |
|---|---|---|---|---|
| 源文件 | 6 | 6 | 6 | 0 |
| 源码 LOC | 1358 | 1511 | 1666 | +155 |
| 单元测试 | 18 | 26 | 39 | +21 |
| 对齐 Node `routines.ts` 纯函数 | 0/~5 | 1/5 | 4/5 | +3 |

## 9. 下一轮候选（R489）

按高 ROI 排序：

1. **`pc-workflow::build_routine_dispatch_decision`**（Node `routines.ts` catch-up 决策）
2. **`pc-companies` 深化**（当前 13 tests → 目标 50+）
3. **`pc-pipelines` 深化**（当前 8 tests → 目标 30+）
4. **`pc-routines` 服务层接入** `next_cron_tick_in_timezone` / `is_sub_hourly_cron_expression`

建议 **R489 推进 pc-companies 深化** —— 当前是测试最少的核心业务 crate 之一（约 13 tests），缺口最大；Node `services/companies.ts` 含大量可独立复刻的纯函数（角色升级、成员邀请校验、状态机转换等）。
