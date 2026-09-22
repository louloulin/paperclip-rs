# 29 — W1 Google 登录（`POST /auth/google`，M1 最后一条缺口）

> 切片：**LUM-1399 / W1**（`docs/plan1.md` §5 W1「完成 M1 收尾：… `POST /auth/google` …」，验收口径「M1 面 34/34 覆盖」）
> 分支：`feat/multica-rs-w1-google` → `feat/multica-rs-initial`（base `00a2a12`）
> 交付物：`crates/mc-http/src/routes/auth.rs`（`google_login` + 8 个测试）、
> `crates/mc-http/src/state.rs`（`GoogleOAuthConfig` + 4 个测试）、`crates/mc-http/Cargo.toml`（`reqwest` 继承 workspace 依赖）、
> `Cargo.lock`（只加 1 行）、本文件
> 结论：**M1 面 34/34 覆盖**（`route_parity.py`：`M1 known_gap = 0`、`OK: every upstream route is either implemented or owned`）；
> 门禁 ①–⑦ 全绿（见 §8）；**只新增 1 处依赖**（`reqwest` 早已在 `Cargo.lock` 里，因 `apps/mc-cli` 在用）

---

## 1. 结论速览

| # | 事项 | 结论 | 证据 |
| --- | --- | --- | --- |
| 1 | `POST /auth/google` | 已实现，11 步流程与上游逐条对齐（状态码/`code` 字符串一致） | §4 对照表；`crates/mc-http/src/routes/auth.rs:710` |
| 2 | M1 面覆盖 | **34/34**（本切片前 33/34） | `python3 scripts/route_parity.py` → `M1 known_gap = 0`，`M1 implemented = 34` |
| 3 | 出站配置 | 两个 base URL 走 `MC_GOOGLE_TOKEN_URL` / `MC_GOOGLE_USERINFO_URL`（默认 = 上游真实地址），凭据沿用上游 `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` / `GOOGLE_REDIRECT_URI` | §5 |
| 4 | 依赖 | **没有引入新 crate**（`jsonwebtoken` / `openidconnect` 都没有）；`mc-http` 新增 `reqwest = { workspace = true }`，`Cargo.lock` 只加 mc-http 的 1 行依赖边 | `git diff Cargo.lock` |
| 5 | 错误信封 | 保留上游 `code` 字符串，但用本仓 M1 的嵌套信封 `{"error":{"code","message"}}` | §6.1 |
| 6 | `token` 字段 | 本仓 M1 没有 JWT 层 → 返回**不透明 session id**（同时写 `multica_session` cookie + `x-multica-csrf` 头） | §6.2 |
| 7 | 三条 403 code | 本仓**不可达**（无 `ALLOW_SIGNUP` / 邮箱白名单 / 禁用邮箱列表配置面）；常量按上游字符串保留为 `pub const` | §6.4 |
| 8 | 测试 | 10 个不依赖 DB 的用例 + 2 个 DB 用例（`--ignored`，gate ⑥ 跑） | §7 |

---

## 2. 路由与调用链

| Method | Path | Handler | 中间件 | 上游 |
| --- | --- | --- | --- | --- |
| `POST` | `/auth/google`（**无** `/api` 前缀） | `routes::auth::google_login` | 无（挂在 `mount_slice_auth` 的 `auth::router()` 上） | `handler/auth.go:546 Handler.GoogleLogin`，注册于 `router.go:1474` |

- 实现：`crates/mc-http/src/routes/auth.rs`（handler 在 `google_login`，紧邻 M1-B 的 `send_code` / `verify_code` / `logout`）；
- 接线：`crates/mc-http/src/routes/mount.rs::mount_slice_auth` 已经 `.merge(auth::router())`，本切片只在 `auth::router()` 里
  多注册一条 `.route("/auth/google", post(google_login))`——没有新增任何 mount 分支；
- 上游这条路由被 `authRL = middleware.RateLimit(rdb, RATE_LIMIT_AUTH=5, time.Minute, trustedProxies)` 包着
  （`router.go:1460-1490`，Redis 支持的 per-IP 限流），本仓 M1 无此设施 → 见 §6.7。

---

## 3. 线上协议

请求（`Content-Type: application/json`）：

```json
{ "code": "<google authorization code>", "redirect_uri": "https://app.example/auth/callback" }
```

- `code` 必填（**不 trim**，上游原样为空即报错）；字段缺失等同空串（上游 `Code string` 的零值）；
- `redirect_uri` 可选；为空时回落到 `GOOGLE_REDIRECT_URI`，两者都空则发空串（上游同）。

成功（200）：

```json
{ "token": "<opaque session id>", "user": { "id": "…", "name": "…", "email": "…", "created_at": "…" } }
```

同时写 `Set-Cookie: multica_session=…`（`HttpOnly` 等属性由 `session_response` 统一给出）与 `x-multica-csrf` 响应头。

失败：`{"error":{"code":"<code>","message":"<message>"}}`。**全部**错误组合（本切片完整覆盖）：

| 触发条件 | HTTP | `code` | `message`（与上游逐字一致） |
| --- | --- | --- | --- |
| body 不是合法 JSON | 400 | `validation_error` | `invalid request body` |
| `code` 为空/缺失 | 400 | `validation_error` | `code is required` |
| 未配置 `GOOGLE_CLIENT_ID`/`GOOGLE_CLIENT_SECRET` | **403** | `google_login_not_configured` | `Google login is not configured` |
| 换 token 请求发不出去（连接/DNS/TLS） | 502 | `upstream_error` | `failed to exchange code with Google` |
| 换 token 响应体读不出来 | 502 | `upstream_error` | `failed to read Google token response` |
| 非 200 且 body 是 `{"error":"invalid_grant"}` | 400 | `oauth_code_invalid` | `failed to exchange code with Google` |
| 非 200（其它一切情形） | 502 | `upstream_error` | `failed to exchange code with Google` |
| 200 但 body 解析失败 | 502 | `upstream_error` | `failed to parse Google token response` |
| 200 但 `access_token` 缺失/纯空白 | 502 | `upstream_error` | `invalid Google token response` |
| userinfo 请求构造失败 | **500** | `internal_error` | `internal error` |
| userinfo 传输失败 | 502 | `upstream_error` | `failed to fetch user info from Google` |
| userinfo 非 200 | 502 | `upstream_error` | `failed to fetch user info from Google` |
| userinfo body 读失败 / 解析失败 | 502 | `upstream_error` | `failed to parse Google user info` |
| userinfo body 是字面量 `null` | 502 | `upstream_error` | `invalid Google user info` |
| email 为空（`trim` + 小写后） | 400 | `google_account_no_email` | `Google did not provide an email address for this sign-in` |
| 用户落库失败 | 500 | `database_error` | `failed to create user` |
| 签发 session 失败 | 500 | `internal_error` | `failed to generate token` |

`google_error()` 直接复用 `mc_errors::{ErrorBody, ErrorResponse}`，输出与全仓 `ApiError` 逐字节同形——因此没有去改
`mc-errors` 里 `Error::code()` 的固定映射（那是全仓共享面，改它等于把 `oauth_code_invalid` 这类上游专有 code 塞进公共枚举）。

---

## 4. 与上游 11 步的逐条对应（行号 = `handler/auth.go`）

| # | 上游 | 本仓 | 说明 |
| --- | --- | --- | --- |
| 1 | `:547-551` `json.NewDecoder(...).Decode(&req)` 失败 → 400 `invalid request body` | `serde_json::from_slice::<GoogleLoginRequest>` 失败 → 400 | 用 `Bytes` 抽取器而不是 `Json<T>`：axum 的 `Json` rejection 对「字段类型错误」返回 422，而上游一律 400（§6.8） |
| 2 | `:553-556` `req.Code == ""` → 400 `code is required` | 同 | `#[serde(default)]` 让缺失字段变空串；不 trim |
| 3 | `:558-563` 缺 `GOOGLE_CLIENT_ID`/`GOOGLE_CLIENT_SECRET` → `writeFeatureDisabled`（**403** `google_login_not_configured`） | 同 | 上游注释明写「用 403 而不是 503，避免客户端把它当可重试的服务端故障并触发告警」 |
| 4 | `:564-582` `redirectURI` 回落 + `PostForm` 表单（`code`/`client_id`/`client_secret`/`redirect_uri`/`grant_type=authorization_code`） | 同，URL 换成 `MC_GOOGLE_TOKEN_URL` | 传输失败 → 502 |
| 5 | `:585-605` 读 body → 非 200 分支：**只有** `400 && error=="invalid_grant"` 是 400 `oauth_code_invalid`，其余 502 | 同 | 上游注释：`Only a valid invalid_grant response identifies a rejected authorization code` |
| 6 | `:608-615` 解析失败 → 502；`AccessToken` 空白 → 502 `invalid Google token response` | 同 | 本仓额外 `.trim()` 后判空（上游 `strings.TrimSpace` 同义） |
| 7 | `:619-653` `NewRequestWithContext` 失败 → **500** `internal error`；`Do` 失败/非 200 → 502；解析失败 → 502；`null` → 502 `invalid Google user info` | 同，URL 换成 `MC_GOOGLE_USERINFO_URL` | 上游用 `var gUser *googleUserInfo` 区分「解析失败」与「字面 `null`」；本仓用 `Option<GoogleUserInfo>` 保留这条区分。构造失败与传输失败用 `RequestBuilder::build()` 拆开（§6.9） |
| 8 | `:655-662` `strings.ToLower(strings.TrimSpace(email))` 为空 → 400 `google_account_no_email` | 同 | |
| 9 | `:664-674` `IsTemporarilyDisabledUserEmail` → 403 `account_disabled`；`findOrCreateUser`（查 email → 建号，name = `@` 前缀）→ `writeGoogleLoginActionableError` 映射 403，否则 500 | 查 `UserRepo::get_by_email` → 缺失则 `create(NewUser{name: email_local_part, avatar_url: None})`，失败 500 | 本仓无禁用名单/注册白名单 → 403 三条不可达（§6.4）。建号失败统一 500（本仓没有 `ErrSignupProhibited`/`ErrEmailNotAllowed` 这些错误类型） |
| 10 | `:676-705` `needs_name = gUser.Name != "" && user.Name == strings.Split(email,"@")[0]`；`needs_avatar = gUser.Picture != "" && !user.AvatarUrl.Valid`；更新失败**静默保留旧 user** | 同 | `UserRepo::update` 的 `UserUpdate` 没有 `avatar_url` 字段，改用按 email 幂等的 `upsert_by_email`，传入「算好的最终值」（§6.3） |
| 11 | `:707-728` `issueJWT` → 失败 500 `failed to generate token`；`SetAuthCookies` 失败只 warn；`CFSigner != nil` 时再签 72h CF cookie；成功日志 `user logged in via google`；返回 `LoginResponse{token, user}` | `Session::new(user.id, ttl)` + `state.auth.store().put(...)` → 失败 500；随后走 M1-B 的 `session_response()`（cookie + CSRF 头） | 无 CF 签名 cookie（§6.5）、token 不是 JWT（§6.2） |

`auth.go:43-47` 的 code 常量在本仓逐字保留：

| 上游常量 | 字符串 | 本仓 | 可达性 |
| --- | --- | --- | --- |
| `googleLoginCodeAccountDisabled` | `account_disabled` | `GOOGLE_CODE_ACCOUNT_DISABLED` | 不可达（§6.4） |
| `googleLoginCodeSignupProhibited` | `signup_prohibited` | `GOOGLE_CODE_SIGNUP_PROHIBITED` | 不可达（§6.4） |
| `googleLoginCodeEmailNotAllowed` | `email_not_allowed` | `GOOGLE_CODE_EMAIL_NOT_ALLOWED` | 不可达（§6.4） |
| `googleLoginCodeAccountWithoutEmail` | `google_account_no_email` | `GOOGLE_CODE_ACCOUNT_WITHOUT_EMAIL` | ✅ 有测试 |
| `googleLoginCodeInvalidOAuthCode` | `oauth_code_invalid` | `GOOGLE_CODE_INVALID_OAUTH_CODE` | ✅ 有测试 |

（外加 `writeFeatureDisabled` 用的 `google_login_not_configured` = `GOOGLE_CODE_NOT_CONFIGURED`，同样有测试。）

---

## 5. 出站配置面

`crates/mc-http/src/state.rs::GoogleOAuthConfig`（`AppState.google_oauth`）：

| 环境变量 | 默认 | 说明 |
| --- | --- | --- |
| `GOOGLE_CLIENT_ID` | 无（未配置 → 403 `google_login_not_configured`） | 上游同名 |
| `GOOGLE_CLIENT_SECRET` | 无 | 上游同名 |
| `GOOGLE_REDIRECT_URI` | 无 | 上游同名；请求体里的 `redirect_uri` 优先 |
| `MC_GOOGLE_TOKEN_URL` | `https://oauth2.googleapis.com/token` | 本切片新增，**只为可测性**：默认值就是上游硬编码的那个 URL |
| `MC_GOOGLE_USERINFO_URL` | `https://www.googleapis.com/oauth2/v2/userinfo` | 同上 |

三点设计取舍：

1. **空串视为未设置**（`from_env_with` 里 trim 后判空）——`GOOGLE_CLIENT_ID=` 这种 `.env` 写法不会把「未配置」变成「配置了一个空 client」；
2. **`http: Option<reqwest::Client>` 是注入点**：上游同样有 `h.googleOAuthHTTPClient`（nil → `http.DefaultClient`，`auth.go:539-544`），
   本仓把它做成配置字段（默认给一个 15s 超时的 client），测试里换成指向本机 stub 的 client；
3. **不放进 `ConfigSnapshot`**：`ConfigSnapshot` derive 了 `Serialize`（`client_secret` 进快照有泄漏风险），
   而且它有 12 处字面量构造点（`AppState` 只有 2 处）。放 `AppState` 上还让 `apps/mc-server/src/main.rs` **零改动**——
   多条切片正在同时改那个文件，少一次改动就少一次冲突。

---

## 6. 与上游的登记偏离（全部）

### 6.1 错误信封是嵌套的

上游：`{"error":"msg"}` / `{"error":"msg","code":"c"}`；本仓：`{"error":{"code","message"}}`。
这是 M1 已经落地的全仓约定（`docs/17-M1-CONTRACT-GAPS.md`、`routes/inbox.rs` 顶部注释同），本切片**不改约定**，
只保证 `code` 字符串与上游逐字一致（对齐用的 `google_error()` 见 §3）。

### 6.2 `token` 是 session id，不是 JWT

上游 `issueJWT` 发 HS256 JWT 并塞进 `SetAuthCookies`。本仓 M1 没有 JWT 层（`jsonwebtoken` 也**不允许**引入），
所以复用 M1-B 的会话路径：`mc_auth::Session::new` + `SessionStoreContainer` + `multica_session` cookie + `x-multica-csrf`。
对外形状与 `POST /auth/verify-code` 完全一致，前端可以同构处理。→ **承接：M9 引入 JWT 面时统一。**

### 6.3 回填 profile 用 `upsert_by_email`

上游 `UpdateUser` 可以同时写 `name` 与 `avatar_url`；本仓 `UserUpdate`（M1-A 定的签名）没有 avatar 字段，
且本切片**不许改 `crates/mc-repos/**`**。改用按 email 幂等的 `UserRepo::upsert_by_email(NewUser{..})`，
传「算好的最终值」——语义等价（同一条 user 行、同样的最终列值），失败时与上游一样只 warn 并保留旧 user。

### 6.4 三条 403 不可达

| code | 上游条件 | 本仓为什么不可达 |
| --- | --- | --- |
| `account_disabled` | `auth.IsTemporarilyDisabledUserEmail(email)` / `IsTemporarilyDisabledUser`（硬编码名单） | 本仓没有这份名单 |
| `signup_prohibited` | `checkSignupAllowed` 且 `ALLOW_SIGNUP=false` | `mc-config` 没有 `ALLOW_SIGNUP` |
| `email_not_allowed` | `ALLOWED_EMAILS` / `ALLOWED_EMAIL_DOMAINS` 白名单 | `mc-config` 没有白名单配置 |

常量按上游字符串保留（`pub const`），**M9 接上配置面后再启用分支**。这也是本切片唯一「登记但不实现」的部分——
`docs/17-M1-CONTRACT-GAPS.md` 的缺口表可以由 M9 接手。

### 6.5 不签发 CF region cookie

上游 `h.CFSigner != nil` 时额外签一组 72h 的 Cloudflare 区域偏好 cookie；本仓没有 CF 集成。→ **承接：M10（部署面）。**

### 6.6 不做 signup analytics

上游建号成功会 `analytics.Signup(...)`（含 `signup_source` cookie 解析 / PostHog / 指标）。本仓 M1 没有 analytics 管道。

### 6.7 这条路由没有 per-IP 限流

上游用 `middleware.RateLimit(rdb, RATE_LIMIT_AUTH=5, time.Minute, ...)` 包住整个 `/auth/*` 组（含 `/auth/google`）。
本仓 M1 既没有 Redis，也没有 per-IP 限流中间件（`send_code` 走的是「每邮箱每分钟 5 次」的配置项）。
→ **承接：M9/M10 统一限流面。**

### 6.8 body 用 `Bytes` 而非 `Json<T>`

`Json<T>` 的 rejection 对「`{"code":123}` 这类类型错误」是 422；上游 `json.Decode` 一律 400 `invalid request body`。
为了状态码同构，handler 自己解 `Bytes`（代价：错误信息里的字段名不如 `Json` 详细，但对外契约一致）。
另外 axum 默认 body 上限（2MB）仍然生效——上游 `json.Decoder` 无上限，这是一条**有意保留**的防御性偏离。

### 6.9 userinfo 请求构造失败 = 500，其它 = 502

上游 `http.NewRequestWithContext` 失败 500、`Do` 失败 502。reqwest 的 `RequestBuilder` 在 `send()` 时才报错，
所以本仓在 `.send()` 前显式 `.build()` 一次：`build()` 失败 → 500，`execute()` 失败 → 502。**这是刻意保留上游的 500/502 区分。**

### 6.10 15s 超时

上游 `http.DefaultClient` 没有超时；本仓默认 client 设 `HTTP_TIMEOUT_SECS = 15`，避免 Google 抖动时挂住 axum worker。
可通过注入自定义 `reqwest::Client` 覆盖。

### 6.11 email 本地部分的边界

上游 `name = email[:at]`（仅当 `at > 0`）→ `"@x.com"` 的默认名是 `"@x.com"`；本仓复用的 `email_local_part()`（M1-B 既有）
在 `"@x.com"` 上返回 `""`。Google 不会给出空本地部分的邮箱，这条边界**登记不修**（改它会影响 `verify_code` 的既有行为）。

---

## 7. 测试

`crates/mc-http/src/routes/auth.rs::tests`（10 个）+ `crates/mc-http/src/state.rs::tests`（4 个）。

Google stub 是**本机临时 axum listener**（`TcpListener::bind("127.0.0.1:0")`），不引入 `wiremock` 之类的新依赖，
也不留常驻服务；stub 行为由**请求里的 `code`** 决定（`invalid_grant` → 400、`provider_error` → 500、其他 → 正常 200），
所以一个 stub 服务跑完全部用例。base URL 是构 state 时**注入**的，不读进程 env → 用例之间零串扰。

| 测试 | 覆盖 | 需要 DB |
| --- | --- | --- |
| `state::google_config_defaults_to_upstream_endpoints` | 默认两个 URL = 上游真实地址；未配置凭据 | 否 |
| `state::google_config_reads_and_trims_env` | 5 个 env 读取 + trim | 否 |
| `state::google_config_ignores_blank_env` | 空串/纯空白视为未设置 | 否 |
| `state::google_redirect_uri_prefers_request` | 请求 `redirect_uri` 优先于配置 | 否 |
| `google_login_unconfigured_returns_feature_disabled` | 第 3 步：403 `google_login_not_configured` | 否 |
| `google_login_requires_code_and_parseable_body` | 第 1/2 步：畸形 JSON、缺 `code`、空 `code` | 否 |
| `google_login_rejected_code_returns_oauth_code_invalid` | 第 5 步：400 `oauth_code_invalid` | 否 |
| `google_login_provider_error_returns_502` | 第 5 步反面：上游 500 → 502 且**没有** `oauth_code_invalid` | 否 |
| `google_login_token_endpoint_unreachable_returns_502` | 第 4 步：传输失败 → 502 | 否 |
| `google_login_without_email_returns_google_account_no_email` | 第 8 步：400 `google_account_no_email` | 否 |
| `google_login_creates_user_and_returns_token` | 第 9-11 步 happy path：建号（name 由 profile 覆盖、avatar 落库、email trim+小写）、session 落 store、cookie + CSRF 头、`{"token","user"}` | **是**（`#[ignore]`） |
| `google_login_keeps_named_user_and_existing_avatar` | 第 10 步边界：用户改过 name / 已有 avatar 时**不**回填 | **是**（`#[ignore]`） |

DB 用例读 `MULTICA_TEST_DATABASE_URL`（gate ⑥ 的口径；本地手跑也接受 `DATABASE_URL`），两个用例都用随机 email 并在结尾删行。

---

## 8. 门禁与复现

```bash
export PATH="$HOME/.cargo/bin:$PATH"        # 机器自带 /usr/bin/cargo 是 1.75，不可用

# ①–⑤ + ⑦（不需要数据库）
bash scripts/gates.sh

# ⑥ 需要真 PostgreSQL（库要已存在；gate 自己跑迁移）
bash scripts/gates.sh --with-db --db-url 'postgres://user:pw@127.0.0.1:5432/multica_test'
```

本切片实测（rustup 1.98.1 + PostgreSQL 16）：七道门 exit 0；`route_parity.py` → upstream 456 / local 140 /
`M1 known_gap 0` / `regression 0`。

`docs/fixtures/route-parity-baseline.json`（139 条）**未刷新**：baseline 是「曾经注册过的路由」的回归记忆，
新路由不在里面不会红（`baseline ⊆ live`）；等下一轮集成统一刷新，避免和并行切片抢同一个 fixture 文件。

---

## 9. 未覆盖 / 交接

| 项 | 承接 |
| --- | --- |
| `account_disabled` / `signup_prohibited` / `email_not_allowed` 三条 403 分支 | M9（`ALLOW_SIGNUP` / `ALLOWED_EMAILS` / `ALLOWED_EMAIL_DOMAINS` + 禁用邮箱名单） |
| JWT（上游 `issueJWT`）与 CF 签名 cookie | M9 / M10 |
| `/auth/*` 的 per-IP 限流（上游 `RATE_LIMIT_AUTH`） | M9 / M10 |
| signup analytics 事件 | 不在本仓范围（无 analytics 管道） |
| `docs/22-ROUTE-PARITY.md` §3.5 那句「只剩 `POST /auth/google`」 | 下一轮集成重跑快照时刷新（本切片不动该文件，避免与工具链切片冲突） |
