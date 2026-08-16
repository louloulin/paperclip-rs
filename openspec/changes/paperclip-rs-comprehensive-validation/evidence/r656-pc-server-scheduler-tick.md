# R656 (2026-08-16) — pc-server scheduler tick 运行时集成 + 真实启动验证

## 背景

R649-R655 完成了 pc-routines crate 内的 scheduler 核心（worktree cutoff / activity gate / record_skipped_run / RunSkipped hook / project scope SQL），但 pc-server 二进制并未挂载 tick loop。本轮把 scheduler tick 集成进 pc-server 启动序列，并真实 PG + 启动验证 tick 行为。

## 实现

### 修改文件
- apps/pc-server/Cargo.toml — 加 pc-routines + chrono = { workspace = true } 依赖
- apps/pc-server/src/main.rs — 加 use pc_routines::scheduler::RoutineSchedulerContext; use pc_routines::RoutineService;
- apps/pc-server/src/main.rs — 在 heartbeat_scheduler 之后新增 routine_scheduler = tokio::spawn(...)（5s 间隔 ticker）
- apps/pc-server/src/main.rs — 在 heartbeat_scheduler.abort() 之后加 routine_scheduler.abort()

### scheduler tick 任务模式（mirror heartbeat_scheduler）

```rust
let routine_tick_state = state.clone();
let db_for_routine = routine_tick_state.db.clone();
let scheduler_ctx = RoutineSchedulerContext::from_process_env(None);
let routine_scheduler = tokio::spawn(async move {
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let svc = RoutineService::new(db_for_routine)
        .with_scheduler_context(scheduler_ctx);
    let mut empty_ticks = 0u32;
    loop {
        ticker.tick().await;
        match svc.tick_scheduled_triggers(chrono::Utc::now(), 25).await {
            Ok(dispatched) if dispatched.is_empty() => { ... }
            Ok(dispatched) => { ... }
            Err(error) => tracing::warn!(...),
        }
    }
});
```

## 真实启动验证

启动临时 PG + pc-migrate + pc-server，等 15s：

```
$ bash .tmp/verify-scheduler-tick.sh
[verify] pre-build pc-server
[verify] starting pc-server on :53331 for 15s
[verify] scheduler tick log lines:
(none)
[verify] pc-server tail:
2026-08-16T00:40:37.533671Z DEBUG pc_routines::scheduler: tick_scheduled_triggers: 0 due candidates
2026-08-16T00:40:37.533733Z  INFO paperclip_server: plugin workers bootstrapped count=0
2026-08-16T00:40:37.569461Z  INFO paperclip_server: serving UI bundle from dist path=ui/dist
2026-08-16T00:40:37.571989Z  INFO paperclip_server: startup phase complete phase=bind elapsed_ms=0
2026-08-16T00:40:37.575635Z  INFO paperclip_server: http listening host=127.0.0.1 port=53331 total_startup_ms=133
2026-08-16T00:40:37.747993Z  INFO pc_http::middleware::access_log: http access ...
2026-08-16T00:40:42.531304Z DEBUG pc_routines::scheduler: tick_scheduled_triggers: 0 due candidates
2026-08-16T00:40:47.531208Z DEBUG pc_routines::scheduler: tick_scheduled_triggers: 0 due candidates
```

关键观察：3 次 tick 调用，间隔 5s 准确（5.0s + 5.0s = 10s 内 3 次）。Ticker MissedTickBehavior::Skip 保证不会因 SIGTERM 期间积压。

## 全 workspace lib 回归

```
$ cargo test --workspace --lib --no-fail-fast
104 suites / 7611 tests / 1 failed
```

唯一的 1 个 failure 是预存在 pc-adapter-process::graceful_tests::terminate_with_grace_handles_quick_exit，与 R656 工作无关（pre-existing in pc-adapter-process，按用户指示不在本轮修复）。

## Node 1:1 对齐

tick_scheduled_triggers 在 pc-server 启动后由 tokio spawn 调用，对应 Node 端：
- services/routines.ts::tickScheduledTriggers 集成在 server/src/main.ts 的 setInterval 循环

Node 端在 server start 后立即启动 setInterval，每秒 tick；Rust 端使用 5s 间隔（更保守，避免空跑时的 DB 查询开销）。后续可以读 cfg 调整。

## 后续

- R657 — webhook trigger 完整端点 + HMAC 验证
- R658 — pc-realtime 桥接 RunSkipped hook → LiveEvent
- R659 — 读 cfg 暴露 tick interval（默认 5s）
- R660 — scheduler tick 在 activity_gate project scope 下跨公司事务边界检查
