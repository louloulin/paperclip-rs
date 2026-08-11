# R525 — GitHub external object 纯解析层 port

**日期**: 2026-08-11
**轮次**: R525
**目标**: 接续 R523, port `github-external-object-provider.ts` 纯解析部分
**模块**: 新 crate `crates/pc-github-external-objects/`

---

## 改动

### 范围划分 (关键架构决策)

Node 上游 `github-external-object-provider.ts` (445 LOC) 含两类代码:
1. **纯解析逻辑** (URL/externalId 解析, identity 类型, retry-after, status mapping) — 可独立 crate
2. **HTTP/DB 集成** (reqwest fetch, drizzle ORM 持久化, plugin worker manager, live-events) — 需要大量胶水代码

R525 只 port (1); (2) 留给 R526 集成层。理由:
- 高内聚: 纯解析无 IO 副作用, 单测快 (< 1ms), 不需要 mock
- 低耦合: 不依赖 reqwest / sqlx / live-events bus, 只依赖 `serde` + `url` + `pc-github-fetch` (host 判断)
- 可单独验证: 27 测试覆盖所有 parsing 边界, 无 HTTP server 启动开销

### 4 个模块 (~600 LOC)

**`crates/pc-github-external-objects/src/identity.rs`** (~250 LOC):
```rust
pub enum PathKind { Pull, Issues }
pub enum ObjectType { PullRequest, Issue }
pub struct GitHubObjectIdentity {
    pub host: String, pub owner: String, pub repo: String,
    pub number: u64, pub path_kind: PathKind, pub object_type: ObjectType,
}
pub fn parse_github_canonical_url(scheme: &str, host: &str, path: &str) -> Result<GitHubObjectIdentity, ParseError>;
pub fn parse_github_object(external_id: &str, sanitized_canonical_url: Option<&str>) -> Result<GitHubObjectIdentity, ParseError>;
pub fn external_id_for(identity: &GitHubObjectIdentity) -> String;
pub fn display_title_for(identity: &GitHubObjectIdentity) -> String;
pub fn display_key_for(identity: &GitHubObjectIdentity) -> &'static str;
```

**`crates/pc-github-external-objects/src/retry.rs`** (~180 LOC):
```rust
pub struct RetryAfterResponse { pub retry_after: Option<String>, pub x_ratelimit_reset: Option<String> }
pub fn retry_after_seconds(response: &RetryAfterResponse) -> u64;
pub fn failure_from_github_response(
    status: u16,
    rate_limit_remaining: Option<&str>,
    retry_after: &RetryAfterResponse,
) -> Option<ResolveFailure>;
pub struct ResolveFailure { liveness, error_code, error_message, retry_after_seconds }
```

**`crates/pc-github-external-objects/src/status.rs`** (~30 LOC):
```rust
pub enum LivenessState { Active, AuthRequired, Unreachable }
pub enum ErrorCode { GithubAuthRequired, GithubForbidden, GithubRateLimited, GithubUnreachable }
```

**`crates/pc-github-external-objects/src/lib.rs`** (~90 LOC): `ParseError` (7 variants) + re-exports

---

## 设计改进 vs Node 上游

| Node | Rust | 理由 |
|---|---|---|
| `parseGitHubCanonicalUrl(canonical: ExternalObjectCanonicalUrl)` 接受 nested DTO | `parse_github_canonical_url(scheme, host, path)` 接受 3 个 `&str` | 不耦合 DTO crate; 集成层负责把 DTO 拆成 3 参数 |
| `parseGitHubObject(object: { externalId, sanitizedCanonicalUrl })` | `parse_github_object(external_id, sanitized_canonical_url)` 两个 `&str` | 同上 |
| `retryAfterSeconds(response: Response)` 调 `response.headers.get` | `retry_after_seconds(response: &RetryAfterResponse)` 自定义 struct | 解耦 reqwest; 单测不需 HTTP server |
| `failureFromGitHubResponse(response: Response)` 直接读 status + headers | `failure_from_github_response(status: u16, rate_limit_remaining: Option<&str>, retry: &RetryAfterResponse)` 3 个标量 | 同上; 集成层负责从 reqwest::Response 提取 |
| stringly-typed error messages (`"github_auth_required"`) | typed `ErrorCode` enum + `LivenessState` enum | 编译期保证, UI 反序列化安全 |
| `throw unprocessable(...)` for parse errors | `ParseError` enum with 7 variants, callers decide handling | 强类型; 集成层映射到 HTTP |

---

## 测试 (27 个新增, 全过)

**`identity::tests` (15)**:
- `r525_parse_canonical_pull_request` — `/rust-lang/cargo/pull/1234`
- `r525_parse_canonical_issue` — `/rust-lang/cargo/issues/42`
- `r525_parse_canonical_ghe_host_normalises_www` — `Www.GitHub.Com` → `github.com`
- `r525_parse_canonical_ghe_host_preserved` — `ghe.acme.io` 不被规范化
- `r525_parse_canonical_rejects_http` — `http://` → `NotHttps`
- `r525_parse_canonical_rejects_wrong_arity` — `/o/r/pull` (3 segments) → `WrongPathArity(3)`
- `r525_parse_canonical_rejects_invalid_kind` — `/commit/abc` → `WrongKind`
- `r525_parse_canonical_rejects_zero_number` — `/pull/0` → `InvalidNumber`
- `r525_parse_canonical_rejects_invalid_owner_chars` — `/bad@owner/...` → `BadExternalId`
- `r525_parse_external_id_dotcom_default` — `OWNER/REPO#pull/5`, host 默认 `github.com`
- `r525_parse_external_id_with_canonical_url_ghe` — 用 sanitized URL 决定 host
- `r525_parse_external_id_rejects_missing_hash` — 无 `#` → `BadExternalId`
- `r525_parse_external_id_rejects_invalid_canonical_url` — `"not a url"` → `BadCanonicalUrl`
- `r525_external_id_for_lowercases_owner_repo` — `Rust-Lang/Cargo` → `rust-lang/cargo`
- `r525_display_title_includes_hash_separator` — `o/r#7`
- `r525_display_key_pr_vs_issue` — `"GitHub Pull Request"` vs `"GitHub Issue"`

**`retry::tests` (10)**:
- `r525_retry_after_uses_retry_after_header_first` — Retry-After: 30 优先于 X-RateLimit-Reset
- `r525_retry_after_falls_back_to_ratelimit_reset` — Retry-After 缺失时用 reset
- `r525_retry_after_fallback_300_when_both_headers_missing` — 缺省 300 秒 (Node upstream 行为)
- `r525_retry_after_rejects_non_numeric_retry_after` — `"not a number"` → fallback
- `r525_failure_401_maps_to_auth_required`
- `r525_failure_403_with_rate_limit_zero_maps_to_rate_limited`
- `r525_failure_403_without_rate_limit_maps_to_forbidden`
- `r525_failure_429_maps_to_rate_limited`
- `r525_failure_500_maps_to_unreachable`
- `r525_failure_404_returns_none_caller_handles` — 4xx 不映射, caller fallback
- `r525_failure_200_returns_none` — 2xx 不映射, 成功路径

---

## 验证

```
cargo test -p pc-github-external-objects --lib    27 passed (15 identity + 10 retry + 2 默认)
cargo check --workspace                            0 errors (170 pre-existing pc-http warnings)
```

整体单测 ≈ **2047 passing** (+27 R525)

---

## 设计要点

### 1. 强类型胜过 stringly-typed

Node 上游用 string `"github_auth_required"` / `"auth_required"` / `"unreachable"` 等散落字符串。Rust 端用 `enum ErrorCode` + `enum LivenessState`, 编译期保证拼写正确, serde 自动 derive `rename_all = "snake_case"` 与上游 string 对齐 (集成层可直接序列化给 UI)。

### 2. 接受标量胜过 DTO value object

`parse_github_canonical_url(scheme, host, path)` 接受 3 个 `&str` 而非 Node 的 `ExternalObjectCanonicalUrl`。理由:
- 不需要把 DTO crate 加为 dep (避免循环依赖风险)
- 测试不需要构造完整 DTO
- 集成层负责 1 行代码把 DTO 拆成 3 参数

### 3. Helper 解耦 HTTP client

`retry_after_seconds` 和 `failure_from_github_response` 接受自定义 `RetryAfterResponse` struct 而非 `reqwest::Response`。理由:
- 单测 0ms (无需 HTTP server)
- 集成层负责 1 行代码从 reqwest 提取 headers (`RetryAfterResponse::new(resp.header("retry-after"), resp.header("x-ratelimit-reset"))`)
- 未来换 HTTP 库 (ureq / hyper) 不影响这些 helpers

### 4. 集成层留给 R526

**不** port 到 R525 的部分:
- HTTP fetch (用 R523 `pc_github_fetch::gh_fetch_with` — 已就绪)
- DB 持久化 (需要 DTO + sqlx schema — 工作量大)
- Plugin worker manager 集成 (跨 crate 复杂)
- Live-events publish (需要 `pc-realtime` 集成)
- Snapshot 构造 (依赖 DTO `ExternalObjectResolverSnapshot`)

理由: 集成层 1 个函数可能 100-200 LOC, 涉及多个 crate 依赖。R525 先把"可独立验证"的纯逻辑落地, R526 在它上面叠加集成。

---

## 与 R523 的协同

R523 提供 `pc-github-fetch` (URL builders + ghFetch wrapper)。
R525 提供 `pc-github-external-objects` (identity 解析 + retry logic)。

R526 集成层会:
```rust
use pc_github_fetch::gh_fetch_with;
use pc_github_external_objects::{parse_github_canonical_url, failure_from_github_response, RetryAfterResponse};

async fn refresh_object(client: &Client, canonical: (String, String, String)) -> ResolveResult {
    let identity = parse_github_canonical_url(&canonical.0, &canonical.1, &canonical.2)?;
    let url = format!("https://{}/{}/{}/{}/{}", identity.host, identity.owner, identity.repo,
                     identity.path_kind.as_str(), identity.number);
    let resp = gh_fetch_with(client, client.get(&url).bearer_auth(&token)).await?;
    if let Some(failure) = failure_from_github_response(
        resp.status().as_u16(),
        resp.header("x-ratelimit-remaining"),
        &RetryAfterResponse::new(resp.header("retry-after"), resp.header("x-ratelimit-reset")),
    ) { return Err(failure); }
    // ... parse body, build snapshot ...
}
```

---

## 下一步

- **R526** = GitHub external object provider **集成层** (HTTP fetch + DB 持久化 + snapshot 构造), ~300 LOC, 用 R523+R525 现成 helpers
- **R527** = V4 UI 60 client 接入 generated types (解锁 V11/V12)
- **R528** = port `tool-oauth-legacy-backfill.ts` (MCP tool OAuth backfill, 真存在上游)
