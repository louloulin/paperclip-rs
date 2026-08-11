# R528 — port Node `packages/shared/src/issue-references.ts` → Rust `pc-issue-references`

**日期**: 2026-08-11
**轮次**: R528
**目标**: 把 Node 上游 issue reference 解析 (markdown 里找出 /issues/PAP-123 等引用) 完整 port 到 Rust
**模块**: 新 crate `crates/pc-issue-references/`

---

## 改动

### 上游 Node 实现 (188 LOC)

`packages/shared/src/issue-references.ts`:
```typescript
export const ISSUE_REFERENCE_IDENTIFIER_RE = /^[A-Z][A-Z0-9]*-\d+$/;
const ISSUE_REFERENCE_TOKEN_RE = /https?:\/\/[^\s<>()]+|\/[^\s<>()]+|[A-Z][A-Z0-9]*-\d+/gi;

interface IssueReferenceMatch { index; length; identifier; matchedText; }

// helpers
function preserveNewlinesAsWhitespace(value) { ... }
function stripMarkdownCode(markdown) { ... }  // strips ``` and `...` spans
function trimTrailingPunctuation(token) { ... }  // paren/bracket-aware

// public
function normalizeIssueIdentifier(value) { ... }  // "pap-123" → "PAP-123"
function buildIssueReferenceHref(identifier) { ... }
function parseIssueReferenceHref(href) { ... }  // URL parsing via URL constructor
function findIssueReferenceMatches(text) { ... }  // scan + dedup
function extractIssueReferenceIdentifiers(markdown) { ... }  // markdown → unique IDs
function extractIssueReferenceMatches(markdown) { ... }  // markdown → unique matches
```

`packages/shared/src/issue-references.test.ts` (69 LOC, 7 test cases):
- normalize uppercase
- parse relative + absolute + fragment href
- build canonical href
- find in plain text + trim trailing `]`
- extract + dedup
- skip inline code + fenced code blocks

### Rust port (单 crate `pc-issue-references`, ~500 LOC, 23 测试)

**公开 API**:
```rust
pub static ISSUE_REFERENCE_IDENTIFIER_RE: Lazy<Regex>;  // ^[A-Z][A-Z0-9]*-\d+$
pub static ISSUE_REFERENCE_TOKEN_RE: Lazy<Regex>;       // (?i) URL|path|identifier

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueReferenceMatch {
    pub index: usize,
    pub length: usize,
    pub identifier: String,
    pub matched_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueIdentifierRef { pub identifier: String }

// 7 pub fn
pub fn normalize_issue_identifier(value: &str) -> Option<String>;
pub fn build_issue_reference_href(identifier: &str) -> String;
pub fn parse_issue_reference_href(href: &str) -> Option<IssueIdentifierRef>;
pub fn find_issue_reference_matches(text: &str) -> Vec<IssueReferenceMatch>;
pub fn extract_issue_reference_identifiers(markdown: &str) -> Vec<String>;
pub fn extract_issue_reference_matches(markdown: &str) -> Vec<IssueReferenceMatch>;
```

**私有 helpers**:
- `trim_trailing_punctuation(token: &str) -> String` — parens-aware
- `preserve_newlines_as_whitespace(value: &str) -> String` — 保留 newline
- `strip_markdown_code(markdown: &str) -> String` — 完整 hand-written 实现
- `detect_fence_opener(s: &str) -> Option<(char, usize)>` — 返回 `(char, len)`
- `scan_for_backtick_run(s: &str, n: usize) -> Option<usize>`

---

## 测试 (23 个)

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r528_normalize_uppercases_valid_identifiers` | `pap-123` → `PAP-123`, `pc1a2-7` → `PC1A2-7` |
| 2 | `r528_normalize_trims_whitespace` | `"  pap-1  "` → `PAP-1` |
| 3 | `r528_normalize_rejects_invalid` | 8 种 invalid 输入 (空/无 dash/无 number/下划线/数字开头) 全部 None |
| 4 | `r528_build_href_canonical_form` | `pap-123` → `/issues/PAP-123` |
| 5 | `r528_build_href_falls_back_on_invalid` | invalid input 仍然产生 path (trim 后) |
| 6 | `r528_parse_relative_href` | `/issues/PAP-123` + `/PAP/issues/pap-456` |
| 7 | `r528_parse_absolute_href_with_fragment` | `https://...pap-789#comment-1` → `PAP-789` |
| 8 | `r528_parse_rejects_non_issue_path` | `/projects/PAP-789` → None; 空字符串/空白 → None |
| 9 | `r528_parse_rejects_malformed_url` | `not a url`, `ht://bad` → None |
| 10 | `r528_find_matches_plain_text` | 3 tokens (identifier + path + URL) 顺序/长度/内容全对 |
| 11 | `r528_find_matches_trims_trailing_bracket` | `/issues/PAP-123]` → `/issues/PAP-123` |
| 12 | `r528_find_matches_does_not_capture_outer_parens` | `(PAP-1)` 整体不被 regex 捕获, 只匹配 `PAP-1` (因 `[A-Z][A-Z0-9]*-\d+` 不含 `(`) |
| 13 | `r528_find_matches_parens_unbalanced_trims` | `PAP-1)` 不平衡 `)` 被 trim |
| 14 | `r528_find_matches_handles_unbalanced_bracket` | `PAP-1]` 不平衡 `]` 被 trim |
| 15 | `r528_find_matches_empty_input` | 空输入 → `[]` |
| 16 | `r528_extract_dedupes_identifiers` | `[again](/issues/pap-1)` + `PAP-1` 只算一次 |
| 17 | `r528_extract_skips_inline_code_and_fenced_blocks` | 9 行 markdown, 含 inline code + fenced, 只识别 `PAP-1` 和 `PAP-5` |
| 18 | `r528_extract_matches_dedupes` | 4 种引用形式 (id/path/URL) 全部 dedupe 到 `PAP-1` |
| 19 | `r528_strip_inline_code_preserves_lines` | newline 保留, backtick run 替换为等长 spaces |
| 20 | `r528_strip_fenced_with_tilde_fence` | `~~~` fence 也能识别 |
| 21 | `r528_strip_unmatched_inline_code_keeps_literal` | 无闭合 backtick → literal 保留 |
| 22 | `r528_strip_empty_input` | 空 markdown → 空字符串 |
| 23 | (overall 7 个 `r528_*` 测试镜像 Node 7 个 `it(...)`) | |

---

## 验证

```bash
$ cargo test -p pc-issue-references --lib
running 23 tests
... (all 23 passed)
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo check --workspace
... (0 errors, 3 pre-existing warnings from unrelated crates)
```

---

## 设计要点

### 1. 全 `(?i)` case-insensitive
- 上游 `/i` flag, Rust 必须 `(?i)` 前缀
- 否则 `pap-123` 不匹配 `[A-Z][A-Z0-9]*-\d+`
- 测试 `r528_normalize_uppercases_valid_identifiers` 验证 lowercase 输入也能识别

### 2. `url::Url::parse` 替代 `URL` constructor
- Node: `new URL(raw)` + try/catch, 失败返回 null
- Rust: `Url::parse(raw).ok()?` 同样语义
- 相对路径用 `https://paperclip.invalid` 作 base 拼接 (与上游一致)
- 测试 `r528_parse_rejects_malformed_url` 验证 2 种 invalid URL → None

### 3. Fence opener 用 `(char, usize)` 而非 `&'static str`
- 第一次实现用 `regex_fence_start -> Option<&'static str>` 试图返回 runtime-length fence string
- 但 `&'static str` 不能引用动态字符串, 必然出错
- 改成返回 `(fence_char, fence_len)`, 使用时构造 `String`, 然后 `starts_with(&fence)`
- 干净利落, 支持任意 3+/4+/5+ backticks 和 tildes

### 4. 完全 hand-written `strip_markdown_code`
- 不引入 `pulldown-cmark` 或 `comrak` 等 markdown crate 依赖
- 整个函数 ~80 行, 显式 byte-level 操作
- 保留 newline structure (其他字符替换为 space), 后续正则扫描时 byte offset 一致
- 测试 `r528_strip_inline_code_preserves_lines` 验证 newline 位置不变

### 5. Paren-aware trim
- Node: `(` count >= `)` count 时保留 `)`, 否则 trim
- Rust: 同等语义, 用 `str::matches` 计数
- 测试 `r528_find_matches_parens_unbalanced_trims` + `r528_find_matches_does_not_capture_outer_parens` 两种情况

### 6. 集成层 (留给后续 round)
- `server/src/services/issue-references.ts` 的 `issueReferenceService` (DB 持久化 + 跨 service 协调)
- Node 上游调用方 (待集成时再 wire):
  - `server/src/routes/costs.ts`, `routes/agents.ts`, `routes/activity.ts`, `routes/issues.ts`
  - `server/src/services/issues.ts`
  - `server/src/scripts/backfill-issue-reference-mentions.ts`

### 7. UI 不动
- `ui/src/lib/issue-reference.ts` 仍然在 TS 端
- UI 是冻结契约, 通过 HTTP/WS 与 server 通信, 不需要在 Rust 端 mirror
- 如果以后想做 pure shared lib, 可以提取到 `packages/shared/` 作为 npm 包给 UI 用

---

## V 真实进度更新

| V | R528 前 | R528 后 | 增量 |
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

R528 是**质量层**轮次: 不直接推进任何 V 数字, 但为 routes/costs/agents/activity/issues 4 个 route 的 issue reference 解析 + `issueReferenceService` 集成层提供独立可测试的基础设施。

---

## 教训

1. **`&'static str` 不能引用动态字符串**: 第一次写 `regex_fence_start -> Option<&'static str>` 试图返回 5+ backtick 的 fence 是不行的, 改成 `(char, usize)` 干净利落
2. **regex `[^\s<>()]+` 不包含 `(`**: 测试 `r528_find_matches_does_not_capture_outer_parens` 验证 `(PAP-1)` 只匹配 `PAP-1`, 不是 `(PAP-1)` — 这是上游 Node 行为, 写测试时不要想当然认为会匹配外层括号
3. **spaces count 与 byte length 一致**: `preserve_newlines_as_whitespace` 用 1:1 字符替换, 这样替换前后 byte 长度一致, regex 的 byte offset 才有意义
4. **测试先写, bug 后查**: 失败的 2 个测试 (`find_matches_trims_trailing_paren_with_balance` + `strip_inline_code_preserves_lines`) 都是我对上游行为的理解偏差, 写测试就能暴露

---

## 下一步

### R529 (推荐)
- **pc-secret-binding 集成层**: 把 R527 `pc-secret-redaction` + R526 `pc-log-redaction` helpers 接到 `pc-http` middleware
- 处理 `secret_ref` / `user_secret_ref` / `plain` binding 检测 + 整条 response sanitize pipeline
- 让 `/api/companies/:id/audit-log` 这类 endpoint 验证 secret 不漏出

### R530
- **port `packages/shared/frontmatter.ts` (648 LOC)** — 含完整 YAML parser, 体积大, 适合单独一轮
- 用于 skill catalog, plugin manifest, doc anchor 等多处

### R531
- **port `packages/shared/gitignore-runtime.ts`** — gitignore 解析, 给 pc-acpx workspace sync 用

### R532
- **V8 远程 SSH execution**: `restoreRemoteWorkspace` + `materializeRemoteClaudeConfig`
- **V10 plugin 互操作**: spawn 真实 subprocess 跑 plugin

### R533+ (V11/V12/V13)
- UI 60 client happy 跑
- Playwright 真实 UI 剧本
- 长跑性能 baseline
