# R809 - Company/Auth 多个 update 方法返回类型统一

日期: 2026-08-18
范围: pc-repos::{company,auth} + pc-companies + pc-http::routes
方法: R793 原则 (UPDATE...RETURNING + RepoError::NotFound/sqlx::RowNotFound)

## 1. 改动概要

| 方法 | 改动前 | 改动后 |
|---|---|---|
| CompanyRepo::set_logo_url | sqlx::Result<bool> | sqlx::Result<CompanyRow> |
| AuthRepo::set_email_verified | RepoResult<bool> | RepoResult<UserRow> |
| AuthRepo::extend_session | RepoResult<bool> | RepoResult<SessionRow> |
| CompanyService::set_logo_url | CompanyServiceResult<bool> | CompanyServiceResult<CompanyRow> |

## 2. 关键设计要点

- set_logo_url: UPDATE companies SET logo_url = $1 (返回完整 CompanyRow)
- set_email_verified: UPDATE user SET email_verified=true (返回完整 UserRow)
- extend_session: UPDATE session WHERE expires_at > now() (返回完整 SessionRow)
- CompanyService 通过 From<sqlx::Error> 自动转换

## 3. 验证结果

- cargo build -p pc-repos: 通过 (4.13s)
- cargo build -p pc-companies: 通过
- cargo build -p pc-http: 通过 (16.68s)
- cargo build -p pc-server: 通过 (11.73s)
- cargo test -p pc-repos --lib: 533 passed
- cargo test -p pc-companies --lib: 49 passed
- Rust server: /health + 9 API 200

## 4. 累计 (R756 → R809)

- 14 个跟踪 crate lib 测试: ~1460 PASS
- 整体加权进度: ~99.3%

## 5. R810+ 计划

- R810: skill::delete_comment / create_variant / fork
- R811: folder::delete_legacy (no callers — 待评估删除)
- R812+: 继续审计纯化
- Adapter 15 个永久跳过 (硬约束 #2)