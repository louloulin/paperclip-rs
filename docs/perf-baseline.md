# Paperclip-rs Performance Baseline

> 配套：`RUNBOOK.md §5 性能调优` / `ARCHITECTURE.md §1 验证基线` / `scripts/long-run-5min.sh`
> 范围：criterion benches 覆盖 + 5 分钟长跑数据 + Rust vs Node 对比

---

## 1. 测试方法

### 1.1 单元级 benchmark（criterion）

每个 hot-path crate 提供 `benches/` 目录，criterion 自动产出 HTML report：

```bash
# 跑全部 benches
cargo bench --workspace

# 跑单个
cargo bench -p pc-decisions
cargo bench -p pc-routines
cargo bench -p pc-heartbeat
cargo bench -p pc-realtime
cargo bench -p pc-http

# 输出位置
# target/criterion/<bench_name>/report/index.html
```

### 1.2 5 分钟长跑（wrk）

```bash
# 启动 pc-server
./target/release/paperclip-server &

# 跑长跑
./scripts/long-run-5min.sh

# 输出
# - perf-baseline.json（wrk 完整输出）
# - 控制台：P50/P95/P99 latency + RPS + RSS
```

### 1.3 Rust vs Node 对比

```bash
# Rust 端
./target/release/paperclip-server &
./scripts/long-run-5min.sh > rust-baseline.json 2>&1

# Node 端（paperclip）
cd paperclip
pnpm dev &
./scripts/long-run-5min.sh > node-baseline.json 2>&1

# 对比
./scripts/perf-compare.sh rust-baseline.json node-baseline.json
```

---

## 2. 当前覆盖

| Crate | Bench | 测试函数 |
|---|---|---|
| pc-decisions | `pure_bench.rs` | classify_effect_type, effect_target_ids, target_actions, same_ids_100, interpolate_4vars |
| pc-routines | `pure_bench.rs` | evaluate_activity_gate, compute_catch_up, dispatch_eligibility, worktree_cutoff |
| pc-heartbeat | `scheduler_bench.rs` | heartbeat_tick_overhead |
| pc-realtime | `broadcast_bench.rs` | broadcast_publish |
| pc-http | `route_bench.rs` | serde_companies_list_roundtrip |

---

## 3. 性能目标（V13 声明）

参考 `PROJECT-PLAN.md` 与 `ARCHITECTURE.md §1.3`：

| 指标 | Node baseline | Rust 目标 | 说明 |
|---|---|---|---|
| P99 latency | TBD | ≤ Node × 0.7 | 30% 加速（保守） |
| P95 latency | TBD | ≤ Node × 0.7 | 同上 |
| RPS（GET /api/companies）| TBD | ≥ Node × 1.5 | Rust 并发优势 |
| RSS memory | TBD | ≤ Node × 0.6 | 40% 减少 |
| Cold start | 3-5s | < 200ms | Rust 启动快 15-25x |
| Docker image | ~500MB | ~80-120MB | musl 静态链接 |
| 编译时间 | n/a | first build ~10min, incremental ~30s | cargo |

---

## 4. 实际数据（待 nightly CI 跑通后填）

> 状态：**placeholder** — 需要真实环境（PG + 5min wrk 负载）才能产出。
> 真实环境 CI 配置完成后，本节将自动填充。

| 环境 | P50 | P95 | P99 | RPS | RSS |
|---|---|---|---|---|---|
| macOS M1 | — | — | — | — | — |
| Linux x86_64 | — | — | — | — | — |
| Windows x86_64 | — | — | — | — | — |

---

## 5. 持续跟踪

- 每次 PR 跑 `cargo bench --workspace` 并对比 main 分支的 criterion 输出
- 性能回退 > 5% 触发 PR 评论告警
- weekly job 把 criterion 数据归档到 `target/criterion-history/<date>/`

---

## 6. 相关脚本

- `scripts/long-run-5min.sh` — 5 分钟长跑 + 性能数据采集
- `scripts/perf-compare.sh` — Rust vs Node 对比（待实现）

---

## 7. 历史性能数据

- 2026-08-20：criterion benches scaffold 落地（5 个 crate，共 13 个 bench 函数）
- 待 nightly CI 接入后产出第一批 baseline 数据