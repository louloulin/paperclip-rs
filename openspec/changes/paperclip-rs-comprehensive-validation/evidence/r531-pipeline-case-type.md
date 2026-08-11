# R531 — port Node `packages/shared/src/pipeline-case-type.ts` → Rust `pc-pipelines::case_type`

**日期**: 2026-08-11
**轮次**: R531
**目标**: 把 Node 上游 pipeline case type 派生函数 port 到 Rust, 作为 pc-pipelines 新模块
**模块**: 新模块 `crates/pc-pipelines/src/case_type.rs` (不开新 crate)

---

## 改动

### 上游 Node 实现 (34 LOC)

`packages/shared/src/pipeline-case-type.ts`:
```typescript
interface CaseTypePipelineRef { id: string; key?: string | null; }

export function deriveCaseType(pipeline: CaseTypePipelineRef): string {
  const key = typeof pipeline.key === "string" ? pipeline.key.trim() : "";
  return key || pipeline.id;
}

export function caseTypeMatchesPipeline(
  declaredCaseType: string | null | undefined,
  pipeline: CaseTypePipelineRef,
): boolean {
  if (declaredCaseType == null || declaredCaseType === "") return true;
  return declaredCaseType === deriveCaseType(pipeline);
}
```

无 upstream test 文件, 测试从函数语义 + caller 行为派生。

### Rust port (新模块 `pc-pipelines::case_type`, 163 LOC, 14 测试)

**公开 API**:
```rust
#[derive(Debug, Clone)]
pub struct CaseTypePipelineRef {
    pub id: String,
    pub key: Option<String>,
}
impl CaseTypePipelineRef {
    pub fn new(id: impl Into<String>) -> Self;
    pub fn with_key(mut self, key: impl Into<String>) -> Self;
}

pub fn derive_case_type(pipeline: &CaseTypePipelineRef) -> String;
pub fn case_type_matches_pipeline(
    declared_case_type: Option<&str>,
    pipeline: &CaseTypePipelineRef,
) -> bool;
```

**集成到 pc-pipelines**:
- `crates/pc-pipelines/src/lib.rs:38` 加 `pub mod case_type;`
- 不开新 crate — pc-pipelines 单 lib.rs 已 2609 行, 加模块化结构
- 后续可以继续拆 `pipeline_health.rs` / `pipeline_automation.rs` 等

---

## 测试 (14 个)

**derive_case_type (5 个)**:
| # | 测试 | 验证 |
|---|---|---|
| 1 | `r531_derive_uses_key_when_present` | key="support" → "support" |
| 2 | `r531_derive_falls_back_to_id_when_key_missing` | id="pln-123" + key=None → "pln-123" |
| 3 | `r531_derive_falls_back_to_id_when_key_empty_string` | id="pln-123" + key=Some("") → "pln-123" |
| 4 | `r531_derive_falls_back_to_id_when_key_whitespace` | id="pln-123" + key=Some("   ") → "pln-123" (trim 后空) |
| 5 | `r531_derive_trims_key_whitespace` | key=Some("  support  ") → "support" |

**额外 derive (1 个)**:
| 6 | `r531_derive_preserves_key_with_internal_whitespace` | key=Some("support urgent") → "support urgent" (中间空格保留) |

**case_type_matches_pipeline (8 个)**:
| # | 测试 | 验证 |
|---|---|---|
| 7 | `r531_matches_returns_true_when_declared_is_none` | None → true |
| 8 | `r531_matches_returns_true_when_declared_is_empty` | Some("") → true |
| 9 | `r531_matches_returns_true_when_declared_equals_key` | Some("support") = key="support" → true |
| 10 | `r531_matches_returns_false_when_declared_differs_from_key` | Some("billing") ≠ key="support" → false |
| 11 | `r531_matches_uses_id_fallback_when_key_missing` | pipeline 无 key, declared="pln-123" → true (id fallback) |
| 12 | `r531_matches_uses_id_fallback_when_key_empty` | pipeline 有 key="" 视为 missing, declared="pln-123" → true |
| 13 | `r531_matches_exact_string_comparison` | "Support" ≠ "support" (case-sensitive) |
| 14 | `r531_matches_no_trim_on_declared` | declared=" support" (前导空格) ≠ derive="support" → false (Node 不 trim declared) |

---

## 验证

```bash
$ cargo test -p pc-pipelines --lib case_type
running 14 tests
... (all 14 passed)
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 17 filtered out

$ cargo test --workspace --lib
... (all 74 crates pass)
Total passed: 6580; Total failed: 0

$ cargo check --workspace
... (0 errors)
```

---

## 设计要点

### 1. 不开新 crate — 加入 pc-pipelines 模块
- pc-pipelines 已 2609 行单 lib.rs, 单一模块设计
- R531 拆出 `case_type.rs` 模块是模块化演进的第一步
- 不开 `pc-pipeline-case-type` crate 因为: (a) 只 2 个函数 1 个 struct, (b) 与 pc-pipelines 业务强耦合 (deriveCaseType 在 routes/pipelines.ts 用), (c) 开新 crate 会增加依赖管理负担

### 2. `CaseTypePipelineRef` struct 而非 trait
- Node 上游是 TS interface (duck typing), Rust 强类型
- 选择 struct + `new()` + `with_key()` builder pattern:
  - `CaseTypePipelineRef::new("pln-123")` → 默认无 key
  - `CaseTypePipelineRef::new("pln-123").with_key("support")` → 链式添加 key
- 比 `&dyn Any` 或 trait 简单, 编译期类型安全

### 3. `Option<&str>` 互斥签名
- Node: `declaredCaseType: string | null | undefined` (三态)
- Rust: `Option<&str>` (None)
- 与 R529 `ConnectionInput` enum、R530 `Option<&str>` 设计哲学一致
- 测试 `r531_matches_returns_true_when_declared_is_none` + `_empty` 验证 2 种 "absent" 语义

### 4. 1:1 镜像 Node 行为 (含 quirks)
- **declared 不 trim, key trim**: 测试 `r531_matches_no_trim_on_declared` 验证 upstream 不 trim declared
- **case-sensitive**: 测试 `r531_matches_exact_string_comparison` 验证 "Support" ≠ "support"
- **whitespace-only key = empty**: 测试 `r531_derive_falls_back_to_id_when_key_whitespace` 验证 `trim()` 后空 → fallback id

### 5. 集成层 (留给后续 round)
- `pc-pipelines` service 的 case list/get response 应该用 `derive_case_type(row.pipeline)` 填 `caseType` 字段
- `pc-http/routes/pipelines.rs` 的 caseType 输出应来自 pc-pipelines::case_type
- 当前 `pc-pipelines/lib.rs:2330` (server/routes/pipelines.ts mirror) 还没 wire up

---

## V 真实进度更新

| V | R531 前 | R531 后 | 增量 |
|---|---|---|---|
| V1 | ~80% | ~80% | — |
| V2 | 61% | 61% | — |
| V3 | 100% | 100% | — |
| V4 | ~60% | ~60% | — |
| V5 | ~85% | ~85% | — |
| V6 | ~100% | ~100% | — |
| V8 | 0% | 0% | — |
| V9 | ~40% | ~40% | — |
| V10 | ~30% | ~30% | — |
| V11/V12 | 0% | 0% | — |

R531 是**质量层 + 模块化**轮次: 拆出 case_type 模块让 pc-pipelines 更模块化, 同时补齐 caseType 派生基础设施 (下游 service 层 R532+ 可直接 wire)。

---

## 教训

1. **模块拆分时机**: pc-pipelines 单文件 2609 行已经过大; R531 第一次拆分. 后续 R532+ 应该继续拆 `pipeline_health.rs` / `pipeline_automation.rs` 等
2. **不开新 crate 的判断**: 2 函数 + 1 struct 不值得开 crate, 加到现有 crate 更合适 (R527/R528/R529/R530 都开了新 crate 是因为是全新独立领域)
3. **1:1 镜像 vs Rust idiom**: `match declared { None => true, Some("") => true, Some(t) => ... }` 比 `Option::is_none() || t.is_empty()` 更直观, 也更容易加测试
4. **declared 不 trim 是个 quirk**: Node 上游故意不 trim declared (只 trim key); 测试要写明这个 quirk, 否则将来别人以为可以加 trim 就破坏兼容性

---

## 下一步

### R532 (推荐)
- **pc-pipelines 继续模块化**: 拆出 `pipeline_health.rs` (computePipelineHealth) + `pipeline_automation.rs` (automation retry 等)
- 或者 **port `packages/shared/external-objects.ts`** (52 LOC, formatExternalObjectMentionSourceLabel)

### R533
- **V8 远程 SSH execution**: `restoreRemoteWorkspace` + `materializeRemoteClaudeConfig`
- **V10 plugin 互操作**: spawn 真实 subprocess 跑 plugin

### R534+
- pc-secret-binding 集成层 (R526+R527 接到 pc-http)
- V11/V12/V13 UI + Playwright + 性能
