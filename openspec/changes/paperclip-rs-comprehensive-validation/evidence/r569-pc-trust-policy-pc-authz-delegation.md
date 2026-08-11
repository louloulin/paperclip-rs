# R569 — R-INTEGRATION-9: pc-trust-policy → pc-authz delegation

**状态**: ✅ 完成 (2026-08-12)

## 1. 目标

消除 `pc-authz::trust` 与 `pc-trust-policy` 之间的类型/常量 DRY 重复，建立
`TrustPreset` 枚举与 `LOW_TRUST_*` 常量的单一来源真相。

## 2. 重复问题（before）

`pc-authz/src/trust.rs` 独立定义了：

```rust
// 1. TrustPreset 枚举（与 pc-trust-policy 完全相同）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrustPreset {
    Standard,
    LowTrustReview,
}
impl TrustPreset {
    pub fn as_str(self) -> &'static str { /* ... */ }
    pub fn from_str_opt(s: &str) -> Option<Self> { /* ... */ }
}

// 2. 三个常量（与 pc-trust-policy 完全相同）
pub const LOW_TRUST_REVIEW_PRESET: &str = "low_trust_review";
pub const LOW_TRUST_REVIEW_PRESET_VERSION: u32 = 1;
pub const LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION: &str = "quarantine";
```

**问题**: 两份独立定义 → 不同内存布局（不同 type identity）→ 跨 crate 传递需要转换；
任一处修改会引入静默不一致 bug。

## 3. 集成实现

### 3.1 pc-trust-policy 微调

新增 `serde::{Serialize, Deserialize}` + `#[serde(rename_all = "snake_case")]` 让
`TrustPreset` 可以 JSON 序列化（pc-authz 需要）：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustPreset {
    Standard,
    LowTrustReview,
}
```

新增 `serde` workspace 依赖到 `pc-trust-policy/Cargo.toml`。

### 3.2 pc-authz 委派

`pc-authz/src/trust.rs` 改为 re-export + 委派：

```rust
// R569: R-INTEGRATION-9 — delegate `TrustPreset` enum + LOW_TRUST_*
// constants to `pc-trust-policy` so the canonical types live in one
// place.

/// Re-export `TrustPreset` from `pc-trust-policy` (single source of truth).
pub use pc_trust_policy::TrustPreset;

pub use pc_trust_policy::{
    LOW_TRUST_REVIEW_PRESET, LOW_TRUST_REVIEW_PRESET_VERSION,
    LOW_TRUST_REVIEW_RAW_OUTPUT_DISPOSITION,
};

/// pc-authz-specific depth limit not present in pc-trust-policy.
pub const LOW_TRUST_ISSUE_ANCESTRY_MAX_DEPTH: u32 = 12;

/// Backwards-compatible alias for `from_str_opt` callers.
pub fn trust_preset_from_str_opt(s: &str) -> Option<TrustPreset> {
    TrustPreset::parse(s)
}
```

`TrustPreset::from_str_opt` 调用处（line 146）替换为 `TrustPreset::parse`。

### 3.3 pc-authz/src/lib.rs 顶层 re-export

`pc_authz::trust::TrustPreset` 已通过 `pub use trust::*` 在 crate 根 re-export。
由于 `pub use pc_trust_policy::TrustPreset`，crate 根的 `TrustPreset` 也自动从
`pc-trust-policy` 来 — 无需额外修改。

## 4. 类型单一来源验证

```rust
// 通过 std::any::type_name 验证两个路径指向同一类型
assert_eq!(
    std::any::type_name::<pc_authz::trust::TrustPreset>(),
    std::any::type_name::<pc_trust_policy::TrustPreset>()
);
// 两者都展开为 `"pc_trust_policy::TrustPreset"` ✅
```

## 5. 测试 (crates/pc-authz/tests/r569_trust_policy_delegation.rs)

8 个委派测试：

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r569_trust_preset_alias_matches_canonical` | `type_name` 一致（同一类型） |
| 2 | `r569_trust_preset_variants_round_trip` | Standard/LowTrustReview 双向转换 |
| 3 | `r569_trust_preset_serializes_snake_case` | JSON 输出 `"standard"` / `"low_trust_review"` |
| 4 | `r569_trust_preset_deserializes_snake_case` | 反向解析 |
| 5 | `r569_low_trust_constants_match` | 3 个 LOW_TRUST_* 常量字节级一致 |
| 6 | `r569_trust_preset_from_str_opt_compat_alias` | 旧 API 名称仍工作 |
| 7 | `r569_low_trust_issue_ancestry_max_depth_preserved` | authz 特有深度限制保留 |
| 8 | `r569_trust_presets_constant_in_sync` | TRUST_PRESETS 数组全部可解析 |

## 6. 无回归验证

```bash
$ cargo test -p pc-authz --lib
test result: ok. 73 passed; 0 failed

$ cargo test -p pc-authz --test r569_trust_policy_delegation
test result: ok. 8 passed; 0 failed

$ cargo test -p pc-trust-policy --lib
test result: ok. 5 passed; 0 failed
```

## 7. 设计亮点

### 7.1 真正的单点真相

- `TrustPreset` 类型仅在 `pc-trust-policy` 定义一次
- `pc-authz::trust::TrustPreset` 与 `pc-trust_policy::TrustPreset` 是**同一类型**（不是 alias / 不是 wrapper）
- 跨 crate 传递零摩擦（无 `From` / `Into` 转换）

### 7.2 向后兼容

- `pc_authz::trust::TrustPreset::from_str_opt` → 替换为 `TrustPreset::parse`（一个 callsite）
- `pc_authz::trust::trust_preset_from_str_opt` → 暴露为独立函数供新代码使用
- 旧的 `LOW_TRUST_*` 常量路径保留（re-export）

### 7.3 责任划分清晰

`pc-trust-policy` = 共享 policy types + 常量（无 IO、无 resolver logic）
`pc-authz::trust` = resolver logic + boundary 解析（`LowTrustBoundary`、`DenyReason`、
`TrustPresetResolution`、`TrustPresetSource`、`resolve_core_trust_preset`）

## 8. 累计 R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | pc-mentions → pc-issues | ✅ R562 |
| 3 | pc-pipeline-case-type → pc-pipelines | ✅ R563 |
| 4 | pc-adapter-type → 各 adapter crate | ✅ R564 |
| 5 | pc-portability-fidelity → pc-portability | ✅ R565 |
| 6 | pc-execution-workspace-guards → pc-http | ✅ R566 |
| 7 | pc-external-objects → pc-issue-references | ✅ R567 |
| 8 | pc-app-definitions → pc-http route | ✅ R568 |
| 9 | **pc-trust-policy → pc-authz** | ✅ **R569** |
| 10 | pc-workspace-commands → pc-cli | 待做 |
| 11 | pc-api-routes → pc-http | 待做 |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**9/12 = 75%**

## 9. 下一步

- **R570**: R-INTEGRATION-10 — pc-workspace-commands → pc-cli

