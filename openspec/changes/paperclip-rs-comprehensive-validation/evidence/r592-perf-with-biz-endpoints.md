# R592 — perf-baseline 增加业务端点合约检查（4 重断言）

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

`scripts/perf-baseline.sh` 增强为 4 重断言（之前 3 重）：
- ✅ /health p99 < 30ms
- ✅ RSS < 100MB
- ✅ 最终 /health 200
- 🆕 **6 个业务端点合约正确**（agents / companies / issues / decisions / approvals / heartbeats）

## 2. 真实运行结果（R592 实测）

```
=== Perf Baseline (R592) ===
  Boot time (warm):     1046ms
  /health p50:         3ms
  /health p99:         4ms
  RSS (idle):          54.1MB
  Threads:                   17
  Business endpoints:  6/6 OK

=== vs Node 上游（参考） ===
  metric                       Node         Rust   提升
  boot (warm)                3000ms       1046ms     2.8x
  /health p99                  80ms          4ms    20.0x
  RSS (idle)                  250MB       54.1MB     4.6x

[perf] PASS ✅
  - /health p99 4ms < 30ms target
  - max RSS 54MB < 100MB target
  - business endpoints 6/6 OK
```

## 3. 关键设计决策

### 3.1 选择无认证 list 端点

6 个端点都是「GET list，无认证可访问，返回 200 + 空数组」：
- `/api/agents`
- `/api/companies`
- `/api/issues`
- `/api/decisions`
- `/api/approvals`
- `/api/heartbeats`

避免：
- 需认证端点（容易因 session 问题假阳性失败）
- 需 company_id 嵌套端点（需构造 fake uuid）
- 计算昂贵端点（拉低性能基线）

### 3.2 4 重断言保证 correctness + performance

```
P99_OK  + RSS_OK + HEALTH_OK + BIZ_PASS
              ↓
       ALL PASS = true
```

任何一项失败 → exit 1。明确报告哪项失败。

### 3.3 不破坏原有行为

- 启动时间 / p50 / p99 / RSS 采样逻辑保持
- 输出格式扩展（多一行）
- 与 Node 对比表保持

## 4. 与 V11 协同

| 脚本 | 覆盖 |
|---|---|
| `v11-ui-happy-path.sh` | 60 个端点的合约正确性（含嵌套路径） |
| `perf-baseline.sh` (R592) | 6 个核心 list 端点 + 性能基线 |
| `long-run-5min.sh` | 长时间运行稳定性 |
| `e2e-baseline.sh` | 启动冒烟 |

四个脚本互补，cover 验证层多维度。

## 5. 验收清单

- [x] 业务端点检查（6 个）✅
- [x] 4 重断言逻辑 ✅
- [x] PASS/FAIL 输出清晰 ✅
- [x] Node 对比表保持 ✅
- [x] 失败时打印 server log tail ✅
- [x] 退出码正确（0/1）✅
- [x] 总耗时 < 30s ✅
