# R514 — V5 Auth: API key `pk_` 前缀 (machine-to-machine)

> 配套: `proposal.md` V5 (API key pk_ 前缀) + `ARCHITECTURE.md` §6 R514 路线图。
> 目标: 把 V5 Auth 从 70% → 80%——补全 API key 前缀约定，确立 machine-to-machine token 语义。

## 改动

### 1. `crates/pc-auth/src/lib.rs` — KeyPrefix enum + helper API

**`KeyPrefix` enum**:
- `Pk` → `"pk_"` — Board user API key (machine-to-machine)
- `Sess` → `"sess_"` — 预留 (future per-session tokens)

**`KeyPrefix::parse(token) -> Option<Self>`** — 识别多种前缀并归类：
- `pk_<...>` → `Pk` (R514 当前约定)
- `pcak_<...>` → `Pk` (legacy `ApiKeyIssuer::new_token()` 兼容)
- `pcp_board_<...>` → `Pk` (legacy `routes/access.rs::board_keys_create` 兼容)
- `sess_<...>` → `Sess`
- 其他 / 空 body / 空字符串 → `None`

**`generate_api_key(prefix: KeyPrefix) -> String`**:
- 24 bytes random → 32 url-safe base64 chars (no padding)
- 格式 `{prefix.as_str()}{body}`，总长 35 (Pk) 或 37 (Sess)

**`has_key_prefix(token, expected) -> bool`** — 防御性 prefix 校验：
- 即使 hash 巧合碰撞，错误前缀也不会被当成 API key
- 在 `resolve_api_key` 入口处先调用，省一次 DB 查询

### 2. `crates/pc-auth/src/lib.rs::resolve_api_key` — 加 prefix guard

```rust
pub async fn resolve_api_key(db: &Db, token: &str) -> Result<Option<(Uuid, String)>, AuthError> {
    // R514: 防御性 prefix 校验
    if !has_key_prefix(token, KeyPrefix::Pk) {
        return Ok(None);
    }
    let h = hash_token(token);
    // ... 原有 SQL 查询
}
```

### 3. `crates/pc-http/src/routes/access.rs::board_keys_create` — 切换到 pk_ 前缀

```rust
// 旧: let token = random_cli_token("pcp_board_");  // pcp_board_<uuid><uuid>
let token = pc_auth::generate_api_key(pc_auth::KeyPrefix::Pk);  // pk_<32 url-safe chars>
```

新 key 长度固定 35 chars，URL-safe，无 `+/=` (便于放在 HTTP header)。

## 测试 (13 个新 R514 tests)

| 测试 | 验证 |
|---|---|
| `r514_key_prefix_pk_is_pk` | `KeyPrefix::Pk.as_str() == "pk_"` |
| `r514_key_prefix_sess_is_sess` | `KeyPrefix::Sess.as_str() == "sess_"` |
| `r514_key_prefix_parse_recognizes_pk` | `pk_<...>` → Pk |
| `r514_key_prefix_parse_recognizes_legacy_pcak` | `pcak_<...>` → Pk (legacy 兼容) |
| `r514_key_prefix_parse_recognizes_legacy_pcp_board` | `pcp_board_<...>` → Pk (legacy 兼容) |
| `r514_key_prefix_parse_recognizes_sess` | `sess_<...>` → Sess |
| `r514_key_prefix_parse_rejects_empty_body` | `pk_` / `pcak_` / `pcp_board_` / `sess_` (空 body) → None |
| `r514_key_prefix_parse_rejects_unknown_prefix` | 未知前缀 / `sk_` / 空字符串 → None |
| `r514_generate_api_key_has_pk_prefix` | 生成 token 有 `pk_` 前缀，长度精确 35 |
| `r514_generate_api_key_unique_across_calls` | 两次调用结果不同 |
| `r514_has_key_prefix_accepts_matching` | matching prefix → true |
| `r514_has_key_prefix_rejects_mismatch` | session↔api key 互不通用 + 未知前缀 → false |
| `r514_pk_token_url_safe` | body 部分 URL-safe base64 (no `+/=`) |

## 验证

```
cargo test -p pc-auth --lib                80 passed (67 pre + 13 R514 new)
cargo check --workspace                    0 errors (170 pre-existing warnings)
```

## 设计要点

1. **前缀兼容三种格式** (`pk_` / `pcak_` / `pcp_board_`)：旧 hashed rows 仍能 resolve，新 minted keys 走 `pk_`。这避免了"重 hash 全部现有 key"的强制 migration。
2. **空 body 拒绝 (`pk_` → None)**：保证不存储空 hash 的脏数据进入 DB。
3. **`has_key_prefix` 是短路 guard**：session token 永远不会被当 API key 用，即使 hash 巧合碰撞；省一次 DB 查询。
4. **`generate_api_key` 长度固定 35**：24 bytes random → 32 url-safe base64 chars (no padding) + 3 char prefix；URL-safe，无 `+/=`，便于放 HTTP header / URL query。
5. **未替换 `ApiKeyIssuer::new_token()`**：保留原 `pcak_` 路径作为 compat 层；`parse` 把它也认成 Pk，避免一次性清理多个调用点。后续 R515+ 可统一所有 `new_token()` 走 `generate_api_key(KeyPrefix::Pk)`。

## V5 真实进度更新

- **R513 末**: ~70% (refresh rotation + reuse detection 完成；剩 OAuth / CSRF / API key pk_)
- **R514 末**: **~80%** ⭐ — API key 前缀约定 (Pk + 2 legacy aliases) 完成；剩 OAuth / CSRF

## 教训

1. **`parse` 比 `strip_prefix` 更严格**：仅当 prefix 后还有非空 body 时才算有效 token；空 body（`pk_`）视为无效 token，拒绝写入 DB。
2. **Legacy alias 是渐进式迁移的关键**：现有 hashed rows 不能强制 rotate，否则所有现有 CI 集成会一次性断掉；通过 `parse` 识别多个 prefix 实现渐进迁移。
3. **防御性 prefix guard 比 hash collision 罕见**：实际 collision 概率 ~2^-128；但 prefix guard 让"session token 永远不会被当成 API key"成为**不变量**，比 hash 强。
4. **API key 是身份凭据，不要暴露内部 hash 算法**：通过 `generate_api_key` 集中 token 生成逻辑，避免散落在多个 route handler。

## 下一步 (R515+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R515** | V5 Auth: CSRF token 验证（state-changing endpoints） | V5 80% → 85% |
| **R516** | V6 收尾: Companies 聚合端点 schemas (stats/timeline/artifacts/org) | V6 95% → 100% |
| **R517** | V5 Auth: OAuth 2.0 client (Google/GitHub provider) | V5 80% → 90% |
| **R518** | V4 OpenAPI ↔ UI 类型对齐: 生成 types.ts 给 ui/ 60 client 用 | V4 0% → 60% |
