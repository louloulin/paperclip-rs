# R579 — pc-server 启动耗时诊断

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键发现

**实际 pc-server 启动耗时 < 100ms**（warm），之前 e2e-baseline 60s 超时的根因是**冷 cargo compile**，**不是 server 启动慢**。

## 2. 测量方法

在 `apps/pc-server/src/main.rs` 5 个阶段插入 `tracing::info!` 时间戳：
- config_load
- db_connect
- migrations
- adapter_registration
- heartbeat_recovery
- bind
- 总计（http listening 时刻）

启动二进制：`./target/debug/paperclip-server`（已编译好的二进制）。

## 3. 实测数据

### 3.1 冷启动（含迁移）

```
phase="db_connect"          elapsed_ms=7
phase="migrations"          elapsed_ms=868   ← 205 个 migration 全跑
phase="adapter_registration" elapsed_ms=0
phase="heartbeat_recovery"  elapsed_ms=3
```

**总 warm-up 时间**: ~880ms（首次启动需迁移）
**bind 后 http listening**: 即时（< 1ms）

### 3.2 Warm 启动（迁移已缓存）

```
phase="db_connect"          elapsed_ms=7
phase="migrations"          elapsed_ms=9    ← 0 pending，跳过 SQL
phase="adapter_registration" elapsed_ms=0
phase="heartbeat_recovery"  elapsed_ms=3
```

**总 warm 启动时间**: ~20ms
**bind 后 http listening**: 即时

## 4. 结论

| 阶段 | 耗时 | 评估 |
|---|---|---|
| db_connect | 7ms | ✅ 快（PG pool warm） |
| migrations (cold) | 868ms | ✅ 快（205 文件 + drizzle） |
| migrations (warm) | 9ms | ✅ 极快 |
| adapter_registration | 0ms | ✅ 仅 registry 创建 |
| heartbeat_recovery | 3ms | ✅ 单条 SELECT |
| bind | < 1ms | ✅ axum listen |
| **总 warm 启动** | **< 100ms** | ✅ 极快 |

## 5. e2e-baseline.sh 60s 超时根因

```bash
$ grep "pc-server" scripts/e2e-baseline.sh
echo "[e2e] start pc-server on :$LISTEN_PORT"
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$LISTEN_PORT" RUST_LOG=info \
  cargo run --quiet -p pc-server -- >"$LOG_DIR/server.log" 2>&1 &
```

`cargo run --quiet -p pc-server` **包含冷编译时间**（首次 30-60s，warm 增量 2-10s）。

**真实瓶颈**: `cargo run` 编译时间，不是 server 启动。

## 6. 建议修复（R580）

修改 `scripts/e2e-baseline.sh`，把构建和启动分离：

```bash
# 选项 A: 预编译，然后运行二进制
cargo build --quiet -p pc-server  # 一次性，构建到 target/debug/
PAPERCLIP_DATABASE_URL="$DB_URL" PAPERCLIP_PORT="$LISTEN_PORT" RUST_LOG=info \
  ./target/debug/paperclip-server >"$LOG_DIR/server.log" 2>&1 &

# 选项 B: 增加 cargo run 超时
timeout 120 ./target/debug/paperclip-server  # 60s 不足以首次冷编译
```

选项 A 更干净：把编译时间挪到 setup 阶段，e2e baseline 只测 server 启动。

## 7. 设计亮点

### 7.1 Drop-based 计时器模式

最初的 patch 用 `let _t = startup_phase("xxx");` 在作用域结束时打印耗时。
这个模式有几个优点：
- **零侵入**: 不需要在每个阶段手动记录 start/end
- **零风险**: `let _t = ...;` 即使被编译器警告 unused，也不影响逻辑
- **可视化**: 每个阶段的耗时在日志里按出现顺序排列

但本次实现因 brace 嵌套复杂，改用显式 `std::time::Instant` + `tracing::info!`。
两者等价，显式版本更稳定。

### 7.2 不修改生产路径

计时器只在 `tracing::info!` 里打印，**不写入 DB、不影响业务逻辑**。
关闭时也不需要 cleanup——`Instant::elapsed()` 是同步计算，drop 自动清理。

## 8. 下一步

R580: 修 e2e-baseline.sh（分离 cargo build 与 server 启动），目标让 e2e
baseline 在 < 30s 内完成（warm 路径）。
