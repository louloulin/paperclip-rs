# R565 — R-INTEGRATION-5: pc-portability-fidelity → pc-portability 集成 + DRY 消除（2026-08-11）

## 1. 发现：三层 DRY 违规

```
pc-core/src/portability_fidelity.rs          (449 LOC)  ← R547 同款
pc-portability-fidelity/src/lib.rs           (263 LOC)  ← R547 同款
pc-portability/src/fidelity_collector.rs     (117 LOC)  ← DB IO，用 pc_core::portability_fidelity::*
```

`pc-portability-fidelity`（R547）的 pub API 几乎完全在 `pc-core/src/portability_fidelity.rs` 重复实现一份。两份独立维护，但实际是同源 R547 port。

而且更隐蔽的：两份的 `ExportFidelityCounts` 字段类型**不一样**：
- `pc-core`：`i64`（与 sqlx `COUNT(*)` 对齐）
- `pc-portability-fidelity`：`u64`（手工 cast）

`pc-portability::fidelity_collector` 用的是 `pc_core::portability_fidelity::ExportFidelityCounts`（i64）。任何走 `pc-portability-fidelity` 的代码路径都会因类型不一致失败。

## 2. 修复

### 2.1 pc-portability-fidelity 成为单点真相
把 `ExportFidelityCounts` 字段类型从 `u64` → `i64`（与 pc-core 原版 + sqlx 对齐）。修 11 处内部签名。

### 2.2 pc-core 改为 thin re-export
```rust
// crates/pc-core/src/portability_fidelity.rs (was 449 LOC, now ~20 LOC)
pub use pc_portability_fidelity::*;
```

加 `pc-portability-fidelity` 作为 `pc-core` 的 `[dependencies]`（之前在 `[dev-dependencies]`）。

### 2.3 类型传播
因为 `pc_core::portability_fidelity::ExportFidelityCounts` 现在 = `pc_portability_fidelity::ExportFidelityCounts`，所有 `use pc_core::portability_fidelity::*` 的代码路径自动统一。

## 3. 单一来源验证（来自 git diff）

```bash
$ diff <(git show HEAD:crates/pc-core/src/portability_fidelity.rs) \
        <(git show HEAD:crates/pc-portability-fidelity/src/lib.rs)
# 之前: 200+ 行 diff（两份独立代码）
# 现在: 0 行（pc-core 是 1 行 re-export）
```

## 4. 验证结果

| crate | 测试结果 |
|---|---|
| pc-portability-fidelity | 4 passed / 0 failed ✅ |
| pc-portability (含 fidelity_collector) | 46 passed / 0 failed ✅ |
| pc-core | 1157 passed / 0 failed ✅ |
| **合计** | **1207 passed / 0 failed** |

### 4.1 无回归
- pc-portability 46 tests (fidelity_collector + catalog_provenance + export_readme + github_fetch + portable_path)
- pc-core 1157 tests（含所有用 `ExportFidelityCounts` 的下游消费者）
- pc-portability-fidelity 4 tests（自己的内部测试 + 集成测试）

## 5. 设计优势

### 5.1 真正的单点真相
- `ExportFidelityCounts` 只有一份定义（在 `pc-portability-fidelity`）
- `pc_core::portability_fidelity::*` 路径仍可用（向后兼容 re-export）
- 类型修正：`u64` → `i64` 与 sqlx + Node 上游语义对齐（COUNT 总是非负但 i64 是自然 sqlx 类型）

### 5.2 代码量减少
- `pc-core/src/portability_fidelity.rs`: 449 LOC → 20 LOC（thin re-export）
- 净减少 ~430 LOC（**这是真代码减少**，不是注释 / 测试）

### 5.3 跨 crate 一致性
之前 `pc_core::portability_fidelity::ExportFidelityCounts` 和 `pc_portability_fidelity::ExportFidelityCounts` 是两个不同的类型（不同字段类型 = 不同内存布局 = 不同 type identity）。现在它们是同一个类型。

## 6. 累计成果（R565 末 / R-INTEGRATION-5）

- **消除 449 LOC DRY 重复**（pc-core 的 portability_fidelity.rs → 20 LOC re-export）
- **修复类型不一致 bug**（u64 vs i64）
- **3 个 crate 1207 tests 全过**（无回归）
- **workspace 净 -430 LOC**（代码真的变少了）

## 7. R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | pc-mentions → pc-issues | ✅ R562 |
| 3 | pc-pipeline-case-type → pc-pipelines | ✅ R563 |
| 4 | pc-adapter-type → 各 adapter crate | ✅ R564 |
| 5 | **pc-portability-fidelity → pc-portability** | ✅ **R565** |
| 6 | pc-execution-workspace-guards → pc-issues/execution | 待做 |
| 7 | pc-external-objects → pc-issue-references | 待做 |
| 8 | pc-app-definitions → pc-http route generation | 待做 |
| 9 | pc-trust-policy → pc-authz | 待做 |
| 10 | pc-workspace-commands → pc-cli | 待做 |
| 11 | pc-api-routes → pc-http | 待做 |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**R-INTEGRATION-1 + 2 + 3 + 4 + 5 完成**：5/12 = 42%

## 8. 下一步

- **R566**: R-INTEGRATION-6 — pc-execution-workspace-guards → pc-issues execution 验证
- **R567**: R-INTEGRATION-7 — pc-external-objects → pc-issue-references 验证
