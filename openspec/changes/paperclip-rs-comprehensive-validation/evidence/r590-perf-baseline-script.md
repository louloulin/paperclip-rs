# R590 — scripts/perf-baseline.sh 性能基线快速报告

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

**scripts/perf-baseline.sh（105 行）** 写完，单次启动 + 100 次采样 → 输出关键指标。

## 2. 真实运行结果（R590 实测）

```
=== Perf Baseline (R590) ===
  Boot time (warm):     1046ms
  /health p50:         3ms
  /health p99:         5ms
  RSS (idle):          54.0MB
  Threads:                   16

=== vs Node 上游（参考） ===
  metric                       Node         Rust   提升
  boot (warm)                3000ms       1046ms     2.8x
  /health p99                  80ms          5ms    16.0x
  RSS (idle)                  250MB       54.0MB     4.6x

[perf] PASS ✅
```

## 3. 关键指标对照

| 指标 | Node 上游 | Rust 实测 | 提升 |
|---|---|---|---|
| Boot（warm） | 3000ms | 1046ms | **2.8x** ↑ |
| /health p50 | ~40ms | 3ms | **13x** ↑ |
| /health p99 | 80ms | 5ms | **16x** ↑ |
| RSS（idle） | 250MB | 54MB | **4.6x** ↓ |
| Threads | ~30 | 16 | 1.9x ↓ |

## 4. 关键设计决策

### 4.1 单次快速版 vs 5 分钟长跑版

| 脚本 | 时长 | 用途 |
|---|---|---|
| `perf-baseline.sh` | ~30s | CI / 开发者本地快速验证 |
| `long-run-5min.sh` | 5min | 生产前 / 性能回归 |

### 4.2 ns 级时间戳

用 `date +%s%N`（纳秒级）计算启动时间，比秒级精确 1000 倍。

### 4.3 100 次采样取 p50/p99

简单 `curl -w '%{time_total}'` + `sort -n` 取第 50/99 个。避免引入 wrk 工具依赖。

### 4.4 ps 取 RSS

`ps -o rss= -p $SRV_PID` 取常驻内存（KB）。除以 1024 转 MB。

### 4.5 与 Node 上游对比表

脚本内置 Node 参考值；输出实测对比。提升比（`scale=1`）用 `bc` 计算。

## 5. 使用场景

### 5.1 CI 集成

```yaml
# .github/workflows/ci.yml
- bash scripts/perf-baseline.sh  # 必须通过
```

### 5.2 开发者本地

```bash
# 改完代码后验证性能不退化
bash scripts/perf-baseline.sh
```

### 5.3 发布前回归

```bash
# 与上次 release 对比
git checkout v0.X.0 && bash scripts/perf-baseline.sh > /tmp/baseline-X.txt
git checkout main && bash scripts/perf-baseline.sh > /tmp/baseline-main.txt
diff /tmp/baseline-X.txt /tmp/baseline-main.txt
```

## 6. 验收清单

- [x] 临时 PG + migrate + server warm 启动 ✅
- [x] 启动时间 < 2s（实测 1046ms） ✅
- [x] /health p99 < 30ms（实测 5ms） ✅
- [x] RSS < 100MB（实测 54MB） ✅
- [x] 与 Node 上游对比表 ✅
- [x] 单行 PASS/FAIL 退出码 ✅
- [x] 失败打印诊断（log tail） ✅
