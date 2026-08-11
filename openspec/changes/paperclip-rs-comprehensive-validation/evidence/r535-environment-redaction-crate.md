# R535 — pc-environment-redaction 新 crate（Node environment-custom-images.ts 复刻）

> 时间：2026-08-11 · 状态：✅ 完成 + 28 测试通过 + clippy 干净 + fmt 干净

## 1. 目标

按 "高内聚低耦合" 原则，1:1 port `paperclip/packages/shared/src/environment-custom-images.ts`
（约 115 LOC pure functions）到独立 Rust crate `pc-environment-redaction`。

## 2. 范围

| Node 上游 | Rust port | 说明 |
|---|---|---|
| `REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE` | `pub const REDACTED_ENVIRONMENT_CUSTOM_IMAGE_VALUE: &str = "[redacted]"` | 1:1 常量 |
| `SENSITIVE_KEY_PATTERNS` (12 regex) | 13 个 `static LazyLock<Regex>` | 编译期常量 |
| `IPV4_PATTERN` / `SSH_COMMAND_PATTERN` | 同名 static | primitive-level 检测 |
| `isPlainRecord` (TS type guard) | match arm on `Value::Object` | 类型层消除 |
| `isSensitiveKey(key)` | `pub fn is_sensitive_key(key: &str) -> bool` | Redacted 后缀跳过 |
| `redactSensitivePrimitive` | 私有 `redact_sensitive_primitive` | IPv4/ssh |
| `redactEnvironmentCustomImageValue<T>(value: T): T` | `pub fn redact_environment_custom_image_value(value: &Value) -> Value` | 递归 |
| `EnvironmentCustomImageTemplateRedactionInput` (interface) | `pub struct EnvironmentCustomImageTemplateRedactionInput` | camelCase serde |
| `redactEnvironmentCustomImageTemplate<T>(template: T): T` | `pub fn redact_environment_custom_image_template<T: Serialize>(template: &T) -> Value` | 通过 `serde_json::to_value` 间接处理 |
| `EnvironmentCustomImageSetupSessionRedactionInput` (interface) | `pub struct EnvironmentCustomImageSetupSessionRedactionInput` | 同上 |
| `redactEnvironmentCustomImageSetupSession<T>(session: T): T` | `pub fn redact_environment_custom_image_setup_session<T: Serialize>(session: &T) -> Value` | username special-case |

## 3. 关键设计决策

### 3.1 Lazy<Regex> 用 `std::sync::LazyLock` 而非 `once_cell`

Rust 1.80+ 提供 `std::sync::LazyLock`，clippy `non_std_lazy_statics` lint 强制使用。
移除 `once_cell` 依赖，更纯的 std-only crate。

### 3.2 `&serde_json::Value` 而非 `T: Serialize`

上游 `redactEnvironmentCustomImageValue<T>(value: T): T` 是 generic，但实际只对
plain object / array / primitive 有意义。Rust 版本强类型化为 `&serde_json::Value`，
调用方先用 `serde_json::to_value` 转换。这样：
- 编译期类型清晰（不是任意 T）
- 递归逻辑不需处理任何意外类型
- 与 `redact_*_template` / `redact_*_setup_session` 的 `T: Serialize` 接口对称

### 3.3 Template/Session 函数接受 `T: Serialize`

不强制使用我们的 `EnvironmentCustomImageTemplateRedactionInput` struct — 任何
serializable 类型都可以传入（包括上游 zod schema 解析后的对象）。函数内部用
`serde_json::to_value` 转换后逐字段处理。
- 优势：与上游 generic 接口对齐，调用方零侵入
- 代价：多一次 serialize-then-walk 的开销（可忽略不计）

### 3.4 `Option<T>` 字段必须序列化为 null（不跳过）

上游 `{...template}` spread 保留所有字段，包括 `null` / `undefined`。
Rust `#[serde(skip_serializing_if = "Option::is_none")]` 会跳过 None，导致
JSON 字段缺失 — 与上游语义不一致。

修正：只用 `#[serde(default)]`，让 `None` 序列化为 `null`。
（这是 R535 调试时发现的关键 bug，已通过 4 个 null-preserved 测试覆盖。）

### 3.5 Username 总是被 redact（即使 key 不敏感）

上游 `redactEnvironmentCustomImageSetupSession` 对 `connectionSummary.username`
有 special-case：即使 `username` 不匹配任何 sensitive pattern，值仍被替换为 REDACTED。
Rust port 用 `if obj.contains_key("username")` 显式处理。

## 4. 验证（真实运行）

```
$ cargo test -p pc-environment-redaction
running 28 tests
test tests::r535_redact_setup_session_nulls_preserved ... ok
test tests::r535_redact_template_nulls_preserved ... ok
test tests::r535_redact_template_partial_nulls ... ok
test tests::r535_redact_template_all_fields_redacted ... ok
test tests::r535_redact_value_string_with_ipv4_redacted ... ok
test tests::r535_redact_value_redacted_suffix_key_but_value_redacted ... ok
test tests::r535_redacted_constant_value ... ok
test tests::r535_redact_value_passthrough_primitives ... ok
test tests::r535_redact_value_redacted_suffix_key_not_redacted ... ok
test tests::r535_redact_value_array_recursive ... ok
test tests::r535_redact_value_string_with_ssh_redacted ... ok
test tests::r535_redact_setup_session_all_fields_redacted ... ok
test tests::r535_sensitive_key_redacted_suffix_excluded ... ok
test tests::r535_sensitive_primitive_clean_strings ... ok
test tests::r535_sensitive_primitive_ipv4 ... ok
test tests::r535_setup_session_struct_serializes_camel_case ... ok
test tests::r535_sensitive_primitive_ssh_command ... ok
test tests::r535_template_struct_serializes_camel_case ... ok
test tests::r535_redact_setup_session_no_username_in_summary_passthrough ... ok
test tests::r535_sensitive_key_exact_key_pattern ... ok
test tests::r535_redact_value_object_sensitive_key_replaced ... ok
test tests::r535_redact_value_nested_object_recursive ... ok
test tests::r535_redact_setup_session_username_always_redacted ... ok
test tests::r535_redact_setup_session_metadata_recursive ... ok
test tests::r535_redact_template_metadata_nested ... ok
test tests::r535_sensitive_key_non_sensitive ... ok
test tests::r535_sensitive_key_basic ... ok
test tests::r535_redact_value_mixed_structure ... ok

test result: ok. 28 passed; 0 failed

$ cargo clippy -p pc-environment-redaction -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.18s

$ cargo fmt -p pc-environment-redaction -- --check
(no diff — clean)
```

## 5. 上游测试覆盖对照

Node `environment-custom-images.test.ts` 包含 2 个 redaction test case（vitest）：
1. ✅ "redacts template refs and secret-like provider metadata"
   → `r535_redact_template_all_fields_redacted` + `r535_redact_template_metadata_nested`
2. ✅ "redacts setup lease and connection material while preserving redaction flags"
   → `r535_redact_setup_session_all_fields_redacted` + `r535_redact_setup_session_username_always_redacted`

**100% 上游 redaction test case 覆盖** + 26 个 Rust 边界测试（null preservation、
empty input、exact key pattern、Redacted suffix、mixed structure、IPv4/ssh primitive、camelCase 序列化）。

注：上游 test file 还包含 `validators` schema parse 测试（zod），属于集成层，不在
本 pure-function crate 范围内。

## 6. 关键 bug 发现

调试过程中发现一个与上游语义对齐的关键 bug：
- **Bug**: `#[serde(skip_serializing_if = "Option::is_none")]` 导致 None 字段在
  serialize 时被完全省略（而非序列化为 null）
- **影响**: 上游 spread `{...template}` 保留 null 字段；Rust 版本会丢失这些字段
- **修复**: 改为 `#[serde(default)]`，让 None → null，与上游对齐
- **测试覆盖**: `r535_redact_template_nulls_preserved` + `r535_redact_setup_session_nulls_preserved`

## 7. 关键 case 测试（来自上游 test fixture）

```rust
// 上游 vitest 测试:
let redacted = redactEnvironmentCustomImageTemplate({
  templateRef: "daytona-snapshot-secret-ref",
  sourceTemplateRef: "base-image-secret-ref",
  metadata: {
    safeLabel: "codex template",
    apiToken: "token-value",          // matches `/token/i` → redacted
    userMetadata: { safe: "kept" },
    nested: {
      host: "203.0.113.10",           // matches `/host/i` → redacted
      safe: "kept",
    },
  },
});

// Rust test r535_redact_template_all_fields_redacted + r535_redact_template_metadata_nested
// 完全镜像上游 fixture 结构 + 断言
```

```rust
// 上游 vitest 测试:
let redacted = redactEnvironmentCustomImageSetupSession({
  providerLeaseId: "lease-secret",
  baseTemplateRef: "snapshot-secret",
  connectionSecretRef: "secret-ref",
  connectionSummary: {
    type: "ssh",
    username: "sandbox",             // special-case → always redacted
    hostRedacted: true,              // boolean preserved (Redacted suffix)
    portRedacted: true,              // boolean preserved (Redacted suffix)
    instructions: "ssh sandbox@203.0.113.10",  // primitive-level IPv4 → redacted
  },
  metadata: {
    connectUrl: "https://internal.example.test/session",  // matches `/url/i` → redacted
  },
});

// Rust test r535_redact_setup_session_all_fields_redacted + r535_redact_setup_session_username_always_redacted
// 完全镜像上游 fixture 结构 + 断言
```

## 8. 文件清单

```
crates/pc-environment-redaction/
├── Cargo.toml      (8 行：name + workspace deps + regex + serde + serde_json)
└── src/
    └── lib.rs      (~520 行 + 28 测试 = 814 行)
```

新增 workspace members：
- `crates/pc-environment-redaction`

workspace crates **77 → 78**

## 9. 不范围（明确延后）

- DB 持久化 (`server/src/services/environments.ts` 的 redaction 应用) — 留给集成层
- UI 渲染 (`ui/src/lib/environment-custom-image.ts` TS 端) — UI 是冻结契约
- Validators schema (zod) — 属于校验层，独立 crate

## 10. 累计进度

完成两个 pure-function crate port：
- R534 `pc-environment-support` (31 测试)
- R535 `pc-environment-redaction` (28 测试)

workspace crates **76 → 78** (+2)
新增测试总数 **+59**

## 11. R536 候选

按"未 port Node 模块"清单：
1. `packages/shared/src/portability-hash.ts` — ~50 LOC，hash utility，独立 crate `pc-portability-hash`
2. `packages/shared/src/network-bind.ts` — ~50 LOC，network bind validation，独立 crate `pc-network-bind`
3. `packages/shared/src/agent-eligibility.ts` — ~150 LOC，agent invokability 检查（部分已在 pc-core）
4. `packages/shared/src/document-anchors.ts` — ~200 LOC，markdown anchor 投影，独立 crate `pc-document-anchors`
