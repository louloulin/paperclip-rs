# R805-R806 - TeamInstall/Skill archive+soft_delete 返回类型统一

日期: 2026-08-18
范围: pc-repos::{team_install, skill}
方法: R793 原则 (DELETE/UPDATE...RETURNING + RepoError::NotFound)

## 1. 改动概要

| 方法 | 改动前 | 改动后 |
|---|---|---|
| TeamInstallRepo::delete | sqlx::Result<bool> | sqlx::Result<TeamInstallRow> |
| SkillRepo::archive | RepoResult<bool> | RepoResult<CompanySkillRow> |
| SkillRepo::soft_delete | RepoResult<bool> | RepoResult<CompanySkillRow> |

## 2. 关键设计要点

- archive/soft_delete: UPDATE...RETURNING + WHERE deleted_at IS NULL 检查
- 重复调用 (already archived/deleted): RepoError::NotFound
- TeamInstallRow 只有 4 列 (catalog_id, status, snapshot, installed_at)
- CompanySkillRow 有 36 列; RETURNING 包含所有

## 3. 验证结果

- cargo build -p pc-repos: 通过 (4.45s)
- cargo build -p pc-server: 通过 (29.67s)
- cargo test -p pc-repos --lib: 533 passed
- round125_skill_basic_repo 集成测试: 失败 (line 12 db() 函数 DB 连接 — 预先存在的基础设施问题，硬约束 #5 不修)
- Rust server: /health + 8 API 200

## 4. 累计 (R756 → R806)

- 12 个跟踪 crate lib 测试: ~1410 PASS
- 整体加权进度: ~99%

## 5. R807+ 计划

- R807: execution.rs::update_name / set_status_to_reconciling / clear_provider_ref
- R808: auth.rs::delete_account / revoke_api_key / update_user_*
- R809: 继续审计其他 Repo (skill 的 variant 等)
- R810+: 继续审计纯化 (拆分 pure/db)
- Adapter 15 个永久跳过 (硬约束 #2)