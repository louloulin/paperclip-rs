# R802-R803 - decision/execution lease 方法统一

日期: 2026-08-18
范围: pc-repos::{decision,execution,environment} + pc-decisions + pc-environment + pc-http::routes::{execution_workspaces}
方法: 持续 R793 原则 (UPDATE/DELETE...RETURNING + RepoError::NotFound)

## 1. 改动概要

| 方法 | 改动前 | 改动后 | 跨层传播 |
|---|---|---|---|
| DecisionRepo::mark_cancelled | sqlx::Result<bool> | sqlx::Result<DecisionRow> | DecisionService::cancel + approval hook |
| ExecutionRepo::release_lease | RepoResult<bool> | RepoResult<LeaseRow> | HTTP release_lease_route |
| EnvironmentRepo::release_lease | RepoResult<bool> | RepoResult<EnvironmentLeaseRow> | EnvironmentService::release_lease + e2e test |
| DecisionService::cancel | bool 中间检查 | DecisionRow (RowNotFound -> NotFound error) | |
| EnvironmentService::release_lease | EnvResult<bool> | EnvResult<EnvironmentLeaseRow> | |

## 2. 关键设计要点

- mark_cancelled: UPDATE...RETURNING 替代 0/1 计数
- release_lease: 状态校验 (state=holding/active) + RETURNING
- 兼容双层: pc-repos::ExecutionRepo (token-校验) 和 pc-repos::EnvironmentRepo (reason)
- HTTP 层错误映射 (NotFound -> 404)

## 3. 验证结果

- cargo build -p pc-decisions: 通过
- cargo build -p pc-environment: 通过 (1.97s)
- cargo build -p pc-http: 通过 (38.16s)
- cargo build -p pc-server: 通过 (48.36s)
- cargo test -p pc-decisions --lib: 185 passed
- cargo test -p pc-repos --lib: 533 passed
- Rust server: /health + 7 API 200

## 4. 累计 (R756 → R803)

- 11 个跟踪 crate lib 测试: ~1399 PASS
- 整体加权进度: ~99% (+0% — 持续统一)

## 5. 磁盘清理

- 增量编译缓存: 28G → 5.5G (清理 632 个旧 incremental dirs)
- 磁盘从 100% → 31% (释放 ~22GB)

## 6. R804+ 计划

- R804: invite.rs::revoke 统一
- R805: company_skill_policy.rs::delete 统一
- R806: continue auditing more bool → T methods
- R807+: 继续审计纯化
- Adapter 15 个永久跳过 (硬约束 #2)