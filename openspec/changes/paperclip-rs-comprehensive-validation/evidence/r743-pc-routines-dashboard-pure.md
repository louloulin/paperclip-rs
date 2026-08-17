# R743 -- pc-routines/src/dashboard_pure.rs

## 目标

补足 Node paperclip/server/src/services/routines/dashboard.ts 中
dashboard 聚合 zero-DB pure helpers（utc_month_start / utc_date_key /
recent_utc_date_keys / bucket_agents / bucket_tasks / aggregate_cost）。

## 新增 helpers (6 个)

| Node 函数 | Rust 函数 |
|---|---|
| getUtcMonthStart | utc_month_start |
| formatUtcDateKey | utc_date_key |
| getRecentUtcDateKeys | recent_utc_date_keys |
| bucketAgents | bucket_agents |
| bucketTasks | bucket_tasks (修正了原 double-count bug) |
| aggregateCost | aggregate_cost + CostSummary struct |

## 常量

- DEFAULT_DASHBOARD_DAYS = 30
- MAX_AGENT_ROWS = 1000
- MAX_TASK_ROWS = 5000

## 测试结果

cargo test -p pc-routines --lib dashboard_pure
test result: ok. 11 passed; 0 failed

## 关键设计

- bucket_tasks 修正了原 pc-routines/dashboard.rs 中的 double-count bug
  （在 match arm 里算了一次 in_progress，又在后面 if 里再次计入 open）
- 用 match 表达式做 status 分类，避免 Stringly-typed
- 所有 helper 零 IO / 零 DB，便于 actor 上下文外单测

## 文件

- 新增：crates/pc-routines/src/dashboard_pure.rs (6711 bytes)
- 修改：crates/pc-routines/src/lib.rs (+1 行 pub mod dashboard_pure)
