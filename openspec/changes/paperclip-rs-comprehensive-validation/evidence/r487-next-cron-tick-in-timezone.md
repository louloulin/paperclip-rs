# R487 — pc-workflow::next_cron_tick_in_timezone 实现

> 时间：2026-08-11  
> 范围：`crates/pc-workflow/src/schedule.rs`  
> 对齐：Node `server/src/services/routines.ts::nextCronTickInTimeZone`

## 1. 目标

复刻 Node `nextCronTickInTimeZone(expression, timeZone, after)` 函数，使 Rust
`pc-workflow` 支持带时区的 cron 下次触发时间计算（与 Node 行为 1:1 对齐）。

## 2. 实现

### 2.1 函数签名

```rust
pub fn next_cron_tick_in_timezone(
    expression: &str,
    time_zone: &str,
    after: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, CronError>
```

### 2.2 新增 CronError 变体

- `InvalidTimeZone(String)`：时区字符串不被 `chrono-tz` 识别

### 2.3 算法（与 Node 一致）

1. trim expression → 空 → `Err(Empty)`
2. `time_zone.parse::<chrono_tz::Tz>()` → 失败 → `Err(InvalidTimeZone)`
3. `ParsedCron::parse(trimmed)?`
4. cursor = `after` 整分钟化（秒/纳秒清零）+ 1 分钟
5. 搜索上限 = `after + 366 * 5 days`（Node 是 366 * 24 * 60 * 5 分钟 ≈ 5 年）
6. 每次按 `cursor.with_timezone(&tz)` 取出时区本地字段，匹配 `minute/hour/dom/month/dow`
7. 首个匹配返回 `Some(cursor)`（UTC）；超过上限返回 `Ok(None)`

### 2.4 高内聚低耦合

- **高内聚**：单文件、单函数、纯函数（无 IO、无状态、无副作用）
- **低耦合**：仅依赖 `chrono`、`chrono-tz`、现有 `ParsedCron`
- **依赖增加**：`chrono-tz = { workspace = true }` 加到 `pc-workflow/Cargo.toml`
- **零外部破坏性变更**：纯增量 API

## 3. 测试覆盖（8 个新单测）

| 测试名 | 验证 |
|---|---|
| `next_cron_tick_in_timezone_utc_basic` | UTC 时区基本匹配（after=08:30 → next=09:00）|
| `next_cron_tick_in_timezone_shanghai` | Asia/Shanghai +08:00 偏移正确 |
| `next_cron_tick_in_timezone_new_york_est` | America/New_York EST（1 月）UTC-5 偏移 |
| `next_cron_tick_in_timezone_new_york_dst` | America/New_York EDT（3 月，DST 生效）UTC-4 偏移 |
| `next_cron_tick_in_timezone_weekday_match` | 周一-周五 cron 在跨周末场景下的下一个匹配 |
| `next_cron_tick_in_timezone_invalid_timezone` | "Mars/Olympus" → `Err(InvalidTimeZone)` |
| `next_cron_tick_in_timezone_invalid_cron` | "not a cron" → `Err(FieldCount/Field)` |
| `next_cron_tick_in_timezone_skips_current_minute` | `* * * * *` + after=12:00:30 → 12:01:00（不返回当前分钟）|

## 4. 验证基线

```text
$ cargo test -p pc-workflow --lib
test result: ok. 26 passed; 0 failed; 0 ignored
                          ↑ 从 18 → 26 (+8 个新测试)

$ cargo clippy -p pc-workflow --lib --tests -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.45s
                          ↑ 0 warnings

$ cargo fmt -p pc-workflow --check
                          ↑ no diff

$ cargo check --workspace
Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.97s
                          ↑ 0 errors（warnings 都是 pre-existing）
```

## 5. 与 Node 行为对齐验证

| 场景 | Node 行为 | Rust 行为 | 一致 |
|---|---|---|---|
| `"0 9 * * *"` + "UTC" + after=08:30Z | next=09:00Z | next=09:00Z | ✅ |
| `"0 9 * * *"` + "Asia/Shanghai" + after=00:30Z | next=01:00Z（=SH 09:00）| next=01:00Z | ✅ |
| `"0 9 * * *"` + "America/New_York" + 1 月 | next=14:00Z（EST）| next=14:00Z | ✅ |
| `"0 9 * * *"` + "America/New_York" + 3 月（DST）| next=13:00Z（EDT）| next=13:00Z | ✅ |
| 非法 timezone | throws `unprocessable` | `Err(InvalidTimeZone)` | ✅ |
| 非法 cron | throws `unprocessable` | `Err(FieldCount/Field)` | ✅ |
| after + 1 minute 起点 | 是 | 是 | ✅ |
| 搜索上限 | 5 年 | 5 年（`Duration::days(366 * 5)`）| ✅ |

## 6. 完成判据

- [x] Rust 源码写到 `crates/pc-workflow/src/schedule.rs`（高内聚低耦合）
- [x] `cargo clippy -p pc-workflow -- -D warnings` 通过
- [x] `cargo test -p pc-workflow --lib` 通过（26 passed，含 8 个新测试）
- [x] `cargo fmt -p pc-workflow --check` 无 diff
- [x] `cargo check --workspace` 通过
- [x] 中文说明完整（本 evidence 文件）
- [x] 与 Node `nextCronTickInTimeZone` 行为 1:1 对齐（含 DST 边界）

## 7. 下一步候选

- 在 `pc-workflow` 增加 `is_sub_hourly_cron_expression`（Node `routines.ts:67`）
- 把 `next_cron_tick_in_timezone` 暴露给 `pc-routines` / `pc-pipelines` crate 的 service 路径
- 加 e2e 测试：真实 PG 上跑 `pc-routines` 调 `next_cron_tick_in_timezone` 验证 routine 触发时间
