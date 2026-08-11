# R554 — pc-pipeline-case-type（Node pipeline-case-type.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/pipeline-case-type.ts` (33 LOC) 完整复刻到新 crate
`crates/pc-pipeline-case-type`。workspace crates 95 → **96**。

## 设计原则

### 1. struct 替代 interface
- `CaseTypePipelineRef { id, key }` 强类型
- `key: Option<String>` 替代 `string | null`

### 2. `derive_case_type` 镜像 Node 逻辑
- 优先返回 `key.trim()`，key 为空时 fallback 到 `id`
- 包含 trim 行为（whitespace-only key 也 fallback）

### 3. `case_type_matches_pipeline` 用 `Option<&str>`
- 替代 Node `string | null | undefined`
- `match` 模式：`None | Some("") => true, Some(s) => s == derived`

## 公开 API

```rust
pub struct CaseTypePipelineRef { id: String, key: Option<String> }
pub fn derive_case_type(pipeline: &CaseTypePipelineRef) -> String
pub fn case_type_matches_pipeline(declared_case_type: Option<&str>, pipeline: &CaseTypePipelineRef) -> bool
```

## 与上游 Node 差异

- **Option<&str>**：替代 `string | null | undefined`
- **match arm**：`None | Some("")` 单分支替代两个独立 case

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-pipeline-case-type` | **15 passed** (5 internal + 10 integration) |
| `cargo fmt -p pc-pipeline-case-type` | ✅ 通过 |
| `cargo clippy -p pc-pipeline-case-type --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（15 个）

- **derive** (5): prefer key / fall back None / fall back empty / trim whitespace / whitespace-only fallback
- **matches** (5): None / empty / agreeing / disagreeing / fallback id 模式
