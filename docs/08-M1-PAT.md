# M1-PAT — Personal Access Token user-facing 路由

对应 issue：`LUM-1344`（M1 sub-issue C 范围内）

## 范围

multica 上游 PAT 主要在 daemon auth 中间件里被消费，没有显式 `/api/pats` REST
endpoint。multica-rs 自加一组 `/api/me/pats` 路由，便于用户管理自己的 PAT：

- 创建时返回明文 token（仅此一次）
- 列表只暴露 last4 + 显示前缀
- 撤销走 `DELETE /api/me/pats/{id}`

不在本 sub-issue 范围内：
- PAT DB repo 实现 → sub-issue B
- daemon auth 中间件消费 PAT → M3

## 路由清单

| Method | Path | Handler | 鉴权 |
| --- | --- | --- | --- |
| GET | `/api/me/pats` | `list_my_pats` | authenticated |
| POST | `/api/me/pats` | `create_my_pat` | authenticated |
| DELETE | `/api/me/pats/{id}` | `revoke_my_pat` | authenticated |

命名刻意避开 sub-issue A 的 `/api/me` 路由族。

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

集成（`crates/mc-http/tests/pats.rs`，需要 `--features test-util`）：
- create + list + revoke 端到端
- 空 name → 400
- 缺失 `X-Multica-User-Id` → 401
