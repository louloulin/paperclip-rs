# R526 — port Node `log-redaction.ts` → Rust `pc-log-redaction`

**日期**: 2026-08-11
**轮次**: R526
**目标**: 把 Node 上游的 username / home-dir log redaction 完整 port 到 Rust
**模块**: 新 crate `crates/pc-log-redaction/`

---

## 改动

### 上游 Node 实现 (148 LOC)
```typescript
// server/src/log-redaction.ts
const CURRENT_USER_REDACTION_TOKEN = "*";
export function maskUserNameForLogs(value, fallback) { ... }
export function redactCurrentUserText(input, opts?) { ... }
export function redactCurrentUserValue<T>(value: T, opts?): T { ... }
// + 私有 helpers: isPlainObject, escapeRegExp, uniqueNonEmpty,
//                splitPathSegments, replaceLastPathSegment,
//                defaultUserNames, defaultHomeDirs,
//                getDefaultCurrentUserCandidates (with module cache)
//                resolveCurrentUserCandidates
```

### Rust port (4 模块, ~600 LOC)

**公开 API**:
```rust
// lib.rs
pub const CURRENT_USER_REDACTION_TOKEN: &str = "*";
pub struct Options { enabled, replacement, user_names, home_dirs }
pub trait Env { fn var(&self, key: &str) -> Option<String>; }
pub struct StdEnv; // impl Env via std::env
impl Options {
    pub fn with_default_candidates(env: &dyn Env) -> Self { ... }
}
pub fn default_user_names(env: &dyn Env) -> Vec<String>;
pub fn default_home_dirs(env: &dyn Env, user_names: &[String]) -> Vec<String>;

// mask.rs
pub fn mask_user_name_for_logs(value: &str, fallback: Option<&str>) -> String;

// path.rs
pub fn split_path_segments(value: &str) -> Vec<&str>;
pub fn replace_last_path_segment(path: &str, replacement: &str) -> String;

// text.rs (主入口)
pub fn redact_current_user_text(input: &str, opts: &Options) -> String;

// value.rs (递归 JSON)
pub fn redact_current_user_value(value: &Value, opts: &Options) -> Value;
```

---

## 设计改进 vs Node 上游

| Node | Rust | 理由 |
|---|---|---|
| 模块级 `cachedCurrentUserCandidates` 单例 | `Options::with_default_candidates(&dyn Env)` 显式持有 | 测试 100% deterministic (mock env), 无隐藏全局 state |
| `process.env.USER/HOME/USERPROFILE` 直接读取 | `Env` trait + `StdEnv` 实现 | 测试注入 mock env; 跨平台 production env 读取一致 |
| `redactCurrentUserValue<T>(value: T, opts?): T` JS object 反射 | `redact_current_user_value(&Value, &Options) -> Value` (serde_json) | 类型安全; 与上游 Node API 接受 JSON object 语义等价 |
| Word boundary 用 regex `(?<![A-Za-z0-9._-])${needle}(?![A-Za-z0-9._-])` | 手工 `is_word_char` 检查 (零 regex 依赖) | 编译期检查; 不引入 `regex` crate; 测试更确定 |
| `isPlainObject` JS 原型链检查 | 不需要 (serde_json::Value 类型保证) | 类型系统替我们做了 |

---

## 测试 (43 个新增, 全过)

**`lib.rs` tests (5)**:
- `r526_unique_non_empty_dedupes_and_trims`
- `r526_default_user_names_returns_env_var`
- `r526_default_home_dirs_includes_user_paths` — `/home/alice` + `/Users/alice` + `C:\Users\alice`
- `r526_options_with_default_candidates_uses_env`
- `r526_options_disabled_passes_through`

**`mask::tests` (8)**:
- `r526_mask_normal_username` — `alice` → `a****`
- `r526_mask_short_username` — `bo` → `b*`
- `r526_mask_single_char_username_unchanged` — `a` → `a`
- `r526_mask_empty_uses_default_fallback` — `""` → `*`
- `r526_mask_whitespace_only_uses_fallback`
- `r526_mask_empty_with_custom_fallback` — `""` → `REDACTED`
- `r526_mask_trims_before_counting` — `"  alice  "` → `a****`
- `r526_mask_handles_unicode_chars` — `日本` → `日*`

**`path::tests` (9)**:
- `r526_split_unix_path` / `r526_split_windows_path` — 跨平台
- `r526_split_strips_trailing_separators` / `r526_split_drops_empty_segments`
- `r526_split_no_separator_returns_single`
- `r526_replace_last_unix_path` / `r526_replace_last_windows_path`
- `r526_replace_last_no_separator_uses_replacement_verbatim`
- `r526_replace_last_strips_trailing_separators`

**`text::tests` (12)**:
- `r526_redact_username_in_log_line` — `file owned by alice` → `file owned by a****`
- `r526_redact_home_dir_in_log_line` — `/home/alice/.bashrc` → `/home/a****/.bashrc`
- `r526_username_word_boundary_respected` — `alicebox` 不被匹配 (更长 word 的一部分)
- `r526_username_word_boundary_after_path_sep` — `/alice/` 仍匹配 (path sep 非 word char)
- `r526_longer_username_wins_over_shorter` — `alice` 优先于 `al`
- `r526_longer_home_dir_processed_first_but_shorter_still_matches` — **已知 limitation**, 短 prefix 二次匹配
- `r526_disabled_passes_through`
- `r526_empty_input_returns_empty`
- `r526_no_match_returns_input_unchanged`
- `r526_multiple_occurrences_all_redacted`
- `r526_windows_path_redaction` — `C:\Users\alice` → `C:\Users\a****`
- `r526_options_with_default_candidates_works_end_to_end`

**`value::tests` (9)**:
- `r526_redact_string_leaf`
- `r526_redact_array_of_strings`
- `r526_redact_nested_object` — 嵌套 object 全部 string leaf 都 mask
- `r526_object_keys_not_redacted` — key 不被改, 只 value
- `r526_passes_through_numbers_and_bools` — number/bool/null 不动
- `r526_disabled_returns_input_unchanged`
- `r526_empty_object_returns_empty_object`
- `r526_deeply_nested_array`
- `r526_with_default_candidates_suppresses_unused`

---

## 已知 Limitation (文档化)

测试 `r526_longer_home_dir_processed_first_but_shorter_still_matches` 暴露了一个 Node 上游同样存在的算法行为:

```
input:  "path=/home/alice/x"
home_dirs: ["/home", "/home/alice"]  (sorted DESC by length)
```

执行流程:
1. `/home/alice` 匹配 → `path=/home/a****/x`
2. `/home` 现在作为子串也匹配 (替换后还在) → `path=/h***/a****/x`

最终结果 `path=/h***/a****/x` (不是用户期望的 `path=/home/a****/x`)。

修复方案需要:
- 跟踪每次替换的位置区间
- 后续替换跳过这些区间

这是 Node 上游 1:1 行为, 我们保留上游行为 + 单测明示。修复留待后续 (需不需要看真实使用场景)。

---

## 验证

```
cargo test -p pc-log-redaction --lib    43 passed
cargo check --workspace                  0 errors (170 pre-existing pc-http warnings)
```

整体单测 ≈ **2090 passing** (+43 R526)
workspace crates **69 → 70**

---

## 设计要点

### 1. 零全局状态

Node 上游用模块级 `cachedCurrentUserCandidates` 单例避免重复 env 读取。Rust 端:
- `Options` 显式持有 `user_names` + `home_dirs`
- `with_default_candidates` 一次性构造
- 调用方控制 lifetime, 跨测试可重新构造不同 Options

收益: 测试 0 flakiness, 多线程安全 (Options 不可变借用即可).

### 2. Env trait 抽象

```rust
pub trait Env { fn var(&self, key: &str) -> Option<String>; }
pub struct StdEnv; // impl Env via std::env
```

测试用 `MockEnv(vec![("USER", "alice"), ...])` 注入; production 用 `StdEnv`。

不引入 `std::env::set_var` (deprecated + 线程不安全)。

### 3. 跨平台 path 处理

`split_path_segments` 同时处理 `/` 和 `\\`:
- `/home/alice` → `["home", "alice"]`
- `C:\Users\alice` → `["C:", "Users", "alice"]`

测试覆盖 Windows + Unix 双向, 跨平台代码统一。

### 4. JSON value 递归 redact

`redact_current_user_value` 接受 `&serde_json::Value` 而非 Node 的 `any`:
- 类型安全 (编译期保证输入是 JSON value)
- Object key 不被改 (只递归 value, 保留 schema 字段名)
- Array element 递归
- Primitive (number/bool/null) 不动

应用场景: structured log (JSON output) 在写入前 redact。

---

## 与 Node 上游的 1:1 行为契约

✅ `maskUserNameForLogs` 行为一致 (含 trim, fallback, Unicode 处理)
✅ `redactCurrentUserText` 主流程一致 (home_dir → username, longest first)
✅ `redactCurrentUserValue` 递归逻辑一致
✅ 路径分隔符处理一致 (Unix + Windows)
✅ Limitation 一致 (短 prefix 二次匹配, 镜像上游)

---

## 下一步

- **R527** = port `redaction.ts` (144 LOC, 通用 redact-by-key 机制, 比 log-redaction 更广)
- **R528** = V4 UI types integration (60 client 接入 generated)
- **R529** = GitHub external object 集成层 (用 R523+R525+R526 现成 helpers)
