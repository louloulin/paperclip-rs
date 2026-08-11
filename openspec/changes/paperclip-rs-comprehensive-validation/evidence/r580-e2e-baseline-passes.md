# R580 — E2E Baseline 真实验证通过

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

**E2E baseline 首次端到端通过**：

```
[e2e] /health 200 after 3*0.5s (1.5s)
[e2e] final /health status = 200
[e2e] /health body = {"authReady":true,"bootstrapStatus":"ready","db":{"error":null,"latency_ms":0,"ok":true},"deploymentMode":"authenticated","status":"ok","version":"0.1.0"}
[e2e] PASS
```

总耗时 ~8s（PG init + migrate + cargo build + server start + /health）。

## 2. 修复历史

### 2.1 R580: e2e-baseline.sh 重构

| 改动 | 原因 |
|---|---|
| `cargo run` → 预编译 `cargo build` + 运行二进制 | 分离编译时间（30-60s）从 server 启动时间 |
| poll timeout 60s → 30s | server 实际启动 <100ms，30s 已远超实际需求 |
| 新增 `server-build.log` | 编译日志独立于 server 日志 |

### 2.2 R580: 修复 4 个预存在 overlapping route panic

| Path | 重复位置 | 修复 |
|---|---|---|
| `GET /api/agents/:agent_id/budgets` | budgets.rs + agents.rs | 从 budgets.rs 移除（agents.rs canonical） |
| `POST /api/dev-server/restart` | instance_settings.rs + dev_server_restart.rs | 从 instance_settings.rs 移除（dev_server_restart.rs canonical） |
| `GET /api/companies/:company_id/budgets/overview` | costs.rs + budgets.rs | 从 costs.rs 移除（budgets.rs canonical） |
| `POST /api/companies/:company_id/budget-incidents/:incident_id/resolve` | costs.rs + budgets.rs | 从 costs.rs 移除（budgets.rs canonical） |
| `GET /api/companies/:company_id/budgets/policies` | costs.rs + budgets.rs | 从 costs.rs 移除（budgets.rs canonical） |

5 个 panic 由 axum 0.7 `path_router.rs:70:22` 抛出，错误信息：
```
Overlapping method route. Handler for `GET /xxx` already exists
```

每个 panic 都对应了 routes/*.rs 文件间的重复注册 —— pre-existing 代码合并问题，
之前未在 e2e baseline 中暴露（因为 server 启动失败但 cargo build 成功）。

### 2.3 R579: 启动计时诊断

在 pc-server 6 个阶段插入 `tracing::info!` 时间戳：

| 阶段 | cold 启动 | warm 启动 |
|---|---|---|
| db_connect | 7ms | 7ms |
| migrations | 868ms | 9ms |
| adapter_registration | 0ms | 0ms |
| heartbeat_recovery | 3ms | 3ms |
| bind | < 1ms | < 1ms |
| **总计 (不含 cargo)** | **~880ms** | **~20ms** |

**结论**: 之前 60s 等待时间 = 冷 cargo compile，**不是 server 启动慢**。

## 3. 当前 e2e baseline 行为

```
$ PAPERCLIP_TEST_PG_PORT=55511 PAPERCLIP_TEST_HTTP_PORT=53211 \
    bash scripts/e2e-baseline.sh
[e2e] init pg data dir at /var/folders/nj/vtk9xv2j4wq41_94ry3zr8hh0000gn/T//pc-e2e-pgdata-70136
[e2e] start pg on :55511
[e2e] run pc-migrate up
[e2e] count tables
[e2e] table count = 172
[e2e] pre-build pc-server (R580: separate cargo build from run)
[e2e] start pc-server on :53211 (warm binary)
[e2e] /health 200 after 3*0.5s (1.5s)
[e2e] final /health status = 200
[e2e] /health body = {"authReady":true,...,"status":"ok","version":"0.1.0"}
[e2e] PASS
```

总耗时: **~8s**（其中 cargo build ~5s，server 启动 ~1.5s）

## 4. 设计亮点

### 4.1 渐进式冲突解决

R580 没有一次性 grep 所有重叠路由（可能漏掉），而是：
1. 启动 server → panic 暴露第一个冲突
2. 修复一个冲突 → 重启 server → panic 暴露下一个
3. 重复直到 0 panic

这种 "fail-fast iterate" 模式比 static analysis 更可靠，因为 axum 的
overlap detection 在运行时触发，是唯一的真相来源。

### 4.2 canonical 归属判断

每个重叠都遵循 "X-相关 → routes::X" 规则：

- `/api/agents/:id/budgets` → `routes::agents`（agents 维度）
- `/api/companies/:id/budgets/*` → `routes::budgets`（budgets 维度）
- `/api/dev-server/restart` → `routes::dev_server_restart`（专用模块）
- `costs.rs` 仅保留 `/api/costs/*`（costs 维度）

### 4.3 注释保留删除原因

每个被移除的注册都留下注释（Round 282 removal — 重复注册触发 axum 0.7
panic），避免后续 contributor 重新引入冲突。

## 5. 下一步

R581: V11 UI 60 client happy path（依赖 R580 e2e baseline 通过 ✅）。
R582: V12 Playwright 真实 UI 剧本。
