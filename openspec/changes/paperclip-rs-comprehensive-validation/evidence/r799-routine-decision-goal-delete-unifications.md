# R799 - RoutineRepo / DecisionRepo / GoalRepo delete 返回类型统一

日期: 2026-08-18
范围: pc-repos::{routine,decision,goal} + pc-routines + pc-decisions + pc-goals + pc-http::routes::{routines,decisions,goals}
方法: 持续 R793 原则 (DELETE...RETURNING + NotFound 错误语义)

## 1. 改动概要

| 方法 | 改动前 | 改动后 | 跨层传播 |
|---|---|---|---|
| RoutineRepo::delete | sqlx::Result<bool> | sqlx::Result<RoutineRow> | RoutineService::delete + HTTP remove + LiveEvent |
| RoutineRepo::delete_trigger | sqlx::Result<bool> | sqlx::Result<RoutineTriggerRow> | HTTP layer (待跟进) |
| DecisionRepo::delete | sqlx::Result<bool> | sqlx::Result<DecisionRow> | DecisionService::delete + HTTP remove + LiveEvent |
| GoalRepo::delete | RepoResult<bool> | RepoResult<GoalRow> (RepoError::NotFound on miss) | GoalService::delete + HTTP layer |
| GoalRepo::delete_one | RepoResult<bool> | RepoResult<GoalRow> | HTTP layer + LiveEvent |
| RoutineService::delete | Result<bool> | Result<RoutineRow> | 内部 API |
| GoalService::delete | Result<bool> | Result<GoalRow> | 内部 API |

## 2. 关键设计要点

- 所有 delete 改用 DELETE...RETURNING + fetch_optional + .ok_or(RowNotFound)
- RepoError 用户使用 RepoError::NotFound { entity, id } 替代 sqlx::Error::RowNotFound
- ServiceError 用户使用 From<RepoError> 自动转换 (pc-decisions: From<pc_repos::RepoError>)
- HTTP 层统一: RowNotFound → ApiError::NotFound
- 删除后发 LiveEvent (routine.removed / decision.removed / goal.removed)

## 3. 验证结果

| 项 | 结果 |
|---|---|
| cargo build -p pc-repos | 通过 (7.12s) |
| cargo build -p pc-routines | 通过 |
| cargo build -p pc-decisions | 通过 (1.01s) |
| cargo build -p pc-goals | 通过 (0.57s) |
| cargo build -p pc-http | 通过 |
| cargo build -p pc-server --bin paperclip-server | 通过 (20.02s) |
| cargo test -p pc-repos --lib | 533 passed |
| cargo test -p pc-decisions --lib | 185 passed |
| cargo test -p pc-routines --lib | 207 passed |
| cargo test -p pc-goals --lib | 6 passed |
| Rust server /health | 200 |
| /openapi.json | 200 |
| /api/{companies,routines,decisions,goals} | 200 |

## 4. 累计 (R756 → R799)

- 32 跟踪 crate lib 测试: ~3593 PASS (pc-goals 6 passed)
- 整体加权进度: ~99% (+0.5% from 多个 repo delete 统一)

## 5. R800+ 计划

- R800: pc-repos::delete_read_state (read_state 是 read model，可能用 () 即可)
- R801: pc-feedback::RedactionService 等 service 检查
- R802: pc-decisions / pc-routines / pc-goals 是否有其他 mutation 返回 bool
- R803+: 继续审计纯化 (audit + 拆分 pure/db)
- Adapter 15 个永久跳过 (硬约束 #2)