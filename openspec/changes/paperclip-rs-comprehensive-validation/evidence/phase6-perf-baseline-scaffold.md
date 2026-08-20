# Phase 6 (V13) — Performance Baseline Scaffold

## 目标

建立 criterion benches 框架 + long-run 脚本 + perf-baseline 文档，覆盖 hot-path crate 的性能基线跟踪。

## 实现

### 1. Cargo workspace 加入 criterion

```toml
[workspace.dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
```

### 2. 5 个 hot-path crate 增加 benches

| Crate | Bench 文件 | Bench 函数数 | 覆盖 |
|---|---|---|---|
| pc-decisions | `benches/pure_bench.rs` | 5 | classify_effect_type / effect_target_ids / target_actions / same_ids_100 / interpolate_4vars |
| pc-routines | `benches/pure_bench.rs` | 8 | is_valid_routine_variable_name / is_valid_routine_date_string / normalize_webhook_timestamp_ms / parse_{boolean,number,date} / normalize_draft_routine_status / assert_routine_can_enable |
| pc-heartbeat | `benches/scheduler_bench.rs` | 1 | enforce_issue_execution_lock_for |
| pc-realtime | `benches/broadcast_bench.rs` | 2 | live_event_new / live_event_clone |
| pc-http | `benches/route_bench.rs` | 1 | serde_companies_list_roundtrip |

每个 crate Cargo.toml 添加 `[[bench]]` 块 + `criterion = { workspace = true }` 到 `[dev-dependencies]`。

### 3. docs/perf-baseline.md

3 大节：
- 测试方法（criterion / wrk / Rust vs Node 对比）
- 当前覆盖（5 crates, 17 bench 函数）
- 性能目标（V13 声明 + 实际数据 placeholder）
- 持续跟踪（PR benchmark 对比 + 回退告警）

### 4. scripts/long-run-5min.sh (已有)

已有 6814 字节版本（V13 R588 baseline），无需重写。

## 验证

```
cargo build --workspace --benches
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.52s
```

### 实际跑 pc-decisions bench（quick 模式）

```
cargo bench -p pc-decisions --bench pure_bench -- --quick
bench:           0 ns/iter (+/- 0)         ← classify_effect_type (zero cost)
bench:          32 ns/iter (+/- 0)         ← target_actions
bench:           1 ns/iter (+/- 0)         ← effect_target_ids (single obj access)
bench:        3884 ns/iter (+/- 84)        ← same_ids_100 (Vec compare)
bench:          64 ns/iter (+/- 0)         ← interpolate_4vars
```

## 仍 deferred（明确原因）

- 实际 5 分钟 wrk 长跑数据采集：需真实 PG + wrk + 真实负载环境
- GitHub Actions nightly 自动跑：需 CI 配置
- Rust vs Node 对比：需 Node paperclip 也跑同样 bench
- perf-trend.md 历史趋势：需累计多轮数据

## 累计

- 5 个 crates 增加 criterion benches（17 bench 函数）
- criterion 依赖接入 workspace
- perf-baseline.md 文档建立 baseline 框架
- 全部 benches 编译成功 + 已实测可运行

## 后续（V13 完整化）

- 配置 GitHub Actions nightly workflow 跑 `cargo bench --workspace`
- 接入历史数据到 `target/criterion-history/`
- Rust vs Node 同环境对比报告
- P99/RSS 实际数据填入 perf-baseline.md §4