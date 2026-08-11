# R555 — pc-adapter-type（Node adapter-type.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/adapter-type.ts` (15 LOC) 完整复刻到新 crate
`crates/pc-adapter-type`。workspace crates 96 → **97**。

## 设计原则

### 1. 编译期常量替代 zod schema
- Node 用 `z.string().trim().min(1).default("process")`
- Rust 用 `pub const DEFAULT_AGENT_ADAPTER_TYPE: &str = "process"`
- 运行时校验逻辑通过 `normalize_agent_adapter_type` 函数暴露

### 2. `Option<&str>` 入参替代 `z.string().optional()`
- `normalize_agent_adapter_type(None)` → "process"（default）
- `validate_optional_agent_adapter_type(None)` → None

### 3. 预定义 12 个 known built-in adapter types
- `KNOWN_BUILTIN_ADAPTER_TYPES: &[&str]` 覆盖 `constants.ts` 中的所有 `AGENT_ADAPTER_TYPES`
- `is_builtin_adapter_type` 一行判定

### 4. 纯函数零依赖
- 无 serde_json / 无 zod 依赖
- 测试 100% deterministic

## 公开 API

```rust
pub const DEFAULT_AGENT_ADAPTER_TYPE: &str = "process"
pub const KNOWN_BUILTIN_ADAPTER_TYPES: &[&str]  // 12 个内置 adapter

pub fn normalize_agent_adapter_type(raw: Option<&str>) -> String
pub fn validate_agent_adapter_type(value: &str) -> Option<String>
pub fn validate_optional_agent_adapter_type(value: Option<&str>) -> Option<String>
pub fn is_builtin_adapter_type(value: &str) -> bool
```

## 与上游 Node 差异

- **无 zod 依赖**：编译期常量 + 运行时纯函数
- **trim 行为内联**：每次调用都执行（Node 是 schema 一次性约束）
- **DEFAULT 常量**：编译期可用，zod default 是运行期

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-adapter-type` | **21 passed** (8 internal + 13 integration) |
| `cargo fmt -p pc-adapter-type` | ✅ 通过 |
| `cargo clippy -p pc-adapter-type --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（21 个）

- **DEFAULT 常量** (1): "process"
- **normalize** (5): None / empty / whitespace / trim / pass-through
- **validate** (4): 非空接受 / 拒绝空 / 拒绝 whitespace / 拒绝 tab+newline
- **validate_optional** (3): Some / None / empty
- **builtin** (2): 12 个全部识别 / custom 不识别
- **internal** (8): 同上
