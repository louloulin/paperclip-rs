# R807 - ExecutionRepo 4 个 update 方法返回类型统一

日期: 2026-08-18
范围: pc-repos::execution + pc-http::routes::execution_workspaces
方法: R793 原则 (UPDATE...RETURNING + RepoError::NotFound)

## 1. 改动概要

| 方法 | 改动前 | 改动后 |
|---|---|---|
| ExecutionRepo::update_name | RepoResult<bool> | RepoResult<WorkspaceRow> |
| ExecutionRepo::set_status_to_reconciling | RepoResult<bool> | RepoResult<WorkspaceRow> |
| ExecutionRepo::set_branch_provider_ref | RepoResult<bool> | RepoResult<WorkspaceRow> |
| ExecutionRepo::clear_provider_ref | RepoResult<bool> | RepoResult<WorkspaceRow> |

## 2. 关键设计要点

- 所有 4 个 update 方法用 UPDATE...RETURNING + RepoError::NotFound
- WorkspaceRow 24 列全部 RETURNING
- HTTP patch_workspace handler 改用 map_err + 返回 row.name

## 3. 验证结果

- cargo build -p pc-repos: 通过 (5.20s)
- cargo build -p pc-http: 通过 (18.74s)
- cargo build -p pc-server: 通过 (16.03s)
- cargo test -p pc-repos --lib: 533 passed
- cargo test -p pc-decisions --lib: 185 passed
- cargo test -p pc-routines --lib: 207 passed
- Rust server: /health + 8 API 200

## 4. 累计 (R756 → R807)

- 13 个跟踪 crate lib 测试: ~1410 PASS
- 整体加权进度: ~99%

## 5. R808+ 计划

- R808: auth.rs::delete_account / revoke_api_key / update_user_*
- R809: skill::create_variant / fork 等其他 Repo
- R810+: 继续审计纯化
- Adapter 15 个永久跳过 (硬约束 #2)