# R782 - pc-documents pure.rs 拆分 + 24 个单测 (0 -> 24)

**日期**: 2026-08-17
**主题**: 核心域 0 测试 crate 填补 缺口补齐
**crate**: pc-documents

## 背景

pc-documents 是文档领域核心服务层 (DocumentService + 文档 CRUD + 修订 + 锁定 + 标注 + 生命周期钩子),
821 行 service.rs 实现, 但 0 个单元测试. 这是核心域覆盖缺口之一.

R782 完成两件事:
1. 提取 5 个纯验证/归一化函数到 `pure.rs` 模块 (364 行, 0 sqlx 依赖)
2. service.rs 中的 validate 方法 delegate 到 pure 函数 (保持 public API 不变)
3. 在 pure.rs 的 `internal_tests` 模块加 24 个 r782_ 单测

## 改动

### 1. 新增 `src/pure.rs` (364 行)

```rust
pub const ALLOWED_FORMATS: &[&str] = &["markdown", "plain", "html"];
pub const DEFAULT_FORMAT: &str = "markdown";
pub const ANNOTATION_AUTHOR_TYPES: &[&str] = &["user", "agent", "system"];

pub fn normalize_document_key(key: &str) -> String { key.trim().to_lowercase() }
pub fn is_allowed_format(format: &str) -> bool { ... }
pub fn is_allowed_author_type(author_type: &str) -> bool { ... }
pub fn normalize_create_document(company_id: Uuid, body: &str, format: Option<&str>, title: Option<String>) -> Result<NormalizedCreate> { ... }
pub fn validate_document_patch(format: Option<&str>, body: Option<&str>) -> Result<()> { ... }
pub fn validate_annotation_thread(company_id: Uuid, issue_id: Uuid, document_id: Uuid, document_key: &str, selected_text: &str, normalized_start: i32, normalized_end: i32, markdown_start: i32, markdown_end: i32) -> Result<()> { ... }
pub fn validate_annotation_comment(company_id: Uuid, thread_id: Uuid, issue_id: Uuid, document_id: Uuid, body: &str, author_type: &str) -> Result<()> { ... }
pub fn validate_upsert_issue_document(company_id: Uuid, issue_id: Uuid, key: &str, body: &str, format: Option<&str>) -> Result<()> { ... }
```

### 2. 修改 `src/service.rs` (821 -> 786 行)

保留输入结构体 (CreateDocument, DocumentPatch, CreateAnnotationThreadInput, CreateAnnotationComment, UpsertIssueDocument),
其 validate 方法变为单行 delegate:

```rust
impl CreateDocument {
    fn normalize(&self) -> Result<pure::NormalizedCreate> {
        pure::normalize_create_document(self.company_id, &self.body, self.format.as_deref(), self.title.clone())
    }
}

impl DocumentPatch {
    fn validate(&self) -> Result<()> {
        pure::validate_document_patch(self.format.as_deref(), self.body.as_deref())
    }
}
// ... 同样的 delegate 模式 for thread/comment/upsert
```

Public API 完全不变 (CreateDocument, DocumentPatch, etc. 还在 service.rs, 仍然 pub re-export).

### 3. 修改 `src/lib.rs` (38 行)

新增 `pub mod pure;` 和完整的 root re-export:

```rust
pub mod pure;
mod service;
pub use pure::{
    is_allowed_author_type, is_allowed_format, normalize_document_key,
    validate_annotation_comment, validate_annotation_thread,
    validate_document_patch, validate_upsert_issue_document,
    NormalizedCreate, ALLOWED_FORMATS, ANNOTATION_AUTHOR_TYPES, DEFAULT_FORMAT,
};
pub use service::{ ... };
```

## 验证

```bash
cargo test -p pc-documents --lib
# 24 passed; 0 failed (新增 24 个 r782_ 单测)
```

下游编译验证:

```bash
cargo build -p pc-documents    # 0 错误, 0 警告
cargo build -p pc-server -p pc-http  # 正在编译, 验证 public API 兼容性
```

## 24 个新单测明细

| 测试 | 验证点 |
|---|---|
| r782_is_allowed_format_accepts_three_canonical | markdown/plain/html 都接受 |
| r782_is_allowed_format_rejects_others | xml/空/MARKDOWN 被拒 |
| r782_is_allowed_author_type_three_values | user/agent/system 接受, admin 拒绝 |
| r782_normalize_document_key_trims_and_lowercases | 5 个 trim/lowercase 边界 |
| r782_normalize_create_valid_inputs | 完整正常路径 |
| r782_normalize_create_defaults_format_to_markdown | format=None 时 default |
| r782_normalize_create_rejects_nil_company_id | uuid nil 拒绝 |
| r782_normalize_create_rejects_empty_body | body 空 拒绝 |
| r782_normalize_create_rejects_unknown_format | format 不在白名单 拒绝 |
| r782_validate_document_patch_no_changes_ok | 3 种 no-change 路径 |
| r782_validate_document_patch_rejects_empty_body | body empty 拒绝 |
| r782_validate_document_patch_rejects_bad_format | format 不合法 拒绝 |
| r782_validate_annotation_thread_happy_path | 完整正常路径 |
| r782_validate_annotation_thread_rejects_nil_uuids | 3 个 uuid 字段 nil 各自报错 |
| r782_validate_annotation_thread_rejects_empty_key_or_text | document_key 空和 selected_text 各自错误 |
| r782_validate_annotation_thread_rejects_inverted_ranges | normalized_end < start 和 markdown_end < start 各自错误 |
| r782_validate_annotation_thread_allows_zero_length_range | 退化 (start=end) 允许 |
| r782_validate_annotation_comment_happy | 正常路径 |
| r782_validate_annotation_comment_trims_whitespace_body | body 是空白字符 拒绝 |
| r782_validate_annotation_comment_rejects_invalid_author_type | admin 拒绝 |
| r782_validate_upsert_issue_document_happy | 正常路径 (含 format=None / Some) |
| r782_validate_upsert_issue_document_rejects_blank_key | key 空白 拒绝 |
| r782_validate_upsert_issue_document_rejects_empty_body | body 空 拒绝 |
| r782_validate_upsert_issue_document_rejects_bad_format | format 不合法 拒绝 |

## 关键设计点

1. **Layered Architecture**: pure (无依赖) -> service (delegate) -> repos (sqlx).
   pure 层无 sqlx / tonic / tokio, 单元测试不需要 mock 任何东西.
2. **Public API 不变**: 所有输入结构体仍在 service.rs, 所有 validate 仍绑定到 method, 调用方无感知.
3. **API Surface 暴露**: pure 函数全部 pub, 同时通过 lib.rs root re-export 让外部调用更便捷.
4. **Node 1:1 对齐**: 与 paperclip/server/src/services/documents.ts 的 normalize/validate 函数语义一致.

## 与 R781 继承关系

R781 (pc-pipeline-conversation-context pure split) 验证了 "delegate to pure" 模式可行性.
R782 将同一模式应用到 pc-documents, 验证该模式可推广. 后续 R783+ 可批量应用到其他 0-测试 crate.

## 累计 (27 跟踪 crate)

| 维度 | 数据 |
|---|---:|
| R782 增量单测 | +24 |
| R756-R782 累计 | **3086** PASS |
| pc-documents 测试 | 0 -> 24 |

## 后续计划

- R783 - pc-work-products (0 测试, 需调研) 加测
- R784 - pc-workspace-commands (0 测试) 加测
- R785 - pc-plugin-database (0 测试) 加测
- R786 - pc-codex-auth-reconciliation (0 测试) 加测
- R787 - pc-run-liveness (0 测试) 加测
- R788 - pc-documents 进一步加 集成测试 (DB 验证)
- Adapter 永远跳过 (硬约束 #2)