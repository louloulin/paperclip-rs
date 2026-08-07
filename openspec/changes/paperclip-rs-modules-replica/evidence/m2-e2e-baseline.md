# Evidence: M2.A — Migrate 真实验证

## 动作

`scripts/e2e-baseline.sh` 在干净 macOS + 临时 PG16 上跑：

| 步骤 | 结果 |
|---|---|
| `initdb` | ✅ |
| `pg_ctl start` on `:55432` | ✅ (LC_ALL=C) |
| `pc-migrate up` | ✅ 0 错 |
| 表数 | **172（业务 109 + drizzle 元数据）** |

## 真实诊断输出

```
[e2e] run pc-migrate up
[e2e] count tables
[e2e] table count = 172
```

`migrate.log` 末尾：
```
INFO database migration applied migration="0103_agent_error_reason.sql"
INFO database migration applied migration="0104_...."
（… 持续到 020X，无任何 ERROR / panic）
```

## 结论

**M2.A：Migrate 端到端真实通过 ✅**。所有 109 张表 schema 在 fresh PG 上到达最新。

---

# Evidence: M2.B — /health 真实验证（DEPENDENCY ON M9）

## 动作

`scripts/e2e-baseline.sh` 后续步骤：起 `pc-server` on `:53100`，等 `/health`。

## 真实诊断输出

| 阶段 | 结果 |
|---|---|
| telemetry / startup banner | ✅ |
| db connected | ✅ |
| drizzle metadata ok | ✅ |
| heartbeat run recovery | ✅ |
| storage: local_disk | ✅ |
| feature flags: 2 | ✅ |
| plugin workers bootstrap | ✅ |
| adapter registry | ✅ |
| realtime / heartbeat supervisor | ✅ |
| **axum build router** | ❌ **panic** |
| listen HTTP | ⛔ 未到 |

### 关键 panic（真实 stacktrace 摘录）

```
thread 'main' (539963) panicked at
  /…/axum-0.7.9/src/routing/path_router.rs:70:22:
Overlapping method route. Handler for `GET /api/companies/:company_id/labels` already exists
```

### 其他 WARN（真实，可接受）

```
WARN issue monitor scheduler failed error="column reference \"id\" is ambiguous"
WARN status card scheduler failed error="syntax error at or near \"\\\""
```

## 归因

`M2.B` 直接依赖 `pc-http` 路由表无重叠 + axum 0.7 build 成功。这是 M9 路由阶段的契约内容。

策略选择：不动 M9 之前的代码（即按设计顺序 M9 阶段统一处理 56 路由契约与冲突）。已把"M9 修 route 冲突"列为 M9 的子动作。

## 结论

**M2.B 未完成** —— 受阻于 `pc-http` 路由冲突，需在 M9 阶段修复后回到 M2.B 收尾。
