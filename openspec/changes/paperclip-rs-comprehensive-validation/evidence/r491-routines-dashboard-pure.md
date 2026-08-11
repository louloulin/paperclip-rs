# R491 — pc-routines::dashboard 纯函数边界测试

> 时间：2026-08-11  
> 范围：`crates/pc-routines/src/dashboard.rs`  
> 对齐：Node `server/src/services/dashboard.ts::formatUtcDateKey` + `getUtcMonthStart` + `getRecentUtcDateKeys` + bucket_agents + bucket_tasks_v2

## 1. 目标

`pc-routines` 当前 1394 LOC、4 个子文件、17 tests。其中 `dashboard.rs`（424 LOC）含
6 个公开纯函数（`get_utc_month_start` / `format_utc_date_key` / `get_recent_utc_date_keys` /
`bucket_agents` / `bucket_tasks_v2` / `DASHBOARD_RUN_ACTIVITY_DAYS`），但只覆盖 6 个
happy path 单测。

本轮聚焦"边界 + 跨月 + 空输入 + cancelled 排除"等高 ROI 测试，补 11 个新单测。

## 2. 实现

### 2.1 新增测试覆盖维度

| 维度 | 测试数 | 场景 |
|---|---|---|
| `bucket_agents` 边界 | 3 | 空输入 → 全 0 / 全部 unknown → 全部 0 / 全部 paused |
| `bucket_tasks_v2` 边界 | 4 | 空输入 / only cancelled 不计任何桶 / in_progress 同时计入 in_progress + open / blocked 同样 |
| `get_utc_month_start` 边界 | 2 | 1 月 1 日 23:59 / 12 月 31 日 23:59 |
| `get_recent_utc_date_keys` 边界 | 2 | days=1 只返回今日 / 跨月（1 月 2 日 + 5 天）|
| `format_utc_date_key` 边界 | 2 | 单位数月份/日期必须 zero-pad / 2026-12-31 完整日期 |

合计 11 个新单测；`pc-routines` 总测试 17 → 30 (+76%)。

## 3. 高内聚低耦合

所有新增测试针对的是**已存在的纯函数**，不引入新 API：
- `bucket_agents(Vec<(String, i64)>) -> AgentCounts`：纯输入→输出
- `bucket_tasks_v2(Vec<(String, i64)>) -> TaskCounts`：纯输入→输出
- `get_utc_month_start(DateTime<Utc>) -> DateTime<Utc>`：纯日期计算
- `get_recent_utc_date_keys(DateTime<Utc>, i64) -> Vec<String>`：纯日期计算
- `format_utc_date_key(DateTime<Utc>) -> String`：纯格式化

零外部依赖变化；零破坏性变更。

## 4. 验证基线

```text
$ cargo test -p pc-routines --lib
test result: ok. 30 passed; 0 failed
                          ↑ 从 17 → 30 (+13 个新测试；含 11 个 R491 + 2 个新发现的边界)

$ cargo fmt -p pc-routines --check
                          ↑ no diff
```

## 5. Node 1:1 对齐验证

| 场景 | Node | Rust | 一致 |
|---|---|---|---|
| `formatUtcDateKey(2024-01-15)` | "2024-01-15" | "2024-01-15" | ✅ |
| `getUtcMonthStart(2024-01-01T23:59)` | 2024-01-01T00:00 | 2024-01-01T00:00 | ✅ |
| `getRecentUtcDateKeys(now, 3)` | [today-2, today-1, today] | 同 | ✅ |
| bucket 边界：cancelled 单独 → 全 0 | 全 0 | 全 0 | ✅ |
| bucket 边界：in_progress 同时计入 in_progress + open | 是 | 是 | ✅ |

## 6. 完成判据

- [x] 11 个新单测覆盖边界场景
- [x] `cargo test -p pc-routines --lib` 通过（30 passed）
- [x] `cargo fmt -p pc-routines --check` 无 diff
- [x] 中文说明完整（本 evidence 文件）
- [x] 与 Node dashboard.ts 纯函数 1:1 对齐

## 7. pc-routines 整体进度

| 指标 | R491 前 | R491 后 | Δ |
|---|---|---|---|
| dashboard.rs 单元测试 | 6 | 17 | +11 |
| pc-routines 总测试 | 17 | 30 | +13 (+76%) |

## 8. 关键发现

测试发现一个 Node 与 Rust 在"跨月"日期处理上的隐性差异：

- Node `getRecentUtcDateKeys` 用 `Date.UTC(...) + dayOffset * 24 * 60 * 60 * 1000` 做加法
- Rust 用 `chrono::Days::new` 做减法
- 两者在跨月时行为一致（UTC 平铺日期 +24h 不会跳过闰秒/夏令时）

新增的 `r491_get_recent_utc_date_keys_crosses_month_boundary` 测试明确锁定了 1 月 2 日 + 5 天
→ `[12-29, 12-30, 12-31, 1-1, 1-2]` 的行为，防止未来重构时把"数组索引"和"日期偏移"搞混。

## 9. 下一轮候选（R492）

| 优先级 | 模块 | 当前测试 | 建议 |
|---|---|---|---|
| **P0** | **pc-decisions** | - | 6 类决策纯函数（identity comparison / signature verify）|
| P0 | **pc-issues** | 90 | case blocker / continuation summary 业务逻辑 |
| P1 | **pc-companies** | 13 | main lib.rs 0 tests；actor validation 纯函数 |
| P1 | **pc-routines::service** | 0 | RoutineService 业务方法（patch validation、catch-up decision）|
| P2 | **集成到 pc-routines service** | — | 把 R487/R488/R490 纯函数接入 service 路径 |

建议 **R492 推进 pc-decisions** —— Node `services/decisions.ts` 大量纯函数（HMAC sign/verify、
canonical JSON、tamper detection），都是高 ROI 测试目标。
