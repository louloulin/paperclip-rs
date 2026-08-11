# R530 — port Node `packages/shared/src/agent-url-key.ts` + `project-url-key.ts` → Rust `pc-url-keys`

**日期**: 2026-08-11
**轮次**: R530
**目标**: 把 Node 上游 agent + project URL key 规范化函数 port 到 Rust, 同时替换 pc-agent 内联实现
**模块**: 新 crate `crates/pc-url-keys/` + `pc-agent` 重构

---

## 改动

### 上游 Node 实现 (58 LOC, 2 文件)

`packages/shared/src/agent-url-key.ts`:
```typescript
const AGENT_URL_KEY_DELIM_RE = /[^a-z0-9]+/g;
const AGENT_URL_KEY_TRIM_RE = /^-+|-+$/g;
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

export function isUuidLike(value: string | null | undefined): boolean { ... }
export function normalizeAgentUrlKey(value: string | null | undefined): string | null { ... }
export function deriveAgentUrlKey(name: string | null | undefined, fallback?: string | null): string { ... }
```

`packages/shared/src/project-url-key.ts`:
```typescript
const PROJECT_URL_KEY_DELIM_RE = /[^a-z0-9]+/g;
const PROJECT_URL_KEY_TRIM_RE = /^-+|-+$/g;
const NON_ASCII_RE = /[^\x00-\x7F]/;
const UUID_RE = ...; // same as above

export function normalizeProjectUrlKey(value: string | null | undefined): string | null { ... }
export function hasNonAsciiContent(value: string | null | undefined): boolean { ... }
function shortIdFromUuid(value: string | null | undefined): string | null { ... }  // private
export function deriveProjectUrlKey(name: string | null | undefined, fallback?: string | null): string { ... }
```

### Rust port (单 crate `pc-url-keys`, 2 modules, ~500 LOC, 26 测试)

**公开 API**:
```rust
// 模块 1: agent_url_key
pub static UUID_RE: Lazy<Regex>;  // (?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$
pub fn is_uuid_like(value: &str) -> bool;
pub fn normalize_agent_url_key(value: &str) -> Option<String>;
pub fn derive_agent_url_key(name: Option<&str>, fallback: Option<&str>) -> String;

// 模块 2: project_url_key
pub fn normalize_project_url_key(value: &str) -> Option<String>;  // delegates to agent impl
pub fn has_non_ascii_content(value: &str) -> bool;  // [^\x00-\x7F]
pub fn short_id_from_uuid(value: &str) -> Option<String>;  // first 8 hex
pub fn derive_project_url_key(name: Option<&str>, fallback: Option<&str>) -> String;
```

### pc-agent 重构 (R604 内联实现替换)

**之前** (pc-agent/src/service.rs:1179-1211):
- 内联 `normalize_agent_url_key` (35 行 hand-rolled `prev_dash` 算法)
- 内联 `is_uuid_like` (用 `uuid::Uuid::parse_str`, 需 `uuid` crate 依赖)
- 无 `derive_agent_url_key`
- 无 project URL key 支持

**之后**:
- `pub use pc_url_keys::{is_uuid_like, normalize_agent_url_key};` (line 3)
- 内联实现删除 (节省 ~40 行)
- `derive_agent_url_key` 现可用 (从 pc-url-keys re-export)
- `is_uuid_like` 现用纯 regex, 无需 `uuid` crate 调用

**保持向后兼容**:
- `pc_agent::is_uuid_like` 仍可用 (lib.rs `pub use service::{is_uuid_like, ...}`)
- `pc_agent::normalize_agent_url_key` 仍可用
- `e2e_agent_org_chain.rs` 测试无需修改

---

## 测试 (26 个)

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r530_normalize_basic_rules` | "Hello World" → "hello-world", "  CTO_Engineer  " → "cto-engineer", "researcher2" 同, "---" / "" / "   " → None |
| 2 | `r530_normalize_consecutive_separators_collapse` | "a   b" / "a__b" / "a -_ b" 全部 → "a-b" |
| 3 | `r530_normalize_trims_leading_trailing_dashes` | "!!!hello!!!" / "___hello___" / "  hello  " 全部 → "hello" |
| 4 | `r530_normalize_lowercases` | "MyAgent" / "ALLCAPS" 全部 lowercase |
| 5 | `r530_normalize_preserves_digits` | "Agent 2 Beta" → "agent-2-beta", "v2.0.1" → "v2-0-1" |
| 6 | `r530_normalize_non_ascii_replaced_with_dash` | "héllo" → "h-llo", "hello wörld" → "hello-w-rld", "项目 pro" → "pro" |
| 7 | `r530_is_uuid_like_valid` | 4 种 valid UUID (含 v1, v5, uppercase) |
| 8 | `r530_is_uuid_like_invalid` | 8 种 invalid (空 / 非 UUID / 错 version nibble / 0 / 太短 / 太长 / v6+) |
| 9 | `r530_is_uuid_like_trims` | 前后空白 / tab+newline 都能 trim 后匹配 |
| 10 | `r530_derive_prefers_name` | name 优先, fallback 不被用 |
| 11 | `r530_derive_falls_back_to_fallback` | name=None / "" / "---" → 用 fallback |
| 12 | `r530_derive_default_agent` | 都空 / 都 "---" → "agent" |
| 13-26 | (project_url_key 14 测试) | normalize / has_non_ascii_content / short_id_from_uuid / derive_* 各覆盖 |

`lib.rs` 还加了 2 个 smoke test 验证 re-exports 工作。

---

## 验证

```bash
$ cargo test -p pc-url-keys --lib
running 26 tests
... (all 26 passed)
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo check -p pc-agent
... (1 unrelated warning, compiles clean)

$ cargo test -p pc-agent --lib
test result: ok. 68 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test --workspace --lib
... (all 74 crates pass)
Total passed: 6566; Total failed: 0
```

---

## 设计要点

### 1. R604 内联替换 (节省 ~40 行 + 减少依赖)
- **之前**: pc-agent/service.rs 有 35 行 hand-rolled `normalize_agent_url_key`, 12 行 `is_uuid_like` (用 `uuid::Uuid::parse_str`)
- **之后**: pc-agent/service.rs 有 1 行 `pub use pc_url_keys::{is_uuid_like, normalize_agent_url_key};`
- 净收益: -46 行内联代码, -1 个 uuid::parse_str 调用 (虽然 `uuid` crate 仍因 Uuid::nil / Uuid::new_v4 保留), +26 个单测覆盖

### 2. `Option<&str>` 互斥签名 (强类型)
- Node 上游 `string | null | undefined` 三态, Rust 用 `Option<&str>` 表达 None
- 调用方必须显式 `Some(...)` 或 `None`, 编译期阻止忘记处理 None
- 与 R529 `ConnectionInput` enum 同等设计哲学

### 3. `normalize_agent_url_key` 单 `prev_dash` 算法
- Node: `value.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "")`
- Rust: `for ch in trimmed.chars() { if alnum { push + prev_dash=false } else if !prev_dash { push '-' + prev_dash=true } }`
- 两种实现等价: 连续非 alnum 字符只产生一个 `-`, 然后 trim
- 测试 `r530_normalize_consecutive_separators_collapse` 验证 3 种情况

### 4. `normalize_project_url_key` 复用 agent 算法
- Node 是两个独立函数 (虽然算法相同)
- Rust: `project_url_key::normalize_project_url_key` 内部直接 `return normalize_agent_url_key(value);`
- 节省重复代码; 调用方按语义选函数

### 5. `derive_project_url_key` ASCII fast path
- 算法: base + has_non_ascii → 纯 ASCII 直接返回 base; 否则追加 short UUID suffix
- 测试 `r530_derive_ascii_path_uses_base` 验证即使 fallback 是 UUID, ASCII base 也不追加 suffix
- 测试 `r530_derive_non_ascii_appends_short_uuid` 验证 mixed + non-ASCII 触发 suffix
- 测试 `r530_derive_no_uuid_fallback` 验证 fallback 不是 UUID 时降级

### 6. `pub use` 转 `use` 的踩坑
- 第一次写 `use pc_url_keys::{...}` + `pub use pc_url_keys::{...}` 在同一模块, 触发 E0252 (defined multiple times)
- Rust 语义: `use` 创建私有 binding, `pub use` 创建公开 binding, 同名冲突
- 解决方案: 同一模块只用 `pub use` (既 import 又 re-export)
- `lib.rs` 的 `pub use service::{is_uuid_like, ...}` 仍能透传到 `pc_agent::is_uuid_like`

### 7. 集成层 (留给后续 round)
- UI `src/lib/utils.ts` / `search-query-parser.ts` / `company-portability-sidebar.ts` 保留 (TS 端不动)
- pc-agent 业务层 (`CreateAgent.url_key` derivation) 现可用 `pc_agent::derive_agent_url_key(name, Some(&uuid))`
- `pc-repos::project` 业务层可加 `derive_project_url_key` 调用

---

## V 真实进度更新

| V | R530 前 | R530 后 | 增量 |
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

R530 是**质量层 + 重构**轮次: 替换 pc-agent 内联实现, 减少 ~40 行 + 1 个 uuid crate 调用, 同时新增 `derive_agent_url_key` + 整个 project URL key 模块。

---

## 教训

1. **模块抽取时机**: 第一次写 R604 时 inline 实现是对的 (5 行的小函数); 但 R530 把它提到独立 crate 让 URL key 规范化可独立测试 + 复用, 这是 R604 当时不知道有 project URL key 模块的情况下的局部最优。R530 看到全貌后做提取, 这是合理的演进。
2. **`pub use` vs `use` 语义**: 同模块内 `use foo::bar;` + `pub use foo::bar;` 冲突 (E0252)。最佳: 同一模块只用 `pub use`, 跨模块才用 `use`。
3. **测试预期 vs Node 行为**: `r530_normalize_non_ascii_replaced_with_dash` 最初写错了, 以为 `héllo` → `hllo`, 实际 Node 的 `[^a-z0-9]+` 算法产生 `h-llo`。写测试前先在 Node REPL 跑一下确认行为。
4. **`pub use` 跨 crate 传播**: pc-url-keys 的 `pub fn` 通过 pc-agent 的 `pub use` + lib.rs 的 `pub use` 一路透传到 `pc_agent::is_uuid_like`, 调用方代码完全无感。

---

## 下一步

### R531 (推荐)
- **pc-secret-binding 集成层**: 把 R527 `pc-secret-redaction` + R526 `pc-log-redaction` helpers 接到 `pc-http` middleware
- 处理 `secret_ref` / `user_secret_ref` / `plain` binding 检测 + 整条 response sanitize pipeline
- 让 `/api/companies/:id/audit-log` 这类 endpoint 验证 secret 不漏出

### R532
- **V8 远程 SSH execution**: `restoreRemoteWorkspace` + `materializeRemoteClaudeConfig`
- **V10 plugin 互操作**: spawn 真实 subprocess 跑 plugin

### R533
- **port `packages/shared/pipeline-case-type.ts` (34 LOC) → pc-pipelines::case_type**
- 小, 纯, 用在 routes/pipelines.ts 的 caseType 派生

### R534+ (V11/V12/V13)
- UI 60 client happy 跑
- Playwright 真实 UI 剧本
- 长跑性能 baseline
