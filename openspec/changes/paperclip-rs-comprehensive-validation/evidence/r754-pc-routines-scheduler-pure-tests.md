# R754 — pc-routines scheduler 调度计算补充测试

## 目标

pc-routines::scheduler 是 routine 定时触发的核心循环。当前单测只覆盖了 no-policy / sub-hourly 与 cron 解析，但以下路径缺回归：

- `enqueue_missed_with_cap` 在不漏算的情况下正确累计 tick 数
- 25+ 小时未跑时严格命中 MAX_CATCH_UP_RUNS 上限
- `RoutineSchedulerContext::in_worktree` 与 `effective_instance_id` 的 env / 显式注入优先级
- 非法 cron 表达式安全返回 None

本轮在 `crates/pc-routines/src/scheduler.rs::tests` 增加 4 个 r754_ 前缀单测。

## 实现

- 直接复用现有 `compute_catch_up` / `next_cron_tick` / `RoutineSchedulerContext` 公共 API。
- 不引入新依赖，不修改业务行为。

### 测试覆盖

1. `r754_compute_catch_up_cap_counts_missed_ticks`
   - 每小时一次，过去 4 小时窗口（12 / 13 / 14 / 15）
   - 断言 count = 4 且 claimed_next 推进到 now 之后
2. `r754_compute_catch_up_cap_respects_max_limit`
   - 37+ 小时未跑，期望 count = MAX_CATCH_UP_RUNS = 25
3. `r754_scheduler_context_in_worktree_and_instance_id_resolution`
   - 显式 instance_id 优先于 env
   - env fallback 行为
   - 空 env 时 in_worktree=false、effective_instance_id=None
4. `r754_next_cron_tick_invalid_expression_returns_none`
   - 非法 cron 字符串安全返回 None

## 验证结果

定向:

```
cargo test -p pc-routines scheduler::tests --lib
cargo test: 10 passed, 178 filtered out (1 suite, 0.00s)
```

pc-routines 全量:

```
cargo test -p pc-routines --lib
cargo test: 188 passed (1 suite, 0.02s)
```

## 关键决策

- 第 1 个用例原本按 3 次预期写错，已根据实际 loop 行为修正为 4 次（12 / 13 / 14 / 15）。
- 所有断言仅依赖 cron 解析纯函数与 env HashMap，不接触 DB / hook，避免引入 flake。

## 后续重点

- R755 — pc-feedback::share / trace pure 补足
- UI mutation 冒烟：agent / routine / tool / environment
- Adapter 仍按硬约束保持不动
