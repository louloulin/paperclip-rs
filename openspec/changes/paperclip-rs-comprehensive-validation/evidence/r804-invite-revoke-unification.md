# R804 - InviteRepo::revoke 返回类型统一

日期: 2026-08-18
范围: pc-repos::invite + pc-invite + pc-http::routes::companies::revoke_invite
方法: R793 原则 (UPDATE...RETURNING + RepoError::NotFound)

## 1. 改动概要

| 方法 | 改动前 | 改动后 | 跨层传播 |
|---|---|---|---|
| InviteRepo::revoke | RepoResult<bool> | RepoResult<InviteRow> | InviteService::revoke + HTTP revoke_invite + e2e |
| InviteService::revoke | InviteResult<bool> | InviteResult<InviteRow> | e2e_invite_service.rs |

## 2. 关键设计要点

- UPDATE...RETURNING: SET revoked_at = now() WHERE company_id AND id AND revoked_at IS NULL
- 已撤销的 invite 也返回 NotFound (idempotent via error)
- HTTP 层加 LiveEvent 广播 (invite.revoked)
- e2e 测试: assert row.id 匹配; 第二次撤销断言 is_err()

## 3. 验证结果

- cargo build -p pc-invite: 通过
- cargo build -p pc-http: 通过 (29.30s)
- cargo test -p pc-invite --lib: 34 passed
- cargo test -p pc-repos --lib: 533 passed
- Rust server: /health + 7 API 200

## 4. 累计 (R756 → R804)

- 12 个跟踪 crate lib 测试: ~1410 PASS (pc-invite 34)
- 整体加权进度: ~99%

## 5. R805+ 计划

- R805: company_skill_policy.rs::delete + execution.rs update_name 等剩余 bool → T
- R806: 审查 pc-feedback::RedactionService 等 service 检查
- R807+: 继续审计纯化 (拆分 pure/db)
- Adapter 15 个永久跳过 (硬约束 #2)