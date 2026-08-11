# R512 — V5 Auth Refresh Token Rotation (Family + Reuse Detection)

> 配套: `proposal.md` V5 (refresh rotation / OAuth / CSRF / API key pk_) + `ARCHITECTURE.md` §6 R512 路线图。
> 目标: 把 V5 Auth 从 55% → 70%——补全 refresh token rotation 的 family tracking 与 reuse detection。

## 改动

### 1. `crates/pc-auth/src/session_refresh.rs` — 纯逻辑增强

**SessionRecord 加字段**:
- `revoked_at: Option<DateTime<Utc>>` — token 作废时间；`None` 表示未作废。`#[serde(default, skip_serializing_if = "Option::is_none")]` 保证向后兼容旧 session JSON。

**新 SessionCheckOutcome 变体**:
- `Revoked` — token 已作废（轮换 / 显式登出 / 重用作废）；在 idle / absolute 之前先检查。

**新 pure helpers**:
- `mark_revoked(record, now) -> SessionRecord` — 返回新 record（不动其他字段），设置 `revoked_at = Some(now)`。
- `is_revoked(record) -> bool` — 短路检查。
- `ReuseOutcome { Ok, ReuseDetected }` — 判定结果 enum + `is_reuse()` 辅助。
- `detect_reuse(presented, family) -> ReuseOutcome` — 两条规则任一命中即 ReuseDetected:
  1. **presented.revoked_at.is_some()** — 旧 token 又被拿来用
  2. **family 中存在 sibling 满足** `revoked_at.is_none() && last_rotated_at > presented.last_rotated_at` — token 已被轮换，旧 token 仍在被使用

**touch_session / rotate_session / new_session 同步带 `revoked_at`**:
- 默认 `None`；rotate/touch 后保留原值（rotate 时旧 token 由调用方设 revoked）。

### 2. `crates/pc-auth/src/auth_service.rs` — 存储层 + service 接线

**auth_service::SessionRecord 加字段**:
- `family_id: Uuid` — 同一 sign-in 产生的所有轮换 token 共享一个 family；`#[serde(default = "Uuid::new_v4")]` 保证向后兼容。
- `revoked_at: Option<DateTime<Utc>>` — 同上（`#[serde(default, skip_serializing_if = "Option::is_none")]`）。

**SessionStore trait 新增 3 方法**:
```rust
async fn find_family(&self, family_id: Uuid) -> Result<Vec<SessionRecord>, AuthServiceError>;
async fn mark_revoked(&self, token_hash: &str, at: DateTime<Utc>) -> Result<(), AuthServiceError>;
async fn invalidate_family(&self, family_id: Uuid, at: DateTime<Utc>) -> Result<usize, AuthServiceError>;
```

**InMemorySessionStore 实现**:
- `find_family` — 线性扫描 store，按 family_id 过滤。
- `mark_revoked` — 找到对应 record 设 `revoked_at`。
- `invalidate_family` — 批量标记 family 内所有未作废的 token。
- **`rotate` 改为 mark-revoke + insert**（不再 remove）：保留旧 token 记录用于 reuse detection。

**AuthServiceError 新变体**:
- `SessionReuseDetected` — 攻击信号，整个 family 已作废。

**`refresh_session` 完整重写**（5 步）:
1. `find_by_token_hash(old)` → 必须存在。
2. **idle/absolute/revoked check** —— 命中 `Revoked` 即作废整个 family，返回 `SessionReuseDetected`。
3. **detect_reuse** —— 拉取 family 全部成员扫描；若任一 sibling 触发 ReuseDetected，作废 family。
4. 颁发新 token，**继承 family_id**（不变），`revoked_at = None`。
5. `rotate(old_hash, new_session)` —— 旧 token 自动被设 revoked（实现层保证）。

**`new_session_record`（sign-up/sign-in 时）** — family_id 用 `Uuid::new_v4()` 分配新 family。

**测试辅助**:
- `InMemorySessionStore::find_family_for_token(token) -> Option<Vec<SessionRecord>>` — 通过 raw token 找 family_id，再查全部成员。

### 3. 测试 (11 个新 R512 tests)

### session_refresh (9 tests)

| 测试 | 验证 |
|---|---|
| `r512_new_session_has_revoked_at_none` | 新会话初始 revoked_at = None |
| `r512_mark_revoked_sets_timestamp_and_preserves_other_fields` | mark_revoked 只改 revoked_at，其他字段不变 |
| `r512_check_session_returns_revoked_when_revoked_at_set` | check_session 命中 Revoked 分支 |
| `r512_revoked_takes_priority_over_idle_and_absolute` | Revoked 优先级最高（在 idle/absolute 之前） |
| `r512_detect_reuse_ok_for_fresh_presented_alone` | family 只有 presented 本身 → Ok |
| `r512_detect_reuse_fires_when_presented_is_revoked` | presented 自己已作废 → ReuseDetected |
| `r512_detect_reuse_fires_when_sibling_is_newer_and_active` | 兄弟 newer + active → ReuseDetected |
| `r512_detect_reuse_ok_when_newer_sibling_is_also_revoked` | 兄弟 newer + 已被强制作废 → Ok |
| `r512_detect_reuse_skips_self_when_comparing_siblings` | family 中只有 presented 自己，不应误判 |

### auth_service (2 integration tests + 1 updated)

| 测试 | 验证 |
|---|---|
| `r569_refresh_session_rotates_token` (更新) | 旧 token 第二次 refresh 现返回 SessionReuseDetected（更严格） |
| `r512_refresh_session_keeps_family_id_stable_across_rotations` | 3 次轮换后 family_id 稳定，family.len = 1/2/3 递增 |
| `r512_refresh_session_reuse_triggers_family_invalidation` | 攻击者用旧 token refresh → SessionReuseDetected → 整个 family 作废 → 即便合法新 token 也不能 refresh |

## 验证

```
cargo test -p pc-auth --lib           67 passed (56 pre + 11 R512 new)
cargo check --workspace                0 errors (170 pre-existing pc-http warnings)
```

## 设计要点

1. **family_id 是同一 sign-in 的所有轮换 token 的公共祖先** —— 保证攻击者拿到的"旧的/作废的" token 一旦复用，整个会话树被连带作废（包括用户当前最新 token），强制重新登录。
2. **rotate 改为 mark-revoke + insert**（不再 remove）—— 是 reuse detection 能工作的前提；否则 find_by_token_hash 返回 None，revoked branch 永远命中不到。
3. **detect_reuse 是纯函数** —— 不读存储；调用方传入 family 快照。这保持 session_refresh 模块的纯度（无 IO、无 async）。
4. **`revoked_at` 在 idle/absolute 之前检查** —— 即使一个 token "还没过期"（idle/absolute 都未到），只要它被作废，就视为不可用 + 攻击信号。这是 OAuth/Refresh-Token Rotation RFC 推荐的 Best Current Practice。
5. **family_id 用 `#[serde(default = "Uuid::new_v4")]`** —— 旧 session JSON 没有 family_id 字段时反序列化时自动分配新 family（虽然旧 token 仍可用，但因为是新 family，reuse detection 不会跨 session 误判）。
6. **revoked_at 用 `skip_serializing_if`** —— 不写 None 进 JSON，节省字节；同时保证新 session 写出去就是"无 revoked_at 字段"格式，兼容老 reader。

## V5 真实进度更新

- **R510 末**: ~55% (refresh rotation 已 wire 但无 reuse detection；OAuth / CSRF / API key pk_ 未做)
- **R512 末**: **~70%** — refresh rotation 完整：family tracking + reuse detection + 整个 family invalidation on attack；OAuth / CSRF / API key pk_ 仍未做

## 教训

1. **`#[serde(default)]` 是兼容升级的关键** —— 加新字段时如果旧 JSON 没有该字段，没 default 就 deserialization 失败；这次给 `family_id` 和 `revoked_at` 都加了 default，旧 session 存储数据不用迁移。
2. **rotate 语义变更需要更新所有 implementer** —— 我们只有一个 InMemorySessionStore 实现，所以简单；生产中如果有 DB-backed 实现，也要同步改为 mark-revoke + insert（或在 rotate 后再写一条 revoked 记录）。
3. **测试要覆盖"自己跟自己比较"** —— detect_reuse 跳过自己是用 `issued_at + last_rotated_at` 双键匹配的；如果用单一键比较就会出现"自己比自己 newer"的 false positive。`r512_detect_reuse_skips_self_when_comparing_siblings` 这个测试是设计安全网。
4. **测试 helper `find_family_for_token` 不能走 trait** —— 它只是测试辅助，不能放进 `SessionStore` trait（生产实现不一定能直接暴露 token 哈希逻辑）。把它放在 InMemorySessionStore 实现上是合理的边界。

## 下一步 (R513+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R513** | V6 路由补全: companies 子路由 (members/skills/policies) + admin 路由 | V6 86% → 95% |
| **R514** | V5 Auth: API key `pk_` 前缀 (machine-to-machine) | V5 70% → 80% |
| **R515** | V5 Auth: CSRF token 验证（state-changing endpoints） | V5 70% → 85% |
| **R516** | V5 Auth: OAuth 2.0 client (Google/GitHub provider) | V5 70% → 90% |
