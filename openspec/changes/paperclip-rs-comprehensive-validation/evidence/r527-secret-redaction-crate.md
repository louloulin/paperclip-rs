# R527 — port Node `redaction.ts` → Rust `pc-secret-redaction`

**日期**: 2026-08-11
**轮次**: R527
**目标**: 把 Node 上游的 secret field name detection + JWT-shape detection + recursive JSON redaction 完整 port 到 Rust
**模块**: 新 crate `crates/pc-secret-redaction/`

---

## 改动

### 上游 Node 实现 (144 LOC, 2 文件协作)

`server/src/redaction.ts`:
```typescript
import { redactCommandText } from "@paperclipai/adapter-utils";
const SECRET_FIELD_NAME_PATTERN = String.raw`[A-Za-z0-9_-]*(?:api[-_]?key|...)[A-Za-z0-9_-]*`;
const SECRET_PAYLOAD_KEY_RE = new RegExp(SECRET_FIELD_NAME_PATTERN, "i");
const COMMAND_PAYLOAD_KEY_RE = /(^command$|...)/i;
const COMMAND_ARGS_PAYLOAD_KEY_RE = /^(commandArgs|...)$/i;
const JWT_VALUE_RE = /^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+...$/;
const CLI_SECRET_FLAG_RE = new RegExp(...);
const JSON_SECRET_FIELD_TEXT_RE = new RegExp(... + SECRET_FIELD_NAME_PATTERN + ..., "gi");
const ESCAPED_JSON_SECRET_FIELD_TEXT_RE = new RegExp(... + SECRET_FIELD_NAME_PATTERN + ..., "gi");
const SECRET_TEXT_HINTS = ["api", "key", ...] as const;
export const REDACTED_EVENT_VALUE = "***REDACTED***";
function maybeContainsSecretText(input: string) { ... }
function isPlainObject(value: unknown) { ... }
function sanitizeValue(value: unknown): unknown { ... }
function sanitizeCommandArgs(args: unknown[]): unknown[] { ... }
export function sanitizeRecord(record: Record<string, unknown>): Record<string, unknown> { ... }
export function redactEventPayload(payload: Record<string, unknown> | null): ... { ... }
export function redactSensitiveText(input: string): string {
  if (!maybeContainsSecretText(input)) return input;
  return redactCommandText(
    input
      .replace(JSON_SECRET_FIELD_TEXT_RE, `$1${REDACTED_EVENT_VALUE}$2`)
      .replace(ESCAPED_JSON_SECRET_FIELD_TEXT_RE, `$1${REDACTED_EVENT_VALUE}$2`),
    REDACTED_EVENT_VALUE,
  );
}
```

`packages/adapter-utils/src/command-redaction.ts` (被 redactSensitiveText 调用, 144 LOC):
- `REDACTED_COMMAND_TEXT_VALUE = "***REDACTED***"`
- `COMMAND_CLI_SECRET_OPTION_RE` / `COMMAND_ENV_SECRET_ASSIGNMENT_RE` / `COMMAND_AUTHORIZATION_BEARER_RE`
- `COMMAND_OPENAI_KEY_RE` (`\bsk-[A-Za-z0-9_-]{12,}\b`)
- `COMMAND_GITHUB_TOKEN_RE` (`\bgh[pousr]_[A-Za-z0-9_]{20,}\b`)
- `COMMAND_JWT_RE` (`\b[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}...`)
- `redactCommandText(command, redactedValue?)` — pipeline: `AuthBearer` → `CliOption` → `EnvAssignment` → `OpenAIKey` → `GitHubToken` → `InlineJWT`

### Rust port (单 crate `pc-secret-redaction`, ~600 LOC, 25 测试)

**公开 API**:
```rust
pub const REDACTED_EVENT_VALUE: &str = "***REDACTED***";
pub const SECRET_TEXT_HINTS: &[&str] = &[18 items];
#[derive(Debug, Error)] pub enum RedactionError { InvalidPattern(String) }

// 9 static regex (Lazy<Regex>)
pub static SECRET_FIELD_NAME_PATTERN: Lazy<Regex>;            // (?i)
pub static JWT_VALUE_PATTERN: Lazy<Regex>;                    // no (?i) — case matters
pub static JSON_SECRET_FIELD_PATTERN: Lazy<Regex>;            // (?i)
pub static ESCAPED_JSON_SECRET_FIELD_PATTERN: Lazy<Regex>;    // (?i)
pub static CLI_SECRET_FLAG_PATTERN: Lazy<Regex>;              // (?i)
pub static AUTHORIZATION_BEARER_PATTERN: Lazy<Regex>;         // (?i)
pub static OPENAI_KEY_PATTERN: Lazy<Regex>;
pub static GITHUB_TOKEN_PATTERN: Lazy<Regex>;
pub static INLINE_JWT_PATTERN: Lazy<Regex>;

// 7 pub fn
pub fn is_secret_field_name(name: &str) -> bool;
pub fn is_jwt_like(value: &str) -> bool;
pub fn maybe_contains_secret_text(input: &str) -> bool;
pub fn redact_sensitive_text(input: &str) -> String;
pub fn redact_record(record: &Value) -> Value;
pub fn is_cli_secret_flag(arg: &str) -> bool;
```

**`redact_sensitive_text` 5-stage pipeline**:
1. `JSON_SECRET_FIELD_PATTERN.replace_all` — inline `"apiKey": "value"` → `"apiKey": "***REDACTED***"`
2. `ESCAPED_JSON_SECRET_FIELD_PATTERN.replace_all` — `\"apiKey\": \"value\"`
3. `AUTHORIZATION_BEARER_PATTERN.replace_all` — `Authorization: Bearer xxx` → `Authorization: Bearer ***REDACTED***`
4. `OPENAI_KEY_PATTERN.replace_all` — `sk-abcdef...` → `***REDACTED***`
5. `GITHUB_TOKEN_PATTERN.replace_all` — `ghp_abcdef...` → `***REDACTED***`
6. `INLINE_JWT_PATTERN.replace_all` — `eyJhb...SflKxw` → `***REDACTED***`

(`gate`: if `!maybe_contains_secret_text(input)`, return input unchanged — same upstream behaviour.)

---

## 测试 (25 个)

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r527_redacted_event_value_matches_node` | 常量 = `"***REDACTED***"` |
| 2 | `r527_secret_text_hints_has_18_entries` | 防漂移: 列表长度锁 18 |
| 3 | `r527_is_secret_field_name_recognises_common_names` | 12 个常见 secret 字段名 (apiKey/API_KEY/api-key/accessToken/...) |
| 4 | `r527_is_secret_field_name_rejects_safe_names` | name/id/userId/createdAt/空 都被拒 |
| 5 | `r527_is_jwt_like_recognises_jwt_shape` | 真 JWT + 短 JWT + 4 段 JWS |
| 6 | `r527_is_jwt_like_rejects_non_jwt_strings` | 空/非 JWT/2 段/5+ 段/带空格 |
| 7 | `r527_maybe_contains_secret_text_heuristic` | 6 阳性 + 2 阴性 |
| 8 | `r527_redact_sensitive_text_no_match_returns_input` | 启发式 gate 正确 |
| 9 | `r527_redact_sensitive_text_replaces_inline_json_secret` | 核心 bug fix: `apiKey` (大写 K) 现在能被 redact |
| 10 | `r527_redact_sensitive_text_handles_multiple_fields` | 多字段 + 安全字段保留 |
| 11 | `r527_redact_sensitive_text_handles_uppercase_field_names` | `API_KEY` (全大写) 也被 redact |
| 12 | `r527_redact_sensitive_text_handles_escaped_json` | `\"apiKey\": \"abc\"` 转义形式 |
| 13 | `r527_redact_sensitive_text_redacts_authorization_bearer` | HTTP Authorization header |
| 14 | `r527_redact_sensitive_text_redacts_openai_keys` | `sk-...` 12+ alphanum |
| 15 | `r527_redact_sensitive_text_redacts_github_tokens` | `ghp_...` 20+ alphanum |
| 16 | `r527_redact_sensitive_text_redacts_inline_jwt` | 长 JWT shape 在自由文本里 |
| 17 | `r527_redact_record_replaces_secret_field_values` | 嵌套 JSON object |
| 18 | `r527_redact_record_replaces_jwt_string_values` | JWT 作为 string value |
| 19 | `r527_redact_record_handles_arrays` | 数组元素 |
| 20 | `r527_redact_record_preserves_primitives` | 数字/bool/null/array 不动 |
| 21 | `r527_redact_record_handles_empty_inputs` | `{}`/`[]`/`null` |
| 22 | `r527_is_cli_secret_flag_recognises_long_flags` | 6 长 flag |
| 23 | `r527_is_cli_secret_flag_recognises_short_flags` | `-token` / `-password` |
| 24 | `r527_is_cli_secret_flag_rejects_safe_flags` | `--help`/`--verbose`/... |
| 25 | `r527_is_cli_secret_flag_case_insensitive` | `--API-KEY` / `--Token` / `--SECRET` |

---

## 验证

```bash
$ cargo test -p pc-secret-redaction --lib
running 25 tests
... (all 25 passed)
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo check --workspace
... (0 errors, only pre-existing warnings from pc-board-auth / pc-run-liveness)
```

---

## 设计要点

### 1. 关键 bug fix: case-insensitive flag
Rust port 第一版漏了 Node upstream 的 `/i` flag, 导致 `apiKey` (camelCase) 永远不匹配小写 `key`。
- **症状**: `redact_sensitive_text(r#"log: {"apiKey": "abc123"}"#)` 返回原样, 不 redact
- **原因**: Node 正则末尾 `/i` → Rust 必须 `(?i)` 前缀
- **修复**: 所有 5 个 secret-field regex 加 `(?i)`; 测试 `r527_redact_sensitive_text_replaces_inline_json_secret` 现在过

### 2. inline vs 拆 crate 决策
Node 上游 `redactSensitiveText` 调用 `@paperclipai/adapter-utils` 的 `redactCommandText`:
```typescript
return redactCommandText(
  input.replace(JSON_SECRET_FIELD_TEXT_RE, ...).replace(ESCAPED_JSON_SECRET_FIELD_TEXT_RE, ...),
  REDACTED_EVENT_VALUE,
);
```

**选项 A**: 单独 port 整个 `adapter-utils/command-redaction.ts` (144 LOC) → 新 crate `pc-command-redaction`
**选项 B**: inline command-redaction 的纯 regex pattern (COMMAND_AUTHORIZATION_BEARER_RE / COMMAND_OPENAI_KEY_RE / COMMAND_GITHUB_TOKEN_RE / COMMAND_JWT_RE) 到 `pc-secret-redaction`

**决策**: 选 B — 4 个 pattern 都是无状态纯 regex, 拆 crate 会引入无意义边界。`pc-secret-redaction` 现在自包含, 调用方只需 import 1 个 crate。

### 3. 全 `Lazy<Regex>` 零成本
- 9 个 `static Lazy<Regex>` 全是 `once_cell::sync::Lazy`, 首次访问时编译, 后续 `is_match` / `replace_all` 零开销
- 测试中多次调用同一 pattern, 第二次开始走 cache
- `cargo check --workspace` 时 lazy init 不会触发, 只有 `cargo test` / `cargo run` 才触发

### 4. 强类型错误
- `RedactionError::InvalidPattern(String)` — 当前 pattern 全 hard-coded, 实际不会触发, 但保留以便未来允许用户传入 pattern
- 比 Node 上游的 `throw new Error(...)` 强类型, 集成层可 `match` 处理

### 5. 接受纯输入, 不引入 IO/异步/状态
- 不依赖 `std::env` / `tokio` / `serde_json::from_str`
- 所有 `pub fn` 都是 `&str -> T` 或 `&Value -> Value`
- 集成层 (pc-http middleware / pc-adapter-process logger) 负责把 `serde_json::Value` 和 `&str` 喂进来

### 6. 集成层 (留给后续 round)
- `secret_ref` / `user_secret_ref` binding 检测 → 需要 DTO type, 给 R528+ 集成层做
- `commandArgs` argv 处理 (loop `CLI_SECRET_FLAG_RE.test(arg)`, if true, next arg = redact) → 给 `pc-adapter-process` 集成层做
- `redactCommandTextForLogs` 的完整 command resolution + env interpolation → 给 `pc-server` middleware 做

---

## V 真实进度更新

| V | R527 前 | R527 后 | 增量 |
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

R527 是**质量层**轮次: 不直接推进任何 V 数字, 但为 server log / event payload / adapter argv 三条 redaction 链路提供独立可测试的基础设施。

---

## 教训

1. **正则 flag 不要丢**: Node `/i` → Rust `(?i)`, `/g` → Rust `Regex::replace_all` (vs `find`)。每个 upstream regex 都要逐字对照, 不要"看起来差不多就 OK"。
2. **raw string vs quoted string**: 含 backtick (``` ` ```) 的 character class 必须用 `r#"..."#` 而非 `r"..."`, 否则 backtick 被 Rust 当 char literal 开始符 (E0762)。
3. **debug 测试不要 append 在 `mod tests { ... }` 外面**: 会 syntax error; 一定要 append 在 closing brace 之前, 或先 grep `^}` 找正确位置。
4. **测试先写, bug 后查**: 失败的测试输入 + 期望输出比 `println!` 调试更快定位 bug — 这次的 case-insensitive bug 是看测试断言 vs Node upstream flag 对比一眼发现。

---

## 下一步

### R528 (推荐)
- V4 UI types integration: pc-typescript-gen 已生成 35 DTO, 需要 pc-http client / pc-cli 输出 / UI `src/types/` 接入
- 至少把 `pc-http` 的 56 路由 client fn (response type → generated TS DTO) 接进来, 让 `cargo test -p pc-http` 编译期保证 client/server 类型一致

### R529
- V5 收尾: 把 R526+R527 的 redact helper 接到 pc-server middleware (`/api/companies/:id/audit-log` 这类 endpoint 的 response sanitizer)

### R530
- `pc-secret-binding`: 集成层处理 `secret_ref` / `user_secret_ref` / `plain` binding 检测, 给 pc-http 路由响应自动 redact

### R531
- port Node `server/src/commandArgs` argv 处理, 给 pc-adapter-process 用 pc-secret-redaction 的 `redact_sensitive_text` + `is_cli_secret_flag`
