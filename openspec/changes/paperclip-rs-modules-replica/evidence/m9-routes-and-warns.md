# Evidence: M9.1 / M9.2 / M9.3 — 路由冲突清零 + 启动 WARN 清零 + /health 200

## 真实运行最终结果

```text
$ curl -s -w "  status=%{http_code}\n" http://127.0.0.1:53100/health
{"db":{"error":null,"latency_ms":0,"ok":true},"status":"ok","version":"0.1.0"}  status=200

$ grep -cE "WARN" /tmp/s.err
0

$ grep "WARN" /tmp/s.err | head -3
（空）

# access log
INFO http access method=GET path=/health status=200 duration_ms=0
```

✅ **M2.B（/health 200）+ M9.1（路由冲突清零）+ M9.2（启动 WARN 清零）全部真实验证通过**。

---

## 修改清单（M9.1 / M9.2）

| 改动 | 文件 | 真实根因 |
|---|---|---|
| 移除 `/api/companies/:company_id/labels` 重复注册 | `crates/pc-http/src/routes/issues.rs` | Round 282 dedupe 残留；labels.rs 已是 canonical |
| 移除 `PATCH /api/agents/:agent_id/budgets` 重复注册 | `crates/pc-http/src/routes/costs.rs` | agents.rs 已是 canonical |
| 移除 `/api/issues/:id/tree-holds/:hold_id` 重复注册 | `crates/pc-http/src/routes/issue_tree_control.rs` | issues.rs 已是 canonical，但 axum 0.7 把 `tree-holds/:hold_id` 与现有 `tree-holds/:hold_id/release` 视为 prefix 冲突 |
| 重命名 `:id` → `:issue_id` 在 issue_tree_control.rs | `crates/pc-http/src/routes/issue_tree_control.rs` | axum 0.7 不允许同 path 不同名占位符 |
| 移除 `/api/issues/:issue_id/tree-control/{preview,state}` 重复 | `crates/pc-http/src/routes/issue_tree_control.rs` | issues.rs 已是 canonical（Round 27/236） |
| 移除 `/api/issues/:issue_id/checkout` 重复 | `crates/pc-http/src/routes/issues.rs` | issues_checkout_wakeup.rs 已是 canonical |
| 移除 `/api/issues/:id/heartbeat-context` 重复 | `crates/pc-http/src/routes/extensions.rs` | issues.rs 已是 canonical，axum 不允许 `:id`/`:issue_id` 混用 |
| 重写 status card scheduler SQL 为 raw string | `crates/pc-repos/src/status_card.rs` | `\\` 转义被 PG 当作字符串继续符导致语法错 |
| 重写 issue monitor `RETURNING` 列从 IssueRow struct 自动生成 | `crates/pc-repos/src/issue.rs` | 旧 `RETURNING {ISSUE_COLS}` 引入了不存在列 `company_url_key` 等 |
| E2E 脚本固定 `LC_ALL=C` | `scripts/e2e-baseline.sh` | PG 在中文 locale 下 postmaster 多线程启动失败 |

---

## 完整 server 启动序列（截取最终真实日志）

```
INFO pc_telemetry: telemetry initialized service=paperclip-server
INFO pc_telemetry: startup banner service=paperclip-server version=0.1.0
INFO pc_db::pool: db connected attempt=1 max=16 min=1
INFO paperclip_server: heartbeat run recovery complete recovered=0 deferred=0
INFO paperclip_server: storage: local_disk provider registered root=/Users/louloulin/.paperclip/storage
INFO paperclip_server: feature flags: registered 2 default flags
INFO paperclip_server: plugin workers bootstrapped count=0
INFO paperclip_server: http listening host=127.0.0.1 port=53100
INFO pc_http::middleware::access_log: http access method=GET path=/health status=200 duration_ms=0
```

无任何 WARN，无 panic。

---

## 验证可重复性

`scripts/e2e-baseline.sh` 端到端脚本现在可在干净 macOS + 临时 PG16 上 0 错误跑通：

```
[e2e] init pg data dir at /var/folders/nj/.../pc-e2e-pgdata-XXXXX
[e2e] start pg on :55432
[e2e] run pc-migrate up
[e2e] count tables
[e2e] table count = 172
[e2e] start pc-server on :53100
[e2e] final /health status = 200
[e2e] /health body = {"db":{"error":null,"latency_ms":0,"ok":true},"status":"ok","version":"0.1.0"}
[e2e] PASS
```

---

## 结论

**M2.B ✅ / M9.1 ✅ / M9.2 ✅** 全部真实通过。可作为后续 M3–M16 全部模块的回归基线。