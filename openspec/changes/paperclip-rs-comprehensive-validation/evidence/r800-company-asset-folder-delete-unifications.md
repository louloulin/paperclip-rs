# R800 - CompanyRepo/AssetRepo/FolderRepo delete 返回类型统一

日期: 2026-08-18
范围: pc-repos::{company,asset,folder} + pc-companies + pc-folders + pc-http::routes::{assets,companies}
方法: 持续 R793 原则 (DELETE...RETURNING + NotFound 错误语义)

## 1. 改动概要

| 方法 | 改动前 | 改动后 | 跨层传播 |
|---|---|---|---|
| CompanyRepo::delete | sqlx::Result<bool> | sqlx::Result<CompanyRow> | CompanyService::remove + HTTP remove + LiveEvent |
| AssetRepo::delete_by_id | sqlx::Result<bool> | sqlx::Result<AssetRow> | AssetService::delete_by_id + HTTP asset delete + LiveEvent |
| FolderRepo::delete | RepoResult<bool> | RepoResult<FolderRow> (RepoError::NotFound on miss) | FolderService::delete + HTTP delete_folder + LiveEvent |
| CompanyService::remove | CompanyServiceResult<bool> | CompanyServiceResult<CompanyRow> | 内部 API |
| FolderService::delete | Result<bool> | Result<FolderRow> | 内部 API |
| AssetService::delete_by_id | AssetResult<bool> | AssetResult<AssetRow> | 内部 API |

## 2. 验证结果

- cargo build -p pc-repos: 通过 (7.43s)
- cargo build -p pc-companies: 通过
- cargo build -p pc-folders: 通过 (0.94s)
- cargo build -p pc-http: 通过 (14.94s)
- cargo build -p pc-server: 通过 (14.08s)
- cargo test -p pc-repos --lib: 533 passed
- cargo test -p pc-decisions --lib: 185 passed
- cargo test -p pc-routines --lib: 207 passed
- cargo test -p pc-goals --lib: 6 passed
- cargo test -p pc-companies --lib: 49 passed
- cargo test -p pc-folders --lib: 10 passed
- cargo test -p pc-issues --lib: 198 passed
- Rust server /health + 6 API: 200

## 3. 累计 (R756 → R800)

- 7 个跟踪 crate lib 测试: 1188 PASS
- 整体加权进度: ~99%

## 4. R801+ 计划

- R801: 审计 auth.rs (delete_session/revoke/delete_user 都是 bool → T)
- R802: decision.rs::mark_cancelled 统一
- R803: execution.rs::release_lease / update_name 统一
- R804: invite.rs::revoke 统一
- R805+: 继续审计纯化
- Adapter 15 个永久跳过 (硬约束 #2)