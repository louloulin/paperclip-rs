# R452 — Codex Auth Merge Scripts Rust 化

## 目标

把 Node 端三个文件复刻为纯 Rust：

| Node 源文件 | 行数 | Rust 复刻 |
|---|---|---|
| `codex-auth-merge-decision.cjs` | 87 | `codex_auth_merge::decide_codex_auth_merge` + `parse_codex_auth` |
| `codex-auth-merge-extract.sh` | 73 | `codex_auth_merge::apply_codex_auth_merge` |
| `codex-auth-merge-scripts.ts` | 49 | 模块本身（todo：Wiring 进 SandboxManagedRuntimeAssetProvision） |

### 三大设计目标

1. **移除 node 子进程依赖**：原本 sandbox 启动时需要 `node decision.cjs` 才能决定 auth 副本，覆盖 Stage 跨语言。
2. **统一决策入口**：把"inbound 还原 / outbound copyback"两条不同方向打通到同一个 `decide_codex_auth_merge(snapshot, snapshot)` 纯函数。
3. **真实环境验证**：所有 57 个测试用临时目录 + 真实 JSON 文件 + 真实 fs 原子 rename。

---

## Node `.cjs` 决策算法 → Rust 端口

```js
// 5-condition 短路逻辑
if (destinationAuth.kind === "unusable" ||
    sourceAuth.kind === "unusable" ||
    sourceAuth.kind !== destinationAuth.kind ||
    destinationAuth.kind === "apikey" ||
    sourceAuth.accountId !== destinationAuth.accountId) {
  process.exit(KEEP_DESTINATION); // 20
}
if (sourceAuth.lastRefresh !== null &&
    destinationAuth.lastRefresh !== null &&
    sourceAuth.lastRefresh > destinationAuth.lastRefresh) {
  process.exit(USE_SOURCE); // 10
}
process.exit(KEEP_DESTINATION);
```

### Rust 实现

```rust
pub fn decide_codex_auth_merge(
    source: &CodexAuthSnapshot,
    destination: &CodexAuthSnapshot,
) -> CodexAuthMergeDecision {
    if destination.kind == CodexAuthKind::Unusable
        || source.kind == CodexAuthKind::Unusable
        || source.kind != destination.kind
        || destination.kind == CodexAuthKind::ApiKey
        || source.account_id != destination.account_id
    {
        return CodexAuthMergeDecision::KeepDestination;
    }
    if let (Some(src_ms), Some(dst_ms)) = (source.last_refresh_ms, destination.last_refresh_ms) {
        if src_ms > dst_ms {
            return CodexAuthMergeDecision::UseSource;
        }
    }
    CodexAuthMergeDecision::KeepDestination
}
```

完全 1:1 移植每个短路条件，**且保持保守优先**（任意条件失败 → KeepDestination）。

### parseAuth 字段映射

| Node 字段 | Rust 字段 | 含义 |
|---|---|---|
| `kind: "unusable"` | `CodexAuthKind::Unusable` | 解析失败 / 非对象 / 缺字段 |
| `kind: "apikey"` | `CodexAuthKind::ApiKey` | `OPENAI_API_KEY` 非空 |
| `kind: "subscription"` | `CodexAuthKind::Subscription` | `tokens.account_id` + 至少 1 个 token 字段 |
| `accountId` | `Option<String>` | trim 后的非空字符串 |
| `lastRefresh` | `Option<i64>` ms epoch | RFC 3339 / RFC 2822 解析失败 → None |

### `last_refresh` 解析

Node `Date.parse` 同时支持 RFC 3339 与 RFC 2822；Rust 用 `chrono::DateTime::parse_from_rfc3339` + `parse_from_rfc2822` 覆盖两种主流格式，与 Node `Number.isFinite(NaN) → null` 行为对齐。

```rust
fn parse_last_refresh_to_ms(raw: &str) -> Option<i64> {
    let trimmed = raw.trim();
    if trimmed.is_empty() { return None; }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.timestamp_millis());
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(trimmed) {
        return Some(dt.timestamp_millis());
    }
    None
}
```

---

## Extract 协调器

`apply_codex_auth_merge(asset_dir, host_auth, image_auth)`：
1. sandbox `auth.json` 存在 + host `auth.json` 存在 → 走 `decide`：
   - `UseSource` → keep_sandbox=true（保留 sandbox 字节）
   - `KeepDestination` → keep_sandbox=false（用 host 字节）
2. 选 source：keep_sandbox → sandbox；→ host；→ image fallback
3. 原子写入 `asset_dir/auth.json`（0600 + tmp + rename）

---

## 与 `codex_home.rs::has_usable_auth_payload` 的责任边界

| 关注点 | `has_usable_auth_payload` | `codex_auth_merge` |
|---|---|---|
| **目的** | 「是否可用」bool 判定 | 「用哪个」三态判定 |
| **返回** | `bool` | `CodexAuthMergeDecision` |
| **额外字段** | 无 | `account_id`, `last_refresh_ms` |
| **使用场景** | `codex_home_has_usable_auth` | sandbox merge / outbound copyback |

**为什么两个判 kind 函数并存**：
- `has_usable_auth_payload` 早就在 `codex_home.rs` 里，承担"是否可用"布尔判定
- `codex_auth_merge::parse_auth_bytes` 额外维护 `account_id` + `last_refresh_ms` 两个字段
- 二者共享同一组字段判定，Node 注释里有 `co-change` 提示，未来改字段必须同步两处

---

## 测试覆盖（57 个）

### parse_auth_bytes 单元（29 个）
- `Unusable`：invalid JSON / null / array / string / empty object / `{}` / 空 tokens / tokens=null / tokens=array / tokens 缺 account_id / account_id="" / tokens 三个字段全空 / account_id 仅空白
- `ApiKey`：`OPENAI_API_KEY` 非空 / 含前后空白 / 空字符串 / 仅空白
- `Subscription`：id_token / access_token / refresh_token 单独存在 / 三个字段 / account_id trim
- `last_refresh`：RFC 3339 Z / RFC 3339 +08:00 / 不可解析 / 缺失 / 空字符串 / 非字符串

### decide_codex_auth_merge 单元（15 个）
- 5-condition 短路：destination unusable / source unusable / both unusable / kind mismatch / destination apikey / both apikey / account_id mismatch
- last_refresh：source 严格更新 / source 较旧 / 平局 / source 缺 / destination 缺 / 双缺
- exit_code 映射：10/20/0/99

### 异步 end-to-end（13 个）
- 读盘 / 缺文件 / 目录当文件
- `_from_paths` 三种场景（newer / unparseable / account_id mismatch）
- `apply_codex_auth_merge`：installed_host / keeps_sandbox / falls_back_to_image / no_auth / overwrites_when_host_fresher / account_id_mismatch_installs_host / kind_mismatch_installs_host / sandbox_unusable_installs_host / host_unusable_keeps_sandbox
- `write_codex_auth_atomic_creates_file`
- 决策不泄露 token bytes（format!("{:?}", decision) 不含 SENTINEL）

### 并行安全
- `tempdir()` 用 `AtomicU64` 计数器 + nanos + PID 拼接，避免高并发测试温度下临时目录冲突

---

## 文件清单

- **新建**：`crates/pc-adapter-codex-local/src/codex_auth_merge.rs`（约 1250 行）
- **修改**：`crates/pc-adapter-codex-local/src/lib.rs`（新增 `pub mod codex_auth_merge;`）

## 测试结果

```
codex_auth_merge::tests: 57 passed, 0 failed
pc-adapter-codex-local: 223 passed (166 prior + 57 new)
pc-acpx: 883 passed
pc-adapter-claude-local: 153 passed
pc-adapter-process: 6 passed
pc-activity: 14 passed
pc-adapter-quota: (上一次 39, 本次待复跑)
```

---

## 后续

- **R453** `prepare_codex_remote_managed_home` 复刻（90 行，依赖 R452 + `stage_codex_home_for_sync`）
- **R454** `create_codex_acp_executor` factory（20 行，需完整 AcpxEngineExecutor integration）
- **R455** pc-http executionTarget 注入（打通远程执行路径）
- **R456** 其他 adapter（gemini/grok/opencode）— 按用户约束延后
- **R457** quota.ts 完整复刻
- **R458** test.ts (test_environment) 完整复刻
- **R459** pc-repos / pc-heartbeat 深化

## 当前差距

| 维度 | 已经实现 | 后续 |
|---|---|---|
| codex 适配器 | ~95% | R453-R455 |
| claude 适配器 | ~92% | （优先其他） |
| pc-acpx 核心 | ~95% | （少量边界） |
| quota / heartbeat | ~85% | R457 |
| 其他 adapter | 0% | R456（延后） |
