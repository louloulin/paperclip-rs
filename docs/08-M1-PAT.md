# M1-PAT — Personal Access Token user-facing 路由

对应 issue：`LUM-1344`（M1 sub-issue C 范围内）；路由命名由 **`LUM-1362`（M1-E）更正**。

> ⚠️ **2026-09-22 更正（LUM-1362 / M1-E）**：本文件原先写「multica 上游……没有显式 PAT
> REST endpoint」是**事实错误**。上游有完整的 PAT REST 面：
> `server/cmd/server/router.go:1879-1884` → `r.Route("/api/tokens", …)`，含
> `GET /`、`POST /`、`POST /current/renew`、`DELETE /{id}`（handler 在
> `internal/handler/personal_access_token.go`）。本仓原先把主路径定为 `/api/me/pats`
> （M0/M1-C 自造，上游无此路径），已迁到 **`/api/tokens`**。
> 完整决策、残留偏离与上游行号索引见 **`docs/17-M1-CONTRACT-GAPS.md`**。

> ⚠️ **2026-09-22 更正（LUM-1375 / M1-F）**：本文件原先写「存储后端 = `PatStoreContainer`
> （默认 `InMemoryPatStore`）」已过时。**`/api/tokens*` 现在直连
> `mc_repos::pat::PatRepo`（表 `personal_access_token`）**；`mc-auth` 的内存实现只作
> 无库场景的 fallback（`docs/17` R7 已闭环，遗留的 `POST /api/cli-token` 见 R9）。
> 出处：`crates/mc-http/src/routes/pats.rs`、`crates/mc-http/src/state.rs`、
> `crates/mc-repos/src/pat.rs`。

## 范围

用户管理自己的 PAT（上游同名能力，只是上游主要是给 daemon 消费）：

- 创建时返回明文 token（仅此一次）
- 列表只暴露 last4 + 显示前缀
- 撤销走 `DELETE /api/tokens/{id}`
- 就地续期走 `POST /api/tokens/current/renew`

不在本 sub-issue 范围内：
- PAT DB repo 实现 → sub-issue B（表 `personal_access_token`，迁移 `0002`/`0003`）
- daemon auth 中间件消费 PAT → M3

> 上述两项的**接线**已由 **M1-F（LUM-1375）** 完成第一半：DB 仓储被 `/api/tokens*`
> 真实消费；daemon 中间件（含 `PatRepo::touch` 的调用点）仍属 M3。

## 路由清单

| Method | Path | Handler | 鉴权 | 上游出处 |
| --- | --- | --- | --- | --- |
| GET | `/api/tokens` | `list_my_pats` | authenticated | router.go:1880 |
| POST | `/api/tokens` | `create_my_pat` | authenticated | router.go:1881 |
| POST | `/api/tokens/current/renew` | `renew_current_pat` | **bearer PAT 自身** | router.go:1882 |
| DELETE | `/api/tokens/{id}` | `revoke_my_pat` | authenticated | router.go:1883 |
| GET | `/api/me/pats` | 同上（alias） | authenticated | —（deprecated，本仓历史路径） |
| POST | `/api/me/pats` | 同上（alias） | authenticated | —（deprecated） |
| DELETE | `/api/me/pats/{id}` | 同上（alias） | authenticated | —（deprecated） |

`/api/me/pats*` 三条是 M1-C 的历史路径，**保留一个发布周期**作为 deprecated alias：
响应带 `Deprecation: true` 与 `Link: </api/tokens>; rel="successor-version"`（RFC 8594）。
两套路径共用同一 handler / 同一仓储，行为等价（M1-F 起 handler 内部走 `PatRepo`，
alias 行为零回归，见 `crates/mc-http/tests/pats.rs` 的迁移头守卫用例）。

## 存储后端（M1-F / LUM-1375 起）

| | 实现 | 用途 |
| --- | --- | --- |
| (**生产**) | `mc_repos::pat::PatRepo` → 表 `personal_access_token` | `/api/tokens*` 的全部读写：`create` / `list_for_user` / `revoke` / `get_by_token` / `update_expires_at` |
| fallback | `mc_auth::InMemoryPatStore`（`PatStoreContainer`） | 仅供无库测试；**生产路径不再经过它** |

三个 handler 每次请求都 `PatRepo::new(state.db.clone())`，与 `routes/auth.rs` 里
`VerificationCodeRepo` 的用法一致（`mc-auth` 保持与 DB 无关，不加 `sqlx` 依赖）。

- **撤销是软删**：`UPDATE … SET revoked_at = now()`（迁移 `0002`），行保留；
  `list_for_user` / `get_by_token` 都带 `revoked_at IS NULL` 过滤。
- **续期只改过期时间**：`PatRepo::update_expires_at` 一次 UPDATE，不轮换明文 token。
- **重启后仍然有效**：token 落在 PG 而不是进程内存。

## Token 形态

- 明文：`mk_pat_<64-char-hex>`（前缀 + 32 字节随机 → hex）
- 存储：`token_hash = sha256_hex(raw_hex_64)`（仅 hash 入库）
- 显示：`mk_pat_<last4>`（仅暴露最后 4 位）

前缀 `mk_pat_` 与 `mc_auth::DEFAULT_API_KEY_PREFIX = "mk_"` 一致，便于 daemon
auth 中间件（M3）沿用同一解析路径。

## hash 算法

- sha256 → 64 字符小写 hex
- 与 multica 上游 `personal_access_tokens.token_hash` 列一致

## 创建流程

```
1. 校验 name 非空
2. 生成 raw = hex(32 random bytes)
3. token = "mk_pat_" + raw
4. token_hash = PatRepo::hash_token(raw)   // sha256 hex
5. token_last4 = PatRepo::last4(raw)
6. INSERT INTO personal_access_token … RETURNING *（PatRepo::create）
7. 返回 { dto, token } —— token 仅此一次，响应字段取自库内真实行
```

hash / last4 必须走 `PatRepo` 的 canonical helper（而不是路由层自己再实现一遍），
否则 list 的 `display_token` 与 daemon（M3）反查出的行会对不上。

## 撤销

```
1. 从路径拿 id
2. list_for_user 当前用户 —— 校验 id 属于当前用户（防止越权撤销；别人的 id 一律 404，
   不泄露「该 id 存在」）
3. PatRepo::revoke(id) → UPDATE personal_access_token SET revoked_at = now()
```

## 续期（`POST /api/tokens/current/renew`）

上游 `RenewCurrentPersonalAccessToken`（`internal/handler/personal_access_token.go:159`）语义：

```
1. 从 Authorization: Bearer <pat> 取明文（必须带 `Bearer ` 前缀，且以 PAT 前缀开头）
2. 非 PAT 凭据 → 400 only personal access tokens can be renewed
3. 查不到 / 已过期 → 401 token is no longer valid
4. 剩余寿命 > 7 天（PATRenewThreshold）→ 200 {expires_at, renewed: false}（不是错误）
5. 否则 expires_at = now + 90 天（PATRenewExtension）→ 200 {expires_at, renewed: true}
```

第 5 步的写入口是 `PatRepo::update_expires_at`（**落库**）；只改响应不写库的话，
重启后 token 仍会按旧时间过期。第 3 步的「查不到」由 `get_by_token` 的 SQL
（`revoked_at IS NULL AND expires_at > now()`）承担——本仓没有上游那个 auth 中间件。

**不轮换明文 token**：上游刻意如此——CLI 与 daemon 多进程共享同一 PAT，
轮换会同时打断所有进程。

本仓差异（记录在 `docs/17` 决策 D5）：身份直接取自 bearer 对应的 PAT 行，
**不**要求 `x-multica-user-id`；并显式拦掉已过期 token（上游由 auth 中间件拦）。

## 创建请求体兼容

| 字段 | 上游 | 本仓 |
| --- | --- | --- |
| `name` | ✓ | ✓ |
| `expires_in_days` | ✓（`nil`/`<= 0` = 永不过期） | ✓（`> 0` 时生效，否则退化为默认 30 天，见 `docs/17` R2） |
| `scopes` | — | ✓（本仓扩展） |
| `ttl_secs` | — | ✓（本仓扩展，优先级低于 `expires_in_days`） |

## 与 daemon auth 中间件（M3）对接

daemon 发起调用时通过 `Authorization: Bearer mk_pat_<raw>`：

- 中间件解析出 raw
- 算 sha256 hex（`PatRepo::hash_token`，与入库口径同源）→ 查
  `personal_access_token.token_hash`
- 校验未过期 → 取出 user_id → 进入 session
- 命中后调用 `PatRepo::touch(id)` 维护 `last_used_at`

具体中间件代码在 M3；本切片只保证 `PatRepo::touch` **函数可达 + DB 测试覆盖**
（见 `crates/mc-http/tests/pats.rs`），不在路由层实现中间件。

## 测试

单元（`cargo test -p mc-http --lib`）：
- `generate_pat_secret`：64 字符 hex
- `token_hash_is_sha256_hex`：钉死已知 sha256 值（helper 被换掉会立刻红）
- `last4_extraction`：与入库 helper 同源，含短于 4 位的约定
- `pat_prefix_format`：以 `mk_pat_` 开头

集成（`crates/mc-http/tests/pats.rs` + `crates/mc-http/tests/contract_gaps.rs`，需要
`--features test-util`）。**M1-F 起 PAT 用例与 workspace/member 用例同样需要真实 PG**
（`/api/tokens*` 走表，不再有纯内存路径），因此全部标 `#[ignore]` + `MULTICA_TEST_DATABASE_URL`：

无库守卫（默认跑，`Db::placeholder()`，判定发生在触库之前）：
- 缺 `X-Multica-User-Id` → 401（四条路径）
- 空 / 纯空白 name → 400
- 路径 id 非 uuid → 404；alias 带 `Deprecation`/`Link` 头，新路径不带
- 非 PAT 凭据续期 → 400

DB e2e（`--ignored`，4 条）：
- 创建 + 列表 + 撤销 + `expires_in_days` 兼容 + `current/renew` 四种分支
- **跨「进程」持久化**：丢弃第一个 `AppState`，用新连接池重建后 `GET /api/tokens`
  仍列出同一 token，且 `last_used_at` 来自库
- 续期后的新 `expires_at` 在**新连接**里可见
- 撤销后 `revoked_at` 非空、列表不再返回、重复撤销 404、续期 401
- 跨用户隔离：别人的 PAT 不被列出、不被撤销

```text
MULTICA_TEST_DATABASE_URL=postgres://multica:multica@localhost:5432/multica_m1f \
  cargo test -p mc-http --features test-util --test pats -- --ignored
```
