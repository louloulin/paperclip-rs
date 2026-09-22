# M1-PAT — Personal Access Token user-facing 路由

对应 issue：`LUM-1344`（M1 sub-issue C 范围内）；路由命名由 **`LUM-1362`（M1-E）更正**。

> ⚠️ **2026-09-22 更正（LUM-1362 / M1-E）**：本文件原先写「multica 上游……没有显式 PAT
> REST endpoint」是**事实错误**。上游有完整的 PAT REST 面：
> `server/cmd/server/router.go:1879-1884` → `r.Route("/api/tokens", …)`，含
> `GET /`、`POST /`、`POST /current/renew`、`DELETE /{id}`（handler 在
> `internal/handler/personal_access_token.go`）。本仓原先把主路径定为 `/api/me/pats`
> （M0/M1-C 自造，上游无此路径），已迁到 **`/api/tokens`**。
> 完整决策、残留偏离与上游行号索引见 **`docs/17-M1-CONTRACT-GAPS.md`**。

## 范围

用户管理自己的 PAT（上游同名能力，只是上游主要是给 daemon 消费）：

- 创建时返回明文 token（仅此一次）
- 列表只暴露 last4 + 显示前缀
- 撤销走 `DELETE /api/tokens/{id}`
- 就地续期走 `POST /api/tokens/current/renew`

不在本 sub-issue 范围内：
- PAT DB repo 实现 → sub-issue B（表 `personal_access_token`，迁移 `0003`）
- daemon auth 中间件消费 PAT → M3

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
两套路径共用同一 handler / 同一 store，行为等价。

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
4. token_hash = sha256_hex(raw)
5. token_last4 = raw.chars().rev().take(4).collect()
6. PUT into PatStore (in-memory for M1, DB-backed post sub-issue B)
7. 返回 { dto, token } —— token 仅此一次
```

## 撤销

```
1. 从路径拿 id
2. 列出当前用户的 PAT —— 校验 id 属于当前用户（防止越权撤销）
3. DELETE from PatStore
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
- 算 sha256 hex → 查 `personal_access_token.token_hash`
- 校验未过期 → 取出 user_id → 进入 session

具体中间件代码在 M3，本 sub-issue C 不实现。

## 测试

单元：
- `generate_pat_secret`：64 字符 hex
- `sha256_hex`：确定性
- `last4_extraction`：取末 4 位
- `pat_prefix_format`：以 `mk_pat_` 开头

集成（`crates/mc-http/tests/pats.rs` + `crates/mc-http/tests/contract_gaps.rs`，需要 `--features test-util`）：
- `/api/me/pats` create + list + revoke 端到端（alias，带 `Deprecation` 头）
- `/api/tokens` create + list + revoke 端到端
- `expires_in_days` 生效；`current/renew` 的四种分支
- 空 name → 400
- 缺失 `X-Multica-User-Id` → 401
