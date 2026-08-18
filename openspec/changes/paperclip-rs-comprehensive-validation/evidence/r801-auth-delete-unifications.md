# R801 - AuthRepo delete/delete_session/revoke_session_by_token 统一

日期: 2026-08-18
范围: pc-repos::auth + pc-http::routes::auth::sign_out
方法: 持续 R793 原则 (DELETE...RETURNING + RepoError::NotFound)

## 1. 改动概要

| 方法 | 改动前 | 改动后 | 跨层传播 |
|---|---|---|---|
| AuthRepo::delete | RepoResult<bool> | RepoResult<UserRow> | sign_out 等后续 caller |
| AuthRepo::delete_session | RepoResult<bool> | RepoResult<SessionRow> | sign_out 等后续 caller |
| AuthRepo::revoke_session_by_token | RepoResult<bool> | RepoResult<SessionRow> | HTTP sign_out handler (idempotent) |

## 2. sign_out HTTP handler

之前: `deleted = revoke_session_by_token(&token).await? as u64`
之后: `match revoke_session_by_token(...).await { Ok(_) => deleted=1, Err(NotFound)=>{}, Err(e)=>return Err(Internal) }`
优点: idempotent — token 不存在时不报错，符合 sign_out 语义

## 3. 验证结果

- cargo build -p pc-repos: 通过 (7.45s)
- cargo build -p pc-http: 通过 (12.86s)
- cargo build -p pc-server: 通过 (15.31s)
- cargo test -p pc-repos --lib: 533 passed
- cargo test -p pc-auth --lib: 95 passed
- Rust server /health + 8 API: 200

## 4. 累计 (R756 → R801)

- 8 个跟踪 crate lib 测试: ~1283 PASS
- 整体加权进度: ~99% (+0% — 持续统一)

## 5. R802+ 计划

- R802: decision.rs::mark_cancelled 统一
- R803: execution.rs::release_lease / update_name 统一
- R804: invite.rs::revoke 统一
- R805: company_skill_policy.rs::delete 统一
- R806+: 继续审计纯化
- Adapter 15 个永久跳过 (硬约束 #2)