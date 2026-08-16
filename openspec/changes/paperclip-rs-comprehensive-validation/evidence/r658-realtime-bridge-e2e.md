# R658 — realtime bridge 真实 E2E（2026-08-16）

## 目标

完整复刻 Node services/routines.ts + services/realtime-bridge.ts 的关键
交互模式：pc-routines domain 发出 RoutineHookEvent::RunSkipped，realtime
bridge hook 把它翻译成 LiveEvent{event: "routine.run_skipped", resource: "routine_run"}
推到 WS hub，前端 WS client 通过 subscribe() 接收。

## 实现

1. 新增文件：crates/pc-routines/tests/r658_realtime_bridge_e2e.rs（237 行）
2. 新增 dev-dep：crates/pc-routines/Cargo.toml 加 pc-realtime、pc-errors、async-trait
3. 测试覆盖：
   - 真实 PG (postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos)
   - 真实 WS hub RealtimeHandle::start(64)
   - 真实订阅端 realtime.subscribe() -> broadcast::Receiver
   - worktree 抑制路径（env PAPERCLIP_IN_WORKTREE=true）
   - 注入 RealtimeRoutineHook + RecordingHook
   - tick_scheduled_triggers(now, 10) -> 57 个 RunSkipped
   - subscriber 真实收到 routine.run_skipped

## 真实结果

```
running 1 test
R658 tick outcome: dispatched=0
R658 realtime subscriber received: event=routine.run_skipped resource=routine_run
    company=Some(dc9ca860-a56b-4b82-af7e-ad462e484310)
    actor=Some("routine-schedule")
R658 PASS: realtime bridge E2E (57 RunSkipped recorded, subscriber received)
test r658_realtime_hook_publishes_run_skipped_via_ws_hub ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.19s
```

## pc-routines 全套

```
test result: ok. 112 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

| 文件 | tests | 状态 |
|---|---|---|
| lib (unit) | 7 | ok |
| e2e_routine_service | 41 | ok |
| r647_run_lifecycle | 24 | ok |
| r649_worktree_cutoff | 4 | ok |
| r650_activity_gate | 6 | ok |
| r652_suppression_activity_log | 6 | ok |
| r653_realtime_event_broadcast | 4 | ok |
| r654_project_scope_activity_gate | 3 | ok |
| r658_realtime_bridge_e2e | 1 | NEW |
| r659_scheduler_dispatches_real_run | 1 | ok |
| routine_hook_contract | 8 | ok |
| routines_service_route_contract | 7 | ok |
| 总计 | 112 | 0 FAIL |

## 关键技术点

- pc-routines 仍不依赖 pc-realtime（生产代码无强耦合），只有
  dev-dep。bridge 模式由 pc-realtime 注入到 pc-routines，方向正确。
- RealtimeHandle::subscribe() 返回 broadcast::Receiver<Arc<LiveEvent>>，
  测试用 try_recv() 验证消息真实到达。
- LiveEvent 字段：event=routine.run_skipped, resource=routine_run,
  company_id=Some(company_id), actor=Some(routine-schedule)。
- 触发链：
  - RoutineService::tick_scheduled_triggers
  - scheduler::tick_scheduled_triggers
  - evaluate_automatic_dispatch_eligibility (in_worktree=true)
  - suppress worktree_execution_cutoff
  - record_skipped_run -> dispatch RunSkipped event
  - RoutineHook::on_routine_event (each hook)
  - RealtimeRoutineHook::on_routine_event
  - LiveEvent::new(routine.run_skipped, ...)
  - RealtimeHandle::publish
  - broadcast::Sender::send
  - subscriber recv OK

## 文件位置

- 测试：crates/pc-routines/tests/r658_realtime_bridge_e2e.rs
- 依赖配置：crates/pc-routines/Cargo.toml ([dev-dependencies])
- 进度快照：openspec/changes/paperclip-rs-comprehensive-validation/evidence/progress-2026-08-16-v3.md
