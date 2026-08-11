# R551 — pc-trust-policy（Node trust-policy.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/trust-policy.ts` (67 LOC) 完整复刻到新 crate
`crates/pc-trust-policy`。workspace crates 92 → **93**。

## 设计原则

### 1. 强类型 enum 替代字符串字面量
- Node 用 `as const` 字面量联合
- Rust 用 `enum TrustPreset / SourceTrustArtifactKind / SourceTrustDisposition / PromotedByActorType`
- 每个 enum 提供 `as_str()` + `parse()` round-trip

### 2. struct 替代 interface
- `LowTrustBoundary` 用 `Default` + 字段全部 `Option` / `Vec`
- `TrustAuthorizationPolicy` 用 `serde_json::Map` 兜底额外字段（forward-compat）
- `LowTrustOutputPromotionTarget` 用 `r#type` raw identifier 避开 Rust `type` 关键字

### 3. Helper 函数消除魔法
- `is_low_trust_tool_class` 一行判定
- `low_trust_tool_classes_set` 返回零拷贝 `HashSet<&'static str>`
- `low_trust_review_policy()` 工厂返回 canonical preset
- `is_low_trust_review(&policy)` 综合判定 preset + review preset

### 4. const 数组保留 wire-order
- `TRUST_PRESETS: [&str; 2]` / `LOW_TRUST_TOOL_CLASSES: [&str; 3]`
- 与 Node `as const` 完全等价；JSON 序列化顺序稳定

## 公开 API

```rust
// ----- constants -----
pub const TRUST_PRESETS: [&str; 2]
pub const DEFAULT_TRUST_PRESET: &str
pub const LOW_TRUST_REVIEW_PRESET: &str
pub const LOW_TRUST_REVIEW_PRESET_VERSION: u32
pub const LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION: &str
pub const LOW_TRUST_TOOL_CLASSES: [&str; 3]

// ----- enums -----
pub enum TrustPreset { Standard, LowTrustReview }
pub enum SourceTrustArtifactKind { Issue, Comment, Document, WorkProduct }
pub enum SourceTrustDisposition { Quarantined, Promoted }
pub enum PromotedByActorType { Agent, User, System }
pub enum LowTrustPromotionTargetType { Issue }

// ----- structs -----
pub struct LowTrustOutputPromotionTarget { r#type: LowTrustPromotionTargetType, issue_id: String }
pub struct LowTrustBoundary { mode, company_id, project_ids, ..., output_promotion_target }
pub struct LowTrustReviewPresetPolicy { id: String, version: u32, raw_output_disposition: String }
pub struct TrustAuthorizationPolicy { trust_preset, review_preset, trust_boundary, extra: serde_json::Map }
pub struct SourceTrustPromotionSource { artifact_kind, artifact_id, issue_id }
pub struct SourceTrustMetadata { preset, disposition, source_issue_id, source_run_id, ..., promoted_at }

// ----- helpers -----
pub fn is_low_trust_tool_class(class: &str) -> bool
pub fn low_trust_tool_classes_set() -> HashSet<&'static str>
pub fn low_trust_review_policy() -> LowTrustReviewPresetPolicy
pub fn is_low_trust_review(policy: &TrustAuthorizationPolicy) -> bool
```

## 与上游 Node 差异

- **snake_case 字段名**：Node camelCase，Rust snake_case
- **enum + as_str/parse**：替代字符串字面量 union
- **Default trait**：每个 struct 可 `..Default::default()` 构造
- **serde_json::Map 兜底**：`TrustAuthorizationPolicy.extra` 保留上游 `Record<string, unknown>` 行为

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-trust-policy` | **18 passed** (5 internal + 13 integration) |
| `cargo fmt -p pc-trust-policy` | ✅ 通过 |
| `cargo clippy -p pc-trust-policy --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（18 个）

- **constants** (1): 5 个常量值稳定
- **enum round-trip** (4): TrustPreset / ArtifactKind / Disposition / PromotedByActorType
- **helpers** (4): tool class set / is_low_trust_tool_class / canonical policy
- **is_low_trust_review** (4): preset 设置 / review preset 设置 / standard / 空
- **struct** (1): SourceTrustPromotionSource 构造
- **internal** (5): preset/artifact round-trip / canonical values / tool class / is_low_trust_review

## 集成待办（不在本轮范围）

- `pc-issues`：用 `SourceTrustMetadata` 记录 issue 来源 trust
- `pc-pipelines`：用 `TrustAuthorizationPolicy` 控制 pipeline execution
- `pc-routines`：low_trust_review preset 用于 routine 输出 quarantine
- `pc-portability`：`LOW_TRUST_TOOL_CLASSES` 导出/导入工具权限清单
- 端到端：UI trust badge 显示 preset + disposition
