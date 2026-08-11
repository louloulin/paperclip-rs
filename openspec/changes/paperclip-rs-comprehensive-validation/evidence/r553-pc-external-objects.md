# R553 — pc-external-objects（Node external-objects.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/external-objects.ts` (52 LOC) 完整复刻到新 crate
`crates/pc-external-objects`。workspace crates 94 → **95**。

## 设计原则

### 1. enum `ExternalObjectMentionSourceKind` 强类型
- Node 用 `as const` 字面量数组
- Rust 用 `enum` + 6 变体 + `as_str()` / `parse()` round-trip + `all()` 列表
- UI 下拉菜单可直接 `for kind in ExternalObjectMentionSourceKind::all()`

### 2. struct 强类型所有 URL 类型
- `ExternalObjectUrlMatch` / `ExternalObjectCanonicalIdentity` / `ExternalObjectCanonicalUrl` /
  `ExternalObjectMentionSource` 都建模为 struct
- `Option<String>` 替代 nullable 字段
- `enum CanonicalScheme { Http, Https }` 替代字符串字面量

### 3. `format_external_object_mention_source_label` 完全镜像 Node switch
- Document / Property 类型带可选 key，key 为空时 fallback 到 "Document" / "Property"
- 其他类型 (title/description/comment/plugin) 直接返回固定字符串

### 4. 与 `pc-external-objects-server` 解耦
- `pc-external-objects-server` 提供服务端 API（HTTP routes）
- `pc-external-objects` 提供共享类型 + 客户端 label 格式化
- 单一职责，互不依赖

## 公开 API

```rust
pub enum ExternalObjectMentionSourceKind { Title, Description, Comment, Document, Property, Plugin }
impl ExternalObjectMentionSourceKind { pub fn as_str / pub fn parse / pub fn all }

pub enum CanonicalScheme { Http, Https }
impl CanonicalScheme { pub fn as_str }

pub struct ExternalObjectUrlMatch { index, length, matched_text }
pub struct ExternalObjectCanonicalIdentity { scheme, host, path, query_param_hashes }
pub struct ExternalObjectUrlCanonicalizationOptions { identity_query_params }
pub struct ExternalObjectCanonicalUrl { sanitized_canonical_url, sanitized_display_url, canonical_identity, canonical_identity_hash, redacted_matched_text }
pub struct ExternalObjectMentionSource { company_id, source_issue_id, source_kind, source_record_id, document_key, property_key }

pub fn format_external_object_mention_source_label(source: &ExternalObjectMentionSource) -> String
```

## 与上游 Node 差异

- **enum + all()**：替代字面量数组
- **Option<String>**：替代 `string | null`
- **独立 crate**：服务端 / 客户端 label 逻辑分离

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-external-objects` | **23 passed** (7 internal + 16 integration) |
| `cargo fmt -p pc-external-objects` | ✅ 通过 |
| `cargo clippy -p pc-external-objects --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（23 个）

- **enum round-trip** (1): 6 个 kind
- **all() 列表** (1): 完整 6 个顺序
- **scheme** (1): http / https 字符串
- **format label** (8): title / description / comment / document(3) / property(2) / plugin
- **struct** (3): UrlMatch / CanonicalIdentity / UrlCanonicalizationOptions
- **full source** (1): 全部字段填写 + format 输出
