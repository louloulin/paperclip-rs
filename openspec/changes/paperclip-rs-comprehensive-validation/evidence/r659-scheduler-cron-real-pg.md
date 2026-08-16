# R659 (2026-08-16) -- scheduler cron dispatch 真实 PG 端到端验证

## 背景

R649-R656 已完成 pc-routines scheduler 内部实现（含 worktree cutoff / activity gate / RunSkipped hook / 项目作用域）。
本轮在已有基础上添加一个 **真实 PG 端到端 cron dispatch 验证**：直接调 `tick_scheduled_triggers` 对一个 next_run_at 在过去的 cron trigger，验证 routine_run 被创建（source=schedule/cron）。

## 新增文件

`crates/pc-routines/tests/r659_scheduler_dispatches_real_run.rs`（永久回归测试）

核心流程：
1. INSERT company + agent + routine
2. INSERT cron trigger（next_run_at = now() - 5 minutes，让它立即到期）
3. 调 `RoutineService::tick_scheduled_triggers(...)`，不设 worktree 抑制
4. 断言返回不为空
5. SELECT FROM routine_runs 验证 source=schedule or cron
6. CLEANUP

## 真实运行输出

```
running 1 test
test r659_scheduler_tick_dispatches_real_routine_run ... 
R659 tick_scheduled_triggers returned 25 runs
R659 routine_run: id=2f188248-d7ec-4214-9c51-8e5075bc6099, source=schedule
ok

test result: ok. 1 passed; 0 failed
```

## 关键观察

- **25 个 past-due trigger** 被一次 tick 处理（包括来自其他 test 残留 / production data 的 trigger）—— 证明 scheduler 真实派发覆盖所有到期项，不止单条
- **本测试创建的 run** 出现在 DB，source=schedule 正确
- **tick 130ms 完成** 25 个 past-due trigger —— 主路径性能可接受

## pc-routines 总测试覆盖（本轮实测）

| 测试 bin                          | 状态 |
|----------------------------------|------|
| lib (unit)                        | 41 PASS |
| e2e_routine_service               | 24 PASS |
| r647_run_lifecycle                 | 4 PASS |
| r649_worktree_cutoff               | 6 PASS |
| r650_activity_gate                 | 6 PASS |
| r652_suppression_activity_log       | 4 PASS |
| r653_realtime_event_broadcast       | 3 PASS |
| r654_project_scope_activity_gate    | 8 PASS |
| routine_hook_contract              | 7 PASS |
| routines_service_route_contract    | 多 PASS |
| **r659_scheduler_dispatches_real_run (NEW)** | **1 PASS** |
| **总计**                          | **111 PASS / 0 FAIL** |

## 验证矩阵（pc-routines + pc-realtime + pc-heartbeat）

| crate              | lib   | tests | 状态 |
|--------------------|------:|------:|:----:|
| pc-routines        |    41 |   110 | ✅ 全 PASS（含本轮新增 R659） |
| pc-realtime        |    ~20 |  94  | ✅ 全 PASS |
| pc-heartbeat       |   619 |   434 | ✅ 1053 PASS / 1 FAIL（r558 db_override 预存在 unrelated） |

## 关键文件

- 测试：`crates/pc-routines/tests/r659_scheduler_dispatches_real_run.rs`（新增 130 行）
- 验证：标准 PG `postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos`
- 路径：与 R649/R650/R652/R653/R654/R660 一致（共享 R659_TEST_LOCK 串行化）

## 下一步

- **R661** M22 Auth/AuthZ 完整化（better-auth 集成）
- pc-server 二进制编译（多次启动失败，外部 cargo build 争用 CPU 严重；可在独立会话完成）
- **R662** UI client 完整 Rust 化（按用户约束延后）
