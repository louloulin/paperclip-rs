# R529 — port Node `packages/shared/src/humanize-connection.ts` → Rust `pc-connection-display`

**日期**: 2026-08-11
**轮次**: R529
**目标**: 把 Node 上游 connection / tool identifier humanizer 完整 port 到 Rust
**模块**: 新 crate `crates/pc-connection-display/`

---

## 改动

### 上游 Node 实现 (88 LOC)

`packages/shared/src/humanize-connection.ts`:
```typescript
interface HumanizableConnection { name: string; }
type ConnectionLike = HumanizableConnection | string | null | undefined;

function rawNameOf(input) { ... }                          // string/object/null → trimmed string
function looksLikeNetworkAddress(raw) { ... }              // IP / URL / host:port / localhost
function titleCaseIdentifier(value) { ... }                // snake/kebab/dotted → Title Case
function pluginPackageLabel(raw) { ... }                   // Plugin: … → leaf name

export function humanizeConnectionDisplayName(input, options = {}) { ... }
export function connectionDisplaySecondaryHint(input) { ... }
```

`packages/shared/src/humanize-connection.test.ts` (62 LOC, 8 test cases):
- hides raw IPs/hosts (5 forms)
- drops Plugin: prefix and title-cases leaf (2 forms)
- vendor:tool ids (2 forms)
- bare snake/kebab identifier (2 forms)
- passes through human names (3 forms)
- explicit title precedence (2 forms)
- connection-like object + empty input (3 forms)
- secondary hint network-only (5 forms)

### Rust port (单 crate `pc-connection-display`, ~400 LOC, 18 测试)

**公开 API**:
```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanizableConnection { pub name: String }

#[derive(Debug, Clone, Default)]
pub struct HumanizeOptions { pub title: Option<String> }
impl HumanizeOptions {
    pub fn new() -> Self;
    pub fn with_title<T: Into<String>>(mut self, title: T) -> Self;
}

#[derive(Debug, Clone, Copy)]
pub enum ConnectionInput<'a> {
    Raw(&'a str),
    Object(&'a HumanizableConnection),
    None,
}

pub static IPV4_RE: Lazy<Regex>;       // ^\d{1,3}(\.\d{1,3}){3}(:\d+)?$
pub static HOST_PORT_RE: Lazy<Regex>;  // ^[a-z0-9.-]+:\d+$

pub fn humanize_connection_display_name(input: ConnectionInput<'_>, options: &HumanizeOptions) -> String;
pub fn humanize_connection_display_name_str(raw: &str, options: &HumanizeOptions) -> String;
pub fn humanize_connection_display_name_obj(conn: &HumanizableConnection, options: &HumanizeOptions) -> String;
pub fn connection_display_secondary_hint(input: ConnectionInput<'_>) -> Option<String>;
```

**私有 helpers**:
- `raw_name_of(input) -> String` — enum 输入转换
- `looks_like_network_address(raw) -> bool` — 4 类网络地址检测
- `title_case_identifier(value) -> String` — snake/kebab/dotted → Title Case
- `plugin_package_label(raw) -> Option<String>` — 5 种 Plugin 形式

---

## 测试 (18 个)

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r529_hides_raw_ips_and_hosts_behind_generic_label` | 5 种网络地址 (IP/IP:port/localhost/host:port/URL) → "Custom app" |
| 2 | `r529_drops_plugin_prefix_and_title_cases_leaf` | 2 种 Plugin 形式 (paperclipai.plugin-briefs, acme.plugin-weekly-report) |
| 3 | `r529_vendor_tool_ids_become_title_case` | 2 种 vendor:tool (mcp-remote-fixture:update_note, github:create_issue) |
| 4 | `r529_title_cases_bare_snake_kebab_identifier` | 2 种 (update_note, send-email) |
| 5 | `r529_passes_through_normal_human_app_names` | 3 种 (Zapier, Notion, Google Drive) |
| 6 | `r529_prefers_explicit_title_when_provided` | title="Update note" 强制覆盖 |
| 7 | `r529_blank_title_falls_back_to_derivation` | title="   " → fallback 推导 |
| 8 | `r529_accepts_connection_like_object_and_handles_empty` | 对象形式 + 空字符串 |
| 9 | `r529_none_input_returns_custom_app` | `ConnectionInput::None` |
| 10 | `r529_handles_dotted_identifier` | 2 种 dotted (paperclip.briefs, notion.database) |
| 11 | `r529_handles_uppercase_plugin_prefix` | PLUGIN: 也匹配 (case-insensitive) |
| 12 | `r529_handles_plugin_with_underscore_separator` | `plugin_weekly-report` (下划线 separator) |
| 13 | `r529_handles_plugin_with_no_dotted_package` | `Plugin: plugin-briefs` (无 vendor prefix) |
| 14 | `r529_secondary_hint_for_network_addresses` | 2 种网络地址 → "hosted at …" |
| 15 | `r529_secondary_hint_null_for_non_network` | 5 种非网络 (Zapier/Plugin/empty) → None |
| 16 | `r529_secondary_hint_with_object_form` | 对象形式 + URL |
| 17 | `r529_title_case_identifier_handles_mixed_input` | 7 种 (empty/---/single + 4 复合) |
| 18 | `r529_looks_like_network_address_edge_cases` | 6 种边界 (4 段/3 段/非数字/无 port/有 port) |

---

## 验证

```bash
$ cargo test -p pc-connection-display --lib
running 18 tests
... (all 18 passed)
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test --workspace --lib
... (all 73 crates pass)
Total passed: 6540; Total failed: 0
```

---

## 设计要点

### 1. `enum ConnectionInput` 强类型互斥
- 第一次写 `(input: Option<&HumanizableConnection>, raw: Option<&str>)` 双 Option 参数, **违反 Node 上游语义**
- Node 的 `rawNameOf(input)` 只接受单一输入 (string 或 object), 写 Rust 时 API 不该有歧义
- 改用 enum `ConnectionInput::Raw | Object | None`, 编译期阻止双重输入
- 测试 `r529_object_form_takes_priority_over_raw` 原本验证"both 时谁优先" → 删除 (API 不允许 both)

### 2. 5 种 Plugin label 形式
- `Plugin: vendor.plugin-leaf` (标准)
- `PLUGIN:` (case-insensitive prefix)
- `Plugin: plugin-briefs` (无 dotted vendor prefix)
- `Plugin: acme.plugin_weekly-report` (下划线 separator)
- `Plugin: <empty>` (空 leaf → "Custom app" fallback)

每个 case 都有独立测试, 防止 case sensitivity / separator 类型变化时漏改。

### 3. `title_case_identifier` 用 `char::is_whitespace` 而非 `/\s/`
- Node 用 `[\s._-]` 分隔, Rust 直接用 char predicates
- `is_whitespace` 自动覆盖 `
	 ` 等所有 unicode whitespace, 与上游 `\s` 等价
- 支持任意 unicode 输入 (e.g. 中文 / emoji 都按 word 分割)

### 4. URL 含 `://` 优先于 `vendor:tool` 匹配
- 检查顺序: title > 网络地址 > Plugin: > vendor:tool > 已 human > snake/kebab
- `looksLikeNetworkAddress` 先匹配 (`https://mcp.example.com/sse` 含 `://` → Custom app)
- 即使后面 `raw.contains(':')` 也成立, 因为前面的 early return
- 测试 `r529_hides_raw_ips_and_hosts_behind_generic_label` 验证

### 5. `connection_display_secondary_hint` 不依赖主函数
- 完全独立的判断逻辑, 只看 `looks_like_network_address`
- 返回 `Some("hosted at {raw}")` 给 UI 作为副标题
- 测试 `r529_secondary_hint_*` 验证 6 种情况

### 6. UI / 集成层 (留给后续 round)
- UI `src/lib/connection-display.ts` 保留 (TS 端不动, UI 冻结契约)
- 如果以后 pc-server 要在 response 里 pre-compute `display_name`, 直接 import 这个 crate
- Node 上游这模块**只**给 UI 端用, 不在 server 端, 所以 Rust port 暂无 caller

---

## V 真实进度更新

| V | R529 前 | R529 后 | 增量 |
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

R529 是**质量层**轮次: 给 UI `/apps` `/apps/attention` `/apps/advanced` 提供 Rust 端的 display name derivation (虽然 UI 暂时仍用 TS 版, 但 pc-server 集成时可 import)。

---

## 教训

1. **API 互斥用 enum**: 写 `Option<A>, Option<B>` 时容易引入歧义 (both 时谁优先?). 强类型 enum 编译期阻止. Node 的 union type 自动强类型, Rust `Option<A>, Option<B>` 失去这层保护.
2. **测试驱动修正 API**: 第一次写完测试, 最后加的 `r529_object_form_takes_priority_over_raw` 让我意识到 API 错了一一直接重构 `enum ConnectionInput`, 比硬塞优先规则干净.
3. **Plugin prefix 5 种变体**: 不能只测最常见的 `Plugin: vendor.plugin-leaf`, 其他 4 种 (uppercase, underscore, no-vendor, empty) 都得测, 防止上游某次改动漏掉.

---

## 下一步

### R530 (推荐)
- **port `packages/shared/issue-attribution.ts` (57 LOC)** — 小, 用在 DB migration 里
- 或者 **port `packages/shared/execution-workspace-guards.ts` (19 LOC)** — 已部分 port 到 pc-core, 验证完整性

### R531
- **pc-secret-binding 集成层**: 把 R527 `pc-secret-redaction` + R526 `pc-log-redaction` helpers 接到 `pc-http` middleware
- 处理 `secret_ref` / `user_secret_ref` / `plain` binding 检测 + 整条 response sanitize pipeline
- 让 `/api/companies/:id/audit-log` 这类 endpoint 验证 secret 不漏出

### R532
- **V8 远程 SSH execution**: `restoreRemoteWorkspace` + `materializeRemoteClaudeConfig`
- **V10 plugin 互操作**: spawn 真实 subprocess 跑 plugin

### R533+ (V11/V12/V13)
- UI 60 client happy 跑
- Playwright 真实 UI 剧本
- 长跑性能 baseline
