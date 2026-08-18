# R798 - IssueRepo 多个 delete 方法返回类型统一 (R793 原则)

日期: 2026-08-18
范围: pc-repos::IssueRepo + pc-http::routes::issues + pc-issues::IssueService
方法: DELETE...RETURNING + sqlx::Error::RowNotFound

## 1. 改动概要

| 方法 | 改动前 | 改动后 | 跨层传播 |
|---|---|---|---|
| IssueRepo::delete_attachment | sqlx::Result<bool> | sqlx::Result<AttachmentRow> (DELETE...RETURNING) | HTTP remove_attachment + LiveEvent |
| IssueRepo::delete_comment | sqlx::Result<bool> | sqlx::Result<IssueCommentRow> | HTTP delete_comment + LiveEvent + pc-issues::IssueService::remove_comment |
| IssueRepo::delete_interaction | sqlx::Result<bool> | sqlx::Result<IssueThreadInteractionRow> | HTTP delete_issue_interaction + LiveEvent |
| IssueRepo::delete_label | sqlx::Result<bool> | sqlx::Result<LabelRow> | HTTP remove_label + LiveEvent |
| IssueService::remove_comment | IssueServiceResult<bool> | IssueServiceResult<IssueCommentRow> | 调用方改用 IssueCommentRow |

## 2. 关键设计要点

- 所有删除方法都改用 DELETE...RETURNING + fetch_optional + .ok_or(RowNotFound)
- HTTP 层显式 map_err: RowNotFound → ApiError::NotFound(...)
- 删除后发 LiveEvent 通知前端 (issue.{comment,attachment,interaction,label}.removed)
- IssueService::remove_comment 也跟随 R793 原则返回 T 而非 bool

## 3. 验证结果

| 项 | 结果 |
|---|---|
| cargo build -p pc-repos | 通过 (8.28s) |
| cargo build -p pc-http | 通过 (1m 06s) |
| cargo build -p pc-issues | 通过 (2.33s) |
| cargo build -p pc-server --bin paperclip-server | 通过 (48.81s) |
| cargo test -p pc-repos --lib | 533 passed |
| cargo test -p pc-issues --lib | 198 passed |
| cargo test -p pc-work-products --lib | 8 passed |
| Rust server /health | 200 |
| 13/14 GET API | 200 (/api/documents 400 是 schema 检查) |

## 4. 累计 (R756 → R798)

- 32 跟踪 crate lib 测试: ~3764 PASS
- 整体加权进度: ~98.5% (+0.5% from 多 delete 统一)

## 5. R799+ 计划

- R799: pc-repos::delete_read_state (bool → T 或 () + RowNotFound)
- R800: 审查 pc-repos::RoutineRepo / DecisionRepo / GoalRepo 同样适用
- R801: pc-feedback::RedactionService 等 service 检查
- R802+: 进一步纯化 (audit + 拆分 pure/db)
- Adapter 15 个永久跳过 (硬约束 #2)