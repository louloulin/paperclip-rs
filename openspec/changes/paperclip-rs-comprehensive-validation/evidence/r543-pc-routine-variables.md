# R543 — pc-routine-variables（Node routine-variables.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/routine-variables.ts` (143 LOC) 完整复刻到新 crate
`crates/pc-routine-variables`。workspace crates 84 → **85**。

## 设计原则（高内聚低耦合 + Rust 最佳实践）

### 1. Pure functions only
- 所有公开 API 都是确定性的纯函数，无 IO / 无全局状态 / 无环境依赖
- `getBuiltinRoutineVariableValues()` → `builtin_values_at(DateTime<Utc>)`：显式注入时间，
  测试 100% 确定
- 移除 Node 端的 module-level `HUMAN_TIMESTAMP_FORMATTER`（`Intl.DateTimeFormat`）

### 2. 强类型替代 TS 弱类型
- `RoutineVariableType` enum (`Date | Text`) 替代 TS `string` 字面量
- `RoutineVariable` struct 用 `serde(rename_all = "camelCase")` 保持 Node wire format
- `BTreeMap<String, Value>` 替代 JS `Record<string, unknown>`（保 key 顺序 + JSON 互操作）
- `RoutineTemplateInput<'a>` wrapper 替代 `(string | null | undefined | Array<string|null|undefined>)` 联合类型

### 3. Hand-rolled scanner（无 regex 依赖）
- `find_next_placeholder` 手写 30 行 scanner，零 regex crate 依赖
- 完整支持 Node regex 的所有特性：
  - 容忍 `\_` markdown 转义
  - 容忍 `{` / `}` 周围空白
  - 拒绝 `{` 后跟非字母 / 不闭合等 malformed case
- 与 Node `matchAll` 行为一致：去重 + 首次出现顺序

### 4. Date 校验零 chrono 业务依赖
- `is_valid_routine_date_string` 用纯算法：闰年规则 (`year % 4 == 0 && year % 100 != 0 || year % 400 == 0`)
  + 月份天数表
- `parse_iso_date` 长度 + 字节位置双重校验
- 仅在 `builtin_values_at` 中用 `chrono::DateTime<Utc>` + `Datelike` / `Timelike` 做格式化
- 不引入 `time` crate

### 5. Rust 惯用法
- `From` trait implementations 让调用方零成本传 `&str` / `Option<&str>` / `Vec<&str>` / `Vec<Option<&str>>`
- 错误信号用 `None`（对齐上游 TS 的 `null`）而不是 panic
- `#[serde(rename_all = "lowercase")]` / `#[serde(rename = "defaultValue")]` 保持 wire 兼容
- `BTreeMap` 而非 `HashMap`：`builtin_values_at` 的 key 顺序在测试中断言

## 公开 API

```rust
// Types
pub struct RoutineVariable { name, label, type, defaultValue, required, options }
pub enum RoutineVariableType { Date, Text }
pub struct RoutineVariableOption { label, value }
pub struct RoutineTemplateInput<'a> { fragments: Vec<&'a str> }

pub const BUILTIN_ROUTINE_VARIABLES: &[&str] = &["date", "timestamp"];

// Pure functions
pub fn is_builtin_routine_variable(name: &str) -> bool
pub fn builtin_values_at(now: DateTime<Utc>) -> BTreeMap<String, String>
pub fn is_valid_routine_variable_name(name: &str) -> bool
pub fn is_routine_date_variable_name(name: &str) -> bool
pub fn is_valid_routine_date_string(value: &str) -> bool
pub fn extract_routine_variable_names<I: Into<RoutineTemplateInput<'a>>>(input: I) -> Vec<String>
pub fn sync_routine_variables_with_template<I>(template: I, existing: Option<&[RoutineVariable]>) -> Vec<RoutineVariable>
pub fn stringify_routine_variable_value(value: &Value) -> String
pub fn interpolate_routine_template(template: Option<&str>, values: Option<&BTreeMap<String, Value>>) -> Option<String>

// From conversions for RoutineTemplateInput
impl<'a> From<&'a str> for RoutineTemplateInput<'a>
impl<'a> From<Option<&'a str>> for RoutineTemplateInput<'a>
impl<'a> From<Vec<&'a str>> for RoutineTemplateInput<'a>
impl<'a> From<Vec<Option<&'a str>>> for RoutineTemplateInput<'a>
impl<'a> From<&'a [Option<&'a str>]> for RoutineTemplateInput<'a>
```

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-routine-variables` | **37 passed** (5 internal + 32 integration) |
| `cargo fmt -p pc-routine-variables -- --check` | ✅ 通过 |
| `cargo clippy -p pc-routine-variables --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（32 个集成测试 + 5 internal）

- **extract_routine_variable_names** (7): 单模板 / 多模板 / 去重顺序 / markdown 转义 / null+空过滤 / 无占位符 / 空白容忍 / malformed 拒绝
- **sync_routine_variables_with_template** (4): 保留 metadata / 推断 Date 类型 / 过滤 builtin / 丢弃孤儿
- **is_routine_date_variable_name** (2): 接受矩阵 / 拒绝语法错误
- **is_valid_routine_date_string** (4): 闰年 / 月日范围 / 世纪闰年规则 / 拒绝 garbage
- **is_valid_routine_variable_name** (1): 字符集语法
- **interpolate_routine_template** (7): 简单替换 / 缺失保留 / 无 values / None template / 多种 value type / markdown 解码 / 尾部文本
- **stringify_routine_variable_value** (1): 所有 Value 类型
- **builtin_values_at** (2): ISO date + human timestamp / 12 AM/PM 边界
- **is_builtin_routine_variable** (1): date / timestamp / 大小写 / 空字符串
- **RoutineTemplateInput + serde** (3): From 转换 / camelCase serialize / camelCase deserialize

## 集成待办（不在本轮范围）

- `pc-routines` ingest/validate 路径替换 inline regex 为 `extract_routine_variable_names`
- `pc-routines` runtime template apply 路径用 `interpolate_routine_template` + `builtin_values_at`
- 端到端 smoke：从 UI 保存 routine → Rust API 提取变量 → 渲染 prompt
