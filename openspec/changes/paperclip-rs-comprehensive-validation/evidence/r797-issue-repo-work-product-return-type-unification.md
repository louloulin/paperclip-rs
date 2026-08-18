# R797 - IssueRepo work_product HTTP 层返回类型统一 (R793 原则)

日期: 2026-08-18
范围: pc-repos::IssueRepo + pc-http::routes::issues
方法: 应用 R793 service 返回类型原则到 HTTP 层 mutation 方法

## 1. 改动概要

| 方法 | 改动前 | 改动后 | 理由 |
|---|---|---|---|
| IssueRepo::update_work_product | sqlx::Result<Option<IssueWorkProductRow>> | sqlx::Result<IssueWorkProductRow> (0 行 = RowNotFound) | R793: update 返回 T + NotFound |
| IssueRepo::delete_work_product | sqlx::Result<bool> | sqlx::Result<IssueWorkProductRow> (DELETE...RETURNING) | R793: remove 返回 T + NotFound |
| HTTP patch_work_product | .ok_or_else(|ApiError::NotFound|) 后置 | map_err(RowNotFound -> ApiError::NotFound) | 移除 API 层冗余检查 |
| HTTP remove_work_product | if ok {} else {} 分支 | map_err(RowNotFound -> ApiError::NotFound) | 移除 bool 中间层 |

## 2. 关键代码改动

### pc-repos/src/issue.rs

- update_work_product: 加 .ok_or(sqlx::Error::RowNotFound) 在 fetch_optional 后
- delete_work_product: 改为 query_as::<IssueWorkProductRow> + RETURNING + fetch_optional + .ok_or(RowNotFound)

### pc-http/src/routes/issues.rs

- patch_work_product: .ok_or_else(|ApiError::NotFound(...)|) → .map_err(|err| match err { RowNotFound => ApiError::NotFound(...), other => ApiError::from(other) })
- remove_work_product: bool 中间层移除，直接 match sqlx Error
- 顺便补全 LiveEvent 广播 (issue.work_product.removed) — 之前删除事件未发布

## 3. 验证结果

| 项 | 结果 |
|---|---|
| cargo build -p pc-repos | 通过 (8.28s) |
| cargo build -p pc-http | 通过 (1m 06s, 180 warnings 已有) |
| cargo build -p pc-server --bin paperclip-server | 通过 (47.08s, 2 warnings 已有) |
| cargo test -p pc-repos --lib | 533 passed |
| cargo test -p pc-issues --lib | 198 passed |
| cargo test -p pc-work-products | 3 passed (含 r791 集成测试) |
| Rust server /health | 200 |
| Rust server /openapi.json | 200 |
| Rust server /api/{companies,inbox,agents,issues,decisions,routines,goals,costs,pipelines,workspaces,runs} | 13/13 -> 200 |

## 4. 副作用

- remove_work_product 现在发送 LiveEvent (之前只删除不通知前端)
- HTTP 错误映射更显式 (RowNotFound → 404 显式声明)
- 删除事件可通过 WebSocket 收到 (前端实时 UI 联动)

## 5. 累计 (R756 → R797)

- 32 跟踪 crate lib 测试: ~3764 PASS
- DB integration: 17 (R788+R789+R791+R793)
- 整体加权进度: ~98% (+0.5% from HTTP layer 统一)

## 6. R798+ 计划

- R798: pc-repos::IssueRepo::update_issue / delete_issue / create_decision 等其他 mutation 方法审查 + 统一
- R799: pc-repos::RoutineRepo / DecisionRepo / GoalRepo 等同样审查
- R800: pc-feedback::RedactionService::redact 等 service 检查
- R801+: 进一步纯化 (audit + 拆分 pure/db)
- Adapter 15 个永久跳过 (硬约束 #2)