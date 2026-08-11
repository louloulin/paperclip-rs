# R558 — pc-responsible-user-denial-copy（Node responsible-user-denial.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/responsible-user-denial.ts` (76 LOC) 完整复刻到新 crate
`crates/pc-responsible-user-denial-copy`。workspace crates 99 → **100**。

## 设计原则

### 1. 与 `pc-responsible-user-denial` 严格分离
- **新 crate** `pc-responsible-user-denial-copy`：copy contract + 描述文案
- **已有 crate** `pc-responsible-user-denial`：server-side run-outcome code 规范化（不同 code 集）
- 两个 crate 互不依赖；这是 *copy* contract，不是 server logic

### 2. enum + as_str/parse 替代字符串字面量联合
- `ResponsibleUserDenialCode` (Unauthorized / Unavailable) + `as_str()` 返回 `"RESPONSIBLE_USER_UNAUTHORIZED"` / `"RESPONSIBLE_USER_UNAVAILABLE"`
- `ResponsibleUserDenialTone` 区分 display tone（"unauthorized" / "unavailable"）

### 3. `ResponsibleUserDenialCopy` struct 替代 TS interface
- `code`, `tone`, `title`, `description`, `recommended_action` 五个字段
- 所有文案预生成，无需运行期格式化（除 name 插值）

### 4. `responsible_user_label` 强类型 fallback
- `Option<&str>` 替代 `string | null | undefined`
- empty / whitespace / null 三态都 fallback 到 "the responsible user"
- 永不显示 raw id（与 Node 一致）

### 5. 文案内插 name 时保证 fallback
- `describe_responsible_user_denial(code, Some(Options { user_name: Some("Alice") }))` → 包含 "Alice"
- `describe_responsible_user_denial(code, None)` → 包含 "the responsible user"
- `describe_responsible_user_denial(code, Some(Options { user_name: Some("   ") }))` → 包含 "the responsible user"

## 公开 API

```rust
pub const RESPONSIBLE_USER_DENIAL_CODES: [&str; 2]

pub enum ResponsibleUserDenialCode { Unauthorized, Unavailable }
impl ResponsibleUserDenialCode { pub fn as_str / pub fn parse }

pub enum ResponsibleUserDenialTone { Unauthorized, Unavailable }
impl ResponsibleUserDenialTone { pub fn as_str }

pub struct ResponsibleUserDenialCopy {
    pub code: ResponsibleUserDenialCode,
    pub tone: ResponsibleUserDenialTone,
    pub title: String,
    pub description: String,
    pub recommended_action: String,
}

pub struct ResponsibleUserDenialOptions<'a> { pub user_name: Option<&'a str> }

pub fn is_responsible_user_denial_code(code: &str) -> bool
pub fn responsible_user_label(user_name: Option<&str>) -> String
pub fn describe_responsible_user_denial(
    code: ResponsibleUserDenialCode,
    options: Option<ResponsibleUserDenialOptions<'_>>,
) -> ResponsibleUserDenialCopy
```

## 与上游 Node 差异

- **独立 crate**：与 `pc-responsible-user-denial` 解耦
- **enum + as_str/parse**：替代字符串字面量联合
- **Options struct**：替代 `options: { userName? }` named-arg pattern
- **String 替代模板字面量**：`format!("...{who}...")` 替代 `` `...${who}...` ``

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-responsible-user-denial-copy` | **18 passed** (5 internal + 13 integration) |
| `cargo fmt -p pc-responsible-user-denial-copy` | ✅ 通过 |
| `cargo clippy -p pc-responsible-user-denial-copy --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（18 个）

- **constants** (1): 2 个 code 字符串
- **enum round-trip** (1): 2 个 code as_str/parse
- **type guard** (1): `is_responsible_user_denial_code` 4 情况
- **label** (2): fallback (3 情况) / known name (2 情况)
- **describe** (6): unauthorized (3) / unavailable (3)
- **internal** (5): 同上
- **distinction from other crate** (1): codes 不同

## 集成待办（不在本轮范围）

- `pc-server` error rendering：用 `describe_responsible_user_denial` 渲染 agent 失败 banner
- `pc-authz`：emit `RESPONSIBLE_USER_UNAUTHORIZED` / `RESPONSIBLE_USER_UNAVAILABLE` code 时附 copy
- `ui/` 错误页：消费 `ResponsibleUserDenialCopy.title / description / recommended_action` 显示
- 端到端：mock agent call denied → banner 显示 "Responsible user not authorized" 文案
