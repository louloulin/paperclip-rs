# R523 — V5 修正 + 新增 pc-github-fetch crate

**日期**: 2026-08-11
**轮次**: R523
**目标**: 修正 V5 OAuth login 误判 + port Node `github-fetch.ts`
**模块**: 新 crate `crates/pc-github-fetch/`

---

## 关键发现：V5 OAuth login 缺口不存在

按 ARCHITECTURE.md 之前的计划，V5 80% → 90% 应该是「OAuth 2.0 client (Google/GitHub provider)」。R523 在动手前先做了 **gap 验证**：

```bash
$ grep -rn "oauth\|OAuth" /Users/louloulin/Documents/lumosaipaperclip/paperclip/server/src/auth/
# (空 — auth/ 目录下没有任何 oauth 相关代码)

$ find server/src -name "*oauth*" -o -name "*google*login*" -o -name "*github*login*"
# (空 — Node 上游只有 tool-oauth-legacy-backfill.ts / github-external-object-provider.ts)
```

**结论**: Node 上游**没有** Google/GitHub login OAuth（"Sign in with Google" 那种）。Rust 端的 `pc-auth::oauth_state` (R565) 只是 PKCE 算法骨架，没人会调用它。

V5 的 85% 已经反映了现实：
- session 创建 / 刷新 / reuse detection (R512) ✅
- CSRF (R515) ✅
- API key `pk_` 前缀 (R514) ✅
- Argon2id password + email verification (R569) ✅

OAuth login 是我们臆想的缺口，不是真实缺口。

**R523 修正**: 把"剩 OAuth"从 V5 缺口列表移除。

---

## 实际 port: Node `github-fetch.ts` → Rust `pc-github-fetch`

虽然小但**真有用**: `github-fetch.ts` 被 `github-external-object-provider.ts` 依赖（后者 port 后会用前者），是 paperclip GitHub 集成的基础设施层。

### 上游 Node 实现 (30 LOC)
```typescript
// server/src/services/github-fetch.ts
export function isGitHubDotCom(hostname: string) { ... }
export function gitHubApiBase(hostname: string) { ... }
export function resolveRawGitHubUrl(hostname, owner, repo, ref, filePath) { ... }
export async function ghFetch(url, init?) { ... }
```

### Rust port (3 模块, 280 LOC)

**`crates/pc-github-fetch/src/lib.rs`** (~40 LOC):
```rust
pub mod fetch;
pub mod urls;
pub use fetch::{gh_fetch, gh_fetch_with};
pub use urls::{git_hub_api_base, is_git_hub_dot_com, resolve_raw_git_hub_url};

#[derive(Debug, Error)]
pub enum GitHubFetchError {
    #[error("could not connect to {host} — ...")]
    Connection { host: String, source: reqwest::Error },
    #[error("HTTP {status} from GitHub: {body}")]
    Http { status: u16, body: String },
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
}
```

**`crates/pc-github-fetch/src/urls.rs`** (~80 LOC): 纯 URL builder，无 IO，3 个 `pub fn` + 8 测试。

**`crates/pc-github-fetch/src/fetch.rs`** (~140 LOC): async fetch wrapper，2 个 `pub fn` + 4 测试（含真 mock TcpListener）。

---

## 设计改进 (vs Node 上游)

| Node | Rust | 理由 |
|---|---|---|
| `ghFetch(url, init?)` 一函数 | `gh_fetch_with(&Client, RequestBuilder)` + `gh_fetch(url, token)` 双函数 | 生产代码共享 reqwest Client 连接池 (Node `fetch` 全局复用) |
| `throw unprocessable("Could not connect...")` | `GitHubFetchError::Connection { host, source: reqwest::Error }` | 强类型，携带原始 error，调用方决定 HTTP 映射 |
| `case-insensitive hostname` 直接 `lower ===` | `is_git_hub_dot_com` 调 `to_lowercase()` 后比 | 显式、可测 |
| URL builder 函数散落 | `urls` 子模块 + `pub use` 重导出 | 高内聚，未来加 GHE-specific 路径不污染主 API |

---

## 测试 (13 个新增, 全过)

**`pc-github-fetch::urls` (8)**:
- `r523_is_git_hub_dot_com_recognises_dotcom_and_www` — `github.com`, `www.github.com`, 大小写不敏感
- `r523_is_git_hub_dot_com_rejects_enterprise` — `api.github.com`, `ghe.acme.io`, 空串都 false
- `r523_git_hub_api_base_dotcom_returns_api_github_com`
- `r523_git_hub_api_base_enterprise_uses_host_api_v3`
- `r523_resolve_raw_dotcom_url`
- `r523_resolve_raw_strips_leading_slashes_from_path` — 1 个和多个 leading slashes
- `r523_resolve_raw_enterprise_url`
- `r523_resolve_raw_with_ref_containing_slash` — ref="feature/foo" 保持不变

**`pc-github-fetch::fetch` (4)** — 全部用真 mock TcpListener:
- `r523_gh_fetch_returns_response_on_success` — 200 OK + JSON body
- `r523_gh_fetch_passes_bearer_token` — bearer auth header 正确发出
- `r523_gh_fetch_returns_connection_error_on_unreachable_host` — 127.0.0.1:1 → Connection error with host
- `r523_gh_fetch_with_returns_invalid_url_for_malformed_builder` — InvalidUrl / Transport

**`pc-github-fetch::tests` (1)**: `re_exports_are_consistent` — 函数指针相等

---

## 验证

```
cargo test -p pc-github-fetch --lib    13 passed
cargo check --workspace                 0 errors (170 pre-existing pc-http warnings)
```

整体单测 ≈ **2020 passing** (+13 R523)

---

## 设计要点

### 1. 高内聚

整个 crate 只做一件事: 给 GitHub / GHE 提供 fetch + URL 工具。3 个模块职责清晰:
- `urls` 纯函数 (无 IO, 测试快)
- `fetch` 异步包装 (唯一 IO)
- `lib.rs` 错误类型 + re-export

### 2. 低耦合

只依赖:
- `reqwest` (工作区已有)
- `url` (工作区已有)
- `thiserror` (工作区已有)
- `tokio` (dev-dep, 仅测试用)

不引入新外部依赖。

### 3. Caller-Supplied Client

`gh_fetch_with(&Client, RequestBuilder)` 把 client 所有权留给调用方。生产代码可以:
```rust
let client = reqwest::Client::builder().pool_idle_timeout(Duration::from_secs(90)).build()?;
for url in urls {
    let resp = pc_github_fetch::gh_fetch_with(&client, client.get(&url).bearer_auth(&token)).await?;
}
```

避免每次调用都新建 client (Node `fetch` 是全局的, Rust 没有全局所以要主动管理)。

### 4. 真实 Mock Server

测试用 `tokio::net::TcpListener` 起真 HTTP server, 验证:
- 成功路径 (200 + body)
- bearer auth 发出
- 连接失败 → typed `GitHubFetchError::Connection { host, source }`

而不是 mock reqwest (那只是测试 reqwest 自己)。

---

## 教训

1. **先验证再动手**: R523 起始计划是 OAuth 2.0 login, 但 grep 上游发现根本不存在 — 修正方向, 避免做"无意义工作"
2. **Caller-supplied client**: Node 上游一个 `ghFetch(url, init)` 就够, Rust 需要双函数让 caller 管理连接池
3. **测试用真 server**: mock reqwest 等于测 reqwest 自己; 用 tokio TcpListener 测真 HTTP 路径

---

## 下一步

- **R524** = V4 UI types integration (60 client 接入 generated types)
- **R525** = port `github-external-object-provider.ts` 使用新 `pc-github-fetch` crate
- **R526** = port `tool-oauth-legacy-backfill.ts` (MCP tool OAuth backfill, 真存在上游)
