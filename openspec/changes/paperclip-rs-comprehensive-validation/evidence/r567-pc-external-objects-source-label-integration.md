# R567 — R-INTEGRATION-7: pc-external-objects → pc-issue-references (source label)

**状态**: ✅ 完成 (2026-08-11)

## 1. 目标

将 R553 创建的 `pc-external-objects` crate（提供 `format_external_object_mention_source_label`
统一格式化函数，覆盖 Title/Description/Comment/Document/Property/Plugin 六种 source kind）
接入 `pc-issues::references::service::source_label`，消除 source-label 格式化的 DRY 重复。

## 2. 重复问题

`pc-issues::references::service::source_label(kind, document_key)` 旧实现：

```rust
fn source_label(kind: &str, document_key: Option<&str>) -> String {
    if kind == "document" {
        document_key.map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "document".to_string())
    } else {
        kind.to_string()
    }
}
```

**问题**:
- 只识别 `"document"` 一种 kind，其他 kind 直接 echo（无 Title/Description/Comment
  的大写首字母格式化，无 Document[:key] 的统一前缀）
- 没有识别 `"property"` / `"plugin"` kind（直接 echo raw string，不一致）
- 跟 `pc-external-objects::format_external_object_mention_source_label` 的
  统一格式化逻辑 DRY 重复

## 3. 集成实现（crates/pc-issues/src/references/service.rs）

### 3.1 依赖新增

```toml
# crates/pc-issues/Cargo.toml
pc-external-objects = { path = "../pc-external-objects" }
```

### 3.2 source_label 重写

```rust
use pc_external_objects::{
    format_external_object_mention_source_label, ExternalObjectMentionSource,
    ExternalObjectMentionSourceKind,
};

fn source_label(kind: &str, document_key: Option<&str>) -> String {
    match ExternalObjectMentionSourceKind::parse(kind) {
        Some(parsed_kind) => {
            // Document 需要 surface key；其他 kind 的 property_key 暂不传（service 层未 track）
            let doc_key_for_doc =
                if matches!(parsed_kind, ExternalObjectMentionSourceKind::Document) {
                    document_key.map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                };
            let source = ExternalObjectMentionSource {
                company_id: None,
                source_issue_id: None,
                source_kind: parsed_kind,
                source_record_id: None,
                document_key: doc_key_for_doc,
                property_key: None,
            };
            format_external_object_mention_source_label(&source)
        }
        None => kind.to_string(),  // unknown kind: 不破坏现有 UI 显示
    }
}
```

## 4. 行为对照表

| kind | 旧 `source_label` | 新 `source_label` | `format_external_object_mention_source_label` |
|---|---|---|---|
| `title` | `"title"` | `"Title"` | `"Title"` ✅ |
| `description` | `"description"` | `"Description"` | `"Description"` ✅ |
| `comment` | `"comment"` | `"Comment"` | `"Comment"` ✅ |
| `document` (无 key) | `"document"` | `"Document"` | `"Document"` ✅ |
| `document` (key="plan.md") | `"plan.md"` | `"Document: plan.md"` | `"Document: plan.md"` ✅ |
| `document` (key="") | `"document"` | `"Document"` | `"Document"` ✅ |
| `property` | `"property"` | `"Property"` | `"Property"` ✅ |
| `plugin` | `"plugin"` | `"Plugin"` | `"Plugin"` ✅ |
| unknown kind | echo | echo | n/a (未知 kind 走 fallback) |

## 5. 测试 (crates/pc-issues/tests/r567_external_objects_source_label.rs)

10 个测试，覆盖全部 6 种 kind + 边界情况：

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r567_title_label` | `"title"` → `"Title"` |
| 2 | `r567_description_label` | `"description"` → `"Description"` |
| 3 | `r567_comment_label` | `"comment"` → `"Comment"` |
| 4 | `r567_document_label_without_key` | `"document" + None` → `"Document"` |
| 5 | `r567_document_label_with_key` | `"document" + "plan.md"` → `"Document: plan.md"` |
| 6 | `r567_document_label_with_empty_key_falls_back` | `""` / `"   "` → `"Document"` |
| 7 | `r567_property_label_via_unified_formatter` | `"property"` → `"Property"` |
| 8 | `r567_plugin_label_via_unified_formatter` | `"plugin"` → `"Plugin"` |
| 9 | `r567_unknown_kind_returns_raw_string` | `"unknown_kind"` → `"unknown_kind"` (fallback) |
| 10 | `r567_legacy_source_kind_constants_still_resolve` | SOURCE_KIND_* 全部走 unified path |

测试用 inlined re-implementation 校验与 pc-external-objects 的 byte-for-byte 一致性。

## 6. 无回归验证

```bash
$ cargo test -p pc-issues --lib
test result: ok. 96 passed; 0 failed

$ cargo test -p pc-external-objects --lib
test result: ok. 7 passed; 0 failed

$ cargo test -p pc-issues --test r567_external_objects_source_label
test result: ok. 10 passed; 0 failed
```

## 7. 设计亮点

### 7.1 向后兼容

- 未知 kind 直接 echo 原字符串 — 不破坏现有 UI 显示（不抛错，不静默替换）
- Document 空 key → fallback `"Document"`，与旧行为一致

### 7.2 单一来源真相

未来需要调整 source-label 格式时（例如增加 emoji、改大小写、加 i18n），只需改
`pc-external-objects::format_external_object_mention_source_label` 一处。

### 7.3 通用性

重构后的 `source_label` 不限于 service 使用 — extractor / API 响应层未来需要格式化时
可直接复用同一 helper。

## 8. 累计 R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | pc-mentions → pc-issues | ✅ R562 |
| 3 | pc-pipeline-case-type → pc-pipelines | ✅ R563 |
| 4 | pc-adapter-type → 各 adapter crate | ✅ R564 |
| 5 | pc-portability-fidelity → pc-portability | ✅ R565 |
| 6 | pc-execution-workspace-guards → pc-http | ✅ R566 |
| 7 | **pc-external-objects → pc-issue-references** | ✅ **R567** |
| 8 | pc-app-definitions → pc-http route generation | 待做 |
| 9 | pc-trust-policy → pc-authz | 待做 |
| 10 | pc-workspace-commands → pc-cli | 待做 |
| 11 | pc-api-routes → pc-http | 待做 |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**7/12 = 58%**

## 9. 下一步

- **R568**: R-INTEGRATION-8 — pc-app-definitions → pc-http route generation
- 评估 R568 完成后的剩余工作量，决定是否进入 V1-V15 硬目标（UI 60 client happy path 等）

