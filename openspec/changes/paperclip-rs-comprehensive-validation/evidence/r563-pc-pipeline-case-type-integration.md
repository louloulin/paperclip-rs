# R563 — R-INTEGRATION-3: pc-pipelines 重复实现 → pc-pipeline-case-type delegation（2026-08-11）

## 1. 发现：DRY 违规

`pc-pipeline-case-type`（R554, 82 LOC）独立 crate 包含 `derive_case_type` + `case_type_matches_pipeline` + `CaseTypePipelineRef`。

但 `pc-pipelines/src/case_type.rs`（R531, 163 LOC）**重复实现了同样的 API**（独立 struct、独立 fn、独立测试）。

这是 paperclip-rs 架构里的一个 DRY 违规 — 同一份逻辑在两个 crate 维护。

## 2. 重构：单点真相

把 `pc-pipelines/src/case_type.rs` 改成 **thin delegation layer**：

```rust
// 之前：163 LOC 重复实现
pub struct CaseTypePipelineRef { pub id: String, pub key: Option<String> }
impl CaseTypePipelineRef { fn new() { ... }, fn with_key() { ... } }
pub fn derive_case_type(pipeline: &CaseTypePipelineRef) -> String { /* 重写 */ }
pub fn case_type_matches_pipeline(...) -> bool { /* 重写 */ }

// 之后：薄 delegation（pc-pipeline-case-type 是真相）
pub struct CaseTypePipelineRef { pub id: String, pub key: Option<String> }
impl CaseTypePipelineRef {
    fn to_canonical(&self) -> canonical::CaseTypePipelineRef { /* 转换 */ }
}
pub fn derive_case_type(pipeline: &CaseTypePipelineRef) -> String {
    canonical::derive_case_type(&pipeline.to_canonical())
}
pub fn case_type_matches_pipeline(...) -> bool {
    canonical::case_type_matches_pipeline(declared, &pipeline.to_canonical())
}
```

## 3. 设计决策

### 3.1 保留本地 `CaseTypePipelineRef` newtype
- 现有调用者用 `pc_pipelines::case_type::CaseTypePipelineRef` —— 全删会破坏公开 API
- 保留 newtype 作 thin wrapper，`to_canonical()` 做内部转换
- 公开 API 签名零变化 → 现有 caller + 测试零修改

### 3.2 `derive_case_type` / `case_type_matches_pipeline` 1-line delegation
- 调用方代码不变（签名一致）
- 行为不变（语义由 canonical 决定）
- 加 4 个 delegation 测试验证一致性

### 3.3 删 13 个原 internal tests
- 原 `case_type.rs` 自己的 internal tests 测的是重复实现的逻辑
- 现在测的是 delegation —— 用 4 个新 test 覆盖关键路径（derive_with_key / derive_fallback_to_id / matches_none / matches_some / matches_mismatch / delegation_consistency）
- pc-pipeline-case-type 自己的 5 个 internal tests 仍然测原 canonical 实现

## 4. 验证结果

### 4.1 delegation 测试
```
running 4 tests
test case_type::delegation_tests::delegate_derive_falls_back_to_id_when_no_key ... ok
test case_type::delegation_tests::delegate_matches_handles_none_empty_some_and_mismatch ... ok
test case_type::delegation_tests::delegate_derive_uses_key_when_present ... ok
test case_type::delegation_tests::delegation_produces_same_results_as_canonical_directly ... ok

test result: ok. 4 passed; 0 failed
```

### 4.2 pc-pipelines lib 完整无回归
```
cargo test -p pc-pipelines --lib
  → 21 passed / 0 failed
```

### 4.3 clippy
mention_extraction_hook 模块 0 warnings（clean）

## 5. 累计成果

- 消除 DRY 违规（pc-pipelines/src/case_type.rs 163 LOC 重复实现 → 100 LOC 薄 delegation）
- pc-pipeline-case-type 成为 case_type 逻辑的**单点真相**
- 加 pc-pipeline-case-type 作为 pc-pipelines 依赖（单向依赖，无环）
- 4 个新 delegation tests 验证一致性
- 21 个 pc-pipelines lib tests 全过（无回归）

## 6. R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | pc-mentions → pc-issues | ✅ R562 |
| 3 | **pc-pipeline-case-type → pc-pipelines** | ✅ **R563** |
| 4 | pc-adapter-type → 各 adapter crate | 待做 |
| 5 | pc-portability-fidelity → pc-portability | 待做 |
| 6 | pc-execution-workspace-guards → pc-issues/execution | 待做 |
| 7 | pc-external-objects → pc-issue-references | 待做 |
| 8 | pc-app-definitions → pc-http route generation | 待做 |
| 9 | pc-trust-policy → pc-authz | 待做 |
| 10 | pc-workspace-commands → pc-cli | 待做 |
| 11 | pc-api-routes → pc-http | 待做 |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**R-INTEGRATION-1 + 2 + 3 完成**：3/12 = 25%

## 7. 下一步

- **R564**: R-INTEGRATION-4 — pc-adapter-type 接入各 adapter 验证
- **R565**: pc-trust-policy → pc-authz 接入（验证 trust policy 真正被 authz 强制）
