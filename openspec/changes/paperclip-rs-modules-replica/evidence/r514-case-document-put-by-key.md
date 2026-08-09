# R514 — `PUT /api/cases/:case_id/documents/:key` 完整 Node 语义

## 范围

按 Node 端 `server/src/routes/cases.ts:934` (lock + documentRevisions + caseEvent) 的
8 步事务语义复刻到 Rust。

## 代码变更

| 文件 | 变化 | 行数 |
|---|---|---|
| `crates/pc-repos/src/case.rs` | 新增 `CaseDocumentUpsertInput`/`Result`/`Error` + `ExistingCaseDocumentRow` + `upsert_case_document_by_key` | +230 |
| `crates/pc-repos/src/case.rs` | 修复 `lock_document` / `unlock_document` 两个 pre-existing bug | +50/-25 |
| `crates/pc-repos/src/case.rs` | 移除重复定义的 `DocumentRevisionRow` (改用 `pc_repos::document::DocumentRevisionRow`); 更新 `list_document_revisions` 的 SELECT 列 | +5/-12 |
| `crates/pc-http/src/routes/cases.rs` | 新增 `put_case_document_by_key` handler; 路由 `/api/cases/:case_id/documents/:key` 增加 `.put(...)` | +85 |
| `crates/pc-http/src/routes/cases.rs` | 新增 `UpsertCaseDocumentByKeyBody` struct (与原 `UpsertCaseDocumentBody` 共存 — 老的 link-by-id 流程未动) | +12 |
| `crates/pc-http/tests/r513_company_subresource_create_contract.rs` | 4 个 R514 契约测试 (`insert_case` fixture status 'todo' → 'draft' 修正) | +120 |

## 单事务 8 步 (Node 对齐)

1. `pg_advisory_xact_lock(hashtext('paperclip:case-document:<co>:<case>:<key>'))` — 串行化
2. SELECT `case_documents` JOIN `documents` LEFT JOIN `document_revisions` — 拿 existing
3. 校验 4 个 conflict 分支：
   - `existing.document.locked_at IS NOT NULL` → 409 `Document is locked`
   - `existing != None && baseRevisionId == None` → 409 `Case document was updated by someone else`
   - `existing != None && baseRevisionId != existing.latest_revision_id` → 409 同上
   - `existing == None && baseRevisionId != None` → 409 同上
4. INSERT 或 UPDATE `documents` (`latest_revision_number + 1` 在 update 路径)
5. INSERT `document_revisions` (`title`/`format`/`body`/`change_summary` + `created_by_*_id` + `run_id`)
6. UPDATE `documents` 写 `latest_body` / `latest_revision_id` / `latest_revision_number`
7. INSERT 或 UPDATE `case_documents` (按 key 唯一)
8. INSERT `case_events` kind=`document_revised` 含 `{key, documentId, revisionId, revisionNumber}`

## 修复的 pre-existing bug (R109 遗留)

| Bug | 现象 | 修复 |
|---|---|---|
| `lock_document` 不写 `documents.locked_at` | 文档从未真正被锁，PUT 不会 409 | UPDATE `documents SET locked_at = COALESCE(locked_at, now())` |
| `lock_document` / `unlock_document` 写非法 case_event kind | 触发 `case_events_kind_check` 违反 → 500 | 删除 `INSERT case_events` (Node 端本身就不写) |

## 契约测试 (4 个 R514)

| 测试 | 校验点 |
|---|---|
| `case_document_put_creates_new_document` | PUT 200 + `latestRevisionNumber==1` + GET 返回 `documentId` |
| `case_document_put_update_increases_revision_number` | 两次 PUT, `revision_number` 严格递增 |
| `case_document_put_rejects_stale_base_revision` | 写两次后, 第三次带错误 `baseRevisionId` → 409 |
| `case_document_put_rejects_locked_document` | 先 POST `/lock` (200), 再 PUT → 409 |

## 验证

- R513 + R514: **9/9 passed** (1 suite, 0.07s)
- pc-http lib: **274 passed** (1 suite, 0.01s)
- pc-repos lib: **588 passed** (1 suite, 0.51s)
- Route coverage: **98.28% → 98.45%** (missing 10 → 9)
- E2E: **17/17 passed** (7.7s)

## 提交

- `cea5e97` feat(M22-case-docs): R514 — PUT /api/cases/:case_id/documents/:key 完整 Node 语义

## 下一步候选 (R515+)

| 优先级 | 任务 | 理由 |
|---|---|---|
| 高 | `DELETE /api/labels/:id` + `DELETE /api/secrets/:id` | 真缺漏, 简单 |
| 高 | `GET /api/companies/:id/search/extract` | 真缺漏, 涉及全文搜索 |
| 中 | `GET /api/companies/` + `POST /api/companies/` (trailing slash) | 真实但只是 normalization |
| 中 | `GET /api/_plugins/:id/ui/*filePath` | plugin UI 静态文件 (低优先) |
| 低 | Bootstrap flow / 自动 workspace 初始化 | 跨多模块 |
| 低 | UI 类型生成 (前端 openapi/zod) | 跨前后端 |
