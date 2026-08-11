# R594 — e2e-baseline 增加 JSON 报告输出（CI 友好）

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

`scripts/e2e-baseline.sh` 增加 JSON 报告输出：
- 文件路径：`$PAPERCLIP_E2E_REPORT`（默认 `/tmp/paperclip-e2e-report.json`）
- 包含：checks（pg_ready / migrate / tables / server_built / health_200）/ metrics（boot_time_polls / boot_time_s）
- 用途：CI 集成 + 启动历史趋势

## 2. 真实运行结果（R594 实测）

```
[e2e] /health 200 after 2*0.5s (1.0s)
[e2e] report → /tmp/test-e2e-report.json
[e2e] PASS
```

JSON 内容：
```json
{
  "version": "1",
  "timestamp": "2026-08-11T18:47:46Z",
  "checks": {
    "pg_ready": true,
    "migrate": true,
    "tables": 172,
    "server_built": true,
    "health_200": true
  },
  "metrics": {
    "boot_time_polls": 2,
    "boot_time_s": 1.0
  }
}
```

## 3. 与 perf-baseline 协同

| 脚本 | 输出 | 用途 |
|---|---|---|
| `e2e-baseline.sh` | JSON checks + boot_time | 启动冒烟 |
| `perf-baseline.sh` | JSON metrics + asserts | 性能基线 |

两个脚本的 JSON schema 兼容（都有 `version` / `timestamp` / `metrics` / `checks`），方便 CI 统一解析。

## 4. 关键设计决策

### 4.1 检查项而非指标为主

e2e-baseline 是「能不能起来」测试，不是「性能怎么样」测试。所以 JSON 突出 checks 段：
- pg_ready：临时 PG 启动
- migrate：172 表迁移
- server_built：cargo build 成功
- health_200：server /health 端点响应

### 4.2 启动时间以 polls 为单位

保留 `boot_time_polls`（整数）和 `boot_time_s`（小数）双格式，便于：
- `polls` 在 CI 日志里稳定（不依赖 bc locale）
- `s` 用于图表绘制

## 5. 验收清单

- [x] JSON 输出路径可配置 ✅
- [x] checks 5 项 + metrics 2 项 ✅
- [x] 与 perf-baseline schema 兼容 ✅
- [x] 不破坏 stdout 人类可读输出 ✅
- [x] exit code 保持（0/1）✅
