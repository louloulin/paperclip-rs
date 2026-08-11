# R593 — perf-baseline 增加 JSON 报告输出（CI 友好）

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

`scripts/perf-baseline.sh` 增加 JSON 报告输出：
- 文件路径：`$PAPERCLIP_PERF_REPORT`（默认 `/tmp/paperclip-perf-report.json`）
- 包含：metrics / asserts / node_baseline
- 用途：CI 直接消费 + 历史趋势对比

## 2. 真实运行结果（R593 实测）

```
[perf] report → /tmp/test-perf-report.json
[perf] PASS ✅
  - /health p99 4ms < 30ms target
  - max RSS 56MB < 100MB target
  - business endpoints 6/6 OK
```

JSON 内容：
```json
{
  "version": "1",
  "timestamp": "2026-08-11T18:46:38Z",
  "metrics": {
    "boot_ms": 1047,
    "health_p50_ms": 3,
    "health_p99_ms": 4,
    "rss_mb": 56.1,
    "threads":       17,
    "business_endpoints_ok": 6,
    "business_endpoints_total": 6
  },
  "asserts": {
    "p99_ok": 1,
    "rss_ok": 1,
    "biz_pass": 1
  },
  "node_baseline": {
    "boot_ms": 3000,
    "health_p99_ms": 80,
    "rss_mb": 250
  }
}
```

## 3. 关键设计决策

### 3.1 JSON 输出路径可配置

`PAPERCLIP_PERF_REPORT` 环境变量覆盖默认路径，方便：
- CI runner 持久化到 artifact
- 本地开发覆盖到 `/tmp/<random>.json`

### 3.2 结构化字段

- `version`：未来 schema 演进时兼容
- `timestamp`：ISO 8601 UTC（方便排序）
- `metrics`：原始测量值
- `asserts`：布尔结果（便于 CI 决策）
- `node_baseline`：Node 上游参考值（自描述）

### 3.3 与人类输出共存

保持原有的 stdout 输出（人读），同时输出 JSON（机器读）。两者不冲突。

## 4. 使用场景

### 4.1 CI 集成

```yaml
- name: Perf baseline
  run: |
    PAPERCLIP_PERF_REPORT=$RUNNER_TEMP/perf.json \
      bash scripts/perf-baseline.sh
    cat $RUNNER_TEMP/perf.json >> perf-history.jsonl

- name: Upload perf report
  uses: actions/upload-artifact@v4
  with:
    name: perf-report
    path: perf.json
```

### 4.2 历史趋势

```bash
# 累积 JSON Lines 历史
for i in 1 2 3; do
  bash scripts/perf-baseline.sh  # append 到历史
done

# Plot with Python
python3 -c "
import json
data = [json.loads(l) for l in open('perf-history.jsonl')]
print('boot_ms:', [d['metrics']['boot_ms'] for d in data])
"
```

### 4.3 Slack 通知

```bash
PAPERCLIP_PERF_REPORT=report.json bash scripts/perf-baseline.sh
jq -r '.metrics | "Boot: \(.boot_ms)ms | p99: \(.health_p99_ms)ms | RSS: \(.rss_mb)MB"' report.json
```

## 5. 验收清单

- [x] JSON 输出路径可配置（`PAPERCLIP_PERF_REPORT`）✅
- [x] 包含 metrics / asserts / node_baseline 三段 ✅
- [x] ISO 8601 UTC timestamp ✅
- [x] 不破坏人类 stdout 输出 ✅
- [x] 与 R592 4 重断言协同 ✅
- [x] 退出码保持（0/1）✅
