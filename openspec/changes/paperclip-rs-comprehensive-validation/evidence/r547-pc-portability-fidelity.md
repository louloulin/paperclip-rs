# R547 — pc-portability-fidelity（Node portability-fidelity.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/portability-fidelity.ts` (65 LOC) 完整复刻到新 crate
`crates/pc-portability-fidelity`。workspace crates 88 → **89**。

## 设计原则

### 1. 强类型 enum 替代字符串 severity
- Node 用字符串 `"info" | "warning" | "blocker"`
- Rust 用 `enum PortabilityFidelitySeverity { Info, Warning, Blocker }` + `as_str()` 输出
- 编译期穷尽匹配，序列化稳定（warning 仍然按 `"warning"` 字符串上报）

### 2. struct 替代 Record
- Node 用 `Record<(typeof COUNT_KEYS)[number], number>`
- Rust 用 `struct ExportFidelityCounts` 10 个 `u64` 字段
- 字段命名遵循 Rust snake_case：`label_definitions`, `issue_label_references` ...
- 提供 `zero()` / `default()` 两种构造

### 3. 数组常量保留 wire-order
- `EXPORT_FIDELITY_COUNT_KEYS: [&str; 10]` 仍是数组（不是 `HashSet`）
- 保证序列化/反序列化顺序稳定（与 Node `as const` 一致）

### 4. zero-copy JSON 校验
- `normalize_export_fidelity_counts` 接受 `&serde_json::Value`
- 用 `as_object()` / `as_u64()` 链式访问，缺失或类型不符返回 `None`
- 严格匹配 Node `normalizeExportFidelityCounts` 行为

### 5. unsupported 列表用 const 数组
- `UNSUPPORTED_CATEGORIES: [UnsupportedCategory; 3]`
- 编译期确定，零运行时分配
- 与 Node `UNSUPPORTED_DATA_WARNINGS` tuple 数组语义一致

## 公开 API

```rust
pub const EXPORT_FIDELITY_REPORT_SCHEMA: &str = "paperclip-export-fidelity-v1"
pub const EXPORT_FIDELITY_COUNT_KEYS: [&str; 10]  // 10 keys, 顺序稳定

pub enum PortabilityFidelitySeverity { Info, Warning, Blocker }
impl PortabilityFidelitySeverity { pub fn as_str(self) -> &'static str }

pub struct PortabilityFidelityWarning { code: String, severity: PortabilityFidelitySeverity, message: String }

pub struct ExportFidelityCounts {
    pub label_definitions: u64,
    pub issue_label_references: u64,
    pub issue_blocker_relations: u64,
    pub issue_documents: u64,
    pub issue_work_products: u64,
    pub issue_attachments: u64,
    pub approvals: u64,
    pub cost_events: u64,
    pub activity_log_entries: u64,
    pub issue_monitors: u64,
}
impl ExportFidelityCounts { pub fn zero() -> Self; ... }

pub struct ExportFidelityReport { schema, company_id, counts, warnings, generated_at }

pub fn build_export_fidelity_warnings(counts: &ExportFidelityCounts) -> Vec<PortabilityFidelityWarning>
pub fn normalize_export_fidelity_counts(value: &serde_json::Value) -> Option<ExportFidelityCounts>
```

## 与上游 Node 差异

- **u64 only**：`ExportFidelityCounts` 全部 `u64`，与 Node `number` 等价
- **`serde_json::Value`**：接受标准 JSON 库类型，业务侧可自由切换
- **`Option<u64>`**：替代 Node 异常抛出（`return null`）
- **enum 替代字符串字面量**：编译期安全

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-portability-fidelity` | **20 passed** (4 internal + 16 integration) |
| `cargo fmt -p pc-portability-fidelity` | ✅ 通过 |
| `cargo clippy -p pc-portability-fidelity --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（16 个集成 + 4 internal）

- **schema 常量稳定性** (2): 版本字符串 / 10 个 key 顺序
- **build_warnings** (4): zero / supported-only / unsupported / 单复数
- **normalize** (7): round-trip / null / array / string / missing key / negative / non-integer
- **edge cases** (2): u64::MAX / severity as_str
- **internal** (4): zero / supported-only / unsupported-order / singular-for-one

## 集成待办（不在本轮范围）

- `pc-portability`：用 `ExportFidelityCounts` 收集导出报告
- `pc-backup`：可选扩展 `EXPORT_FIDELITY_COUNT_KEYS` 添加 `backupSnapshots`
- `pc-portability-server`：暴露 `/api/exports/:id/fidelity` HTTP endpoint
- 端到端：跑一次完整 export → 验证 report JSON 字段名一致
