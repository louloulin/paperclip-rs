# R808 - AuthRepo 多个 bool 方法返回类型统一

日期: 2026-08-18
范围: pc-repos::auth + pc-http::routes::auth::revoke_api_key
方法: R793 原则 (DELETE/UPDATE...RETURNING + RepoError::NotFound)

## 1. 改动概要

| 方法 | 改动前 | 改动后 |
|---|---|---|
| AuthRepo::delete_account | RepoResult<bool> | RepoResult<AccountRow> |
| AuthRepo::consume_verification | RepoResult<bool> | RepoResult<VerificationRow> |
| AuthRepo::revoke_api_key | RepoResult<bool> | RepoResult<BoardKeyRow> |
| AuthRepo::update_user_name | RepoResult<bool> | RepoResult<UserRow> |
| AuthRepo::update_user_image | RepoResult<bool> | RepoResult<UserRow> |

## 2. 关键设计要点

- 所有 5 个 bool 方法批量统一为返回具体 Row
- BoardKeyRow 从 crate::board_key 导入
- HTTP revoke_api_key handler 加 LiveEvent (auth.api_key.revoked)

## 3. 验证结果

- cargo build -p pc-repos: 通过 (8.21s)
- cargo build -p pc-http: 通过 (1m 03s)
- cargo build -p pc-server: 通过 (14.85s)
- cargo test -p pc-repos --lib: 533 passed
- cargo test -p pc-auth --lib: 95 passed
- Rust server: /health + 9 API 200

## 4. 累计 (R756 → R808)

- 14 个跟踪 crate lib 测试: ~1450 PASS
- 整体加权进度: ~99.2%

## 5. R809+ 计划

- R809: skill::create_variant / fork / 其他剩余 bool
- R810: audit remaining auth/skill execution methods
- R811+: 继续审计纯化
- Adapter 15 个永久跳过 (硬约束 #2)