# R595 — long-run 增加 JSON 报告输出

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

`scripts/long-run-5min.sh` 增加 JSON 报告输出：
- 文件路径：`$PAPERCLIP_LONGRUN_REPORT`（默认 `/tmp/paperclip-longrun-report.json`）
- 包含：duration_s / heartbeat_count / health_p50_s / health_p99_s / max_rss_kb / checks / asserts

## 2. 三个验证脚本的 JSON 输出矩阵

| 脚本 | JSON 路径变量 | 内容 |
|---|---|---|
| `e2e-baseline.sh` | `PAPERCLIP_E2E_REPORT` | 启动 checks + boot_time |
| `perf-baseline.sh` | `PAPERCLIP_PERF_REPORT` | 性能 metrics + asserts |
| `long-run-5min.sh` | `PAPERCLIP_LONGRUN_REPORT` | 长跑 metrics + asserts |

所有脚本共享 schema（`version` / `timestamp` / `metrics` / `checks` / `asserts`）。

## 3. 验收清单

- [x] JSON 输出路径可配置 ✅
- [x] 与 e2e/perf schema 兼容 ✅
- [x] 包含 duration / heartbeat / p50/p99 / RSS ✅
- [x] 不破坏 stdout 输出 ✅
- [x] exit code 保持 ✅
