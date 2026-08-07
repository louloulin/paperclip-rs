# Round 368 — Acpx-engine Cache + Env Helpers (B3.1 第七阶段)

> 适用版本：`paperclip-rs` 截至 R368（R367 = 1218 → R368 = **1243**，+25 pc-acpx 测试）
> 参考实现：`paperclip` Node（`packages/adapter-utils/src/acpx-engine/execute.ts` 中 `cleanupIdleHandles` / `cleanupIdleStagedRuntimes` / `saveStagedRuntimeAfterCleanTurn` / `discardStagedRuntime` / `withSessionStagingLease` / `resolveRuntimeEnv`；`server-utils.ts` 中 `ensurePathInEnv` / `defaultPathForPlatform`）
> 测试基线：`cargo test -p pc-acpx` 227/227 绿（179 unit + 8+4+4+4+9+9+10 integration）；`pc-heartbeat` 928/928 全量无回归；`cargo build --workspace --bins` 通过；`cargo fmt --all -- --check` 通过

---

## 🎯 R368 目标

完成 **acpx-engine cache 生命周期 + env 解析层**的 Rust 化迁移（B3.1 第七阶段）：

1. **cache 模块**：`IdleCache<K, V>` 通用 cache + `LastUsed` trait + idle eviction sweep + `AsyncKeyedLocks` per-key async lease
2. **env_helpers 模块**：`ensure_path_in_env` + `default_path_for_platform` + `resolve_runtime_env`

**为什么这一阶段关键**：cache 是 acpx-engine 唯一会**跨 run 持久化**的状态——warm handle cache 让 resume 不需要重新 spawn agent 进程,staged runtime cache 让远程 sandbox staging 不需要每次重做。这是性能契约,不是可选优化。env helpers 看似简单,但是**所有 subprocess 启动路径**都要走它,任何 bug 都会阻断整个 engine。

---

## 🏗️ 新增模块

```
crates/pc-acpx/src/
├── cache.rs            # NEW：通用 cache + idle eviction + per-key async lock
└── env_helpers.rs      # NEW：ensure_path_in_env + default_path + resolve_runtime_env

crates/pc-acpx/tests/
└── round368_cache_lifecycle.rs    # NEW：端到端集成测试
```

---

## 📐 1. cache 模块

### 公开 API

```rust
pub trait LastUsed {
    fn last_used_at(&self) -> i64;
}

pub struct IdleCache<K, V> {
    entries: HashMap<K, (V, i64)>,  // value + last-used timestamp
}

impl<K, V> IdleCache<K, V>
where
    K: Eq + Hash,
{
    pub fn new() -> Self;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn get(&self, key: &K) -> Option<&V>;
    pub fn get_mut(&mut self, key: &K) -> Option<&mut V>;
    pub fn contains(&self, key: &K) -> bool;
    pub fn put(&mut self, key: K, value: V, now: i64) -> Option<V>;  // 保留旧 timestamp
    pub fn replace(&mut self, key: K, value: V, now: i64) -> Option<V>;  // 覆盖 + refresh timestamp
    pub fn touch(&mut self, key: &K, now: i64);  // 只 refresh timestamp
    pub fn remove(&mut self, key: &K) -> Option<V>;
    pub fn last_used_at(&self, key: &K) -> Option<i64>;
    pub async fn cleanup_idle<F, Fut>(&mut self, now: i64, idle_ms: i64, closer: F) -> Vec<K>;
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)>;
    pub fn snapshot(&self) -> Vec<(K, V)>;
}

pub struct IdleEvictionReport<K> {
    pub evicted: Vec<K>,
}

pub async fn cleanup_idle_with_report<K, V, F, Fut>(
    cache: &mut IdleCache<K, V>, now: i64, idle_ms: i64, closer: F
) -> IdleEvictionReport<K>;

pub struct AsyncKeyedLocks<K: Eq + Hash + Clone> {
    locks: Mutex<HashMap<K, Arc<Semaphore>>>,
}

impl<K: Eq + Hash + Clone> AsyncKeyedLocks<K> {
    pub fn new() -> Self;
    pub async fn with_lease<F, Fut, T>(&self, key: K, f: F) -> T;
    pub fn is_locked(&self, key: &K) -> bool;
}
```

### 与 Node 的对位

| Node 函数 | Rust 抽象 |
|---|---|
| `Map<key, RuntimeCacheEntry>` (warm handle cache) | `IdleCache<K, RuntimeCacheEntry>` |
| `Map<key, StagedRuntimeCacheEntry>` (staged runtime cache) | `IdleCache<K, StagedRuntimeCacheEntry>` |
| `cleanupIdleHandles(handles, now, idleMs, closeWarmHandle)` | `cache.cleanup_idle(now, idleMs, close_closure)` |
| `cleanupIdleStagedRuntimes(handles, locks, now, idleMs)` | 同上 + `locks` 独立管理 |
| `withSessionStagingLease(locks, key, fn)` | `locks.with_lease(key, fn)` |

### 关键设计

1. **`LastUsed` trait**:最小接口,允许任何 value 类型携带自己的时间戳(acpx-engine 后续给 `RuntimeCacheEntry` / `StagedRuntimeCacheEntry` 实现即可)
2. **`put` 保留 timestamp**:语义匹配 Node `Map.set`——重复 put 不重置 idle 窗口,避免 stale write 意外延长 session 寿命
3. **`replace` 覆盖 timestamp**:显式覆盖语义,需要确认 owner 关系时使用
4. **`cleanup_idle` 接受 async closer**:Node 的 `closeWarmHandle` 是 async,Rust 端用 `FnMut(K, V) -> Fut` where `Fut: Future<Output = ()>` 表达
5. **`idle_ms <= 0` 短路**:和 Node 完全一致——配置 0 表示不自动清理
6. **`AsyncKeyedLocks` 用 `tokio::sync::Semaphore`**:每个 key 一个 permit,自动 lazy create。同一 key 严格串行,不同 key 完全并行

### 单元测试覆盖（8 个）

- `cleanup_idle_skips_when_idle_window_is_zero`
- `cleanup_idle_evicts_stale_entries`
- `cleanup_idle_calls_closer_with_evicted_value`
- `put_returns_existing_value_and_keeps_timestamp`
- `replace_overwrites_value_and_timestamp`
- `touch_updates_timestamp_without_changing_value`
- `async_keyed_locks_serialize_concurrent_callers`（max_concurrent=1 锁定）
- `async_keyed_locks_different_keys_run_concurrently`（max_concurrent≥2 验证）

---

## 📐 2. env_helpers 模块

### 公开 API

```rust
pub fn default_path_for_platform() -> String;

pub fn ensure_path_in_env(env: &mut BTreeMap<String, String>) -> &mut BTreeMap<String, String>;

pub fn resolve_runtime_env(env: BTreeMap<String, String>) -> BTreeMap<String, String>;
```

### 与 Node 的对位

| Node 函数 | Rust 函数 | 语义 |
|---|---|---|
| `defaultPathForPlatform()` | `default_path_for_platform()` | 平台默认 PATH 字符串 |
| `ensurePathInEnv(env)` | `ensure_path_in_env(&mut env)` | 缺 PATH 时补,已有不动 |
| `resolveRuntimeEnv(env)` | `resolve_runtime_env(env)` | process env + caller env + ensure PATH |

### 关键设计

- **platform cfg 切换**:Windows 用 `;`-分隔,Unix 用 `:`-分隔
- **Windows 大小写兼容**:同时检查 `PATH` 和 `Path`(Node 行为一致)
- **空字符串视作缺失**:`""` 也算 PATH 缺失,需要补
- **过程 env 用 `vars_os`**:UTF-8 过滤,非 UTF-8 key/value 静默跳过(与 Node 行为对齐)

### 单元测试覆盖（7 个）

- `ensure_path_does_not_overwrite_existing_path`
- `ensure_path_accepts_windows_cased_path`
- `ensure_path_inserts_default_when_missing`
- `ensure_path_ignores_empty_string`
- `default_path_is_non_empty_for_every_platform`
- `resolve_runtime_env_overlays_caller_on_process`
- `resolve_runtime_env_caller_overrides_process`

---

## 🔗 3. round368_cache_lifecycle.rs 集成测试（10 个）

**cache e2e**:
- `cache_evicts_stale_entries_and_fires_closer`
- `cache_preserves_timestamp_on_re_put`
- `cache_refreshes_timestamp_on_replace`
- `cache_touch_updates_timestamp_without_touching_value`

**lease e2e**:
- `lease_serializes_critical_section_for_same_key`
- `lease_lets_distinct_keys_run_concurrently`

**env e2e**:
- `ensure_path_inserts_default_when_caller_omits_path`
- `resolve_runtime_env_overrides_process_path_when_caller_provides_one`
- `resolve_runtime_env_inserts_default_when_neither_layer_has_path`

**跨模块**:
- `cache_stores_resolved_env_for_replay`（env + cache 组合契约）

---

## 🔁 总累计基线

| 模块 | R362 | R363 | R364 | R365 | R366 | R367 | **R368** |
|---|---|---|---|---|---|---|---|
| ... 之前的累计 ... | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| **cache** | | | | | | | **NEW** |
| **env_helpers** | | | | | | | **NEW** |
| **pc-acpx 测试总数** | 47 | 66 | 90 | 105 | 155 | 202 | **227** |
| **总累计** | 975 | 994 | 1018 | 1042 | 1171 | 1218 | **1243** |

---

## ✅ 完成度更新

| 模块 | R368 完成度 |
|---|---|
| **acpx-engine 子模块** | **98%** (+1%, R368) |
| **后端核心** (pc-heartbeat + pc-repos + pc-core) | 96% |
| **完整后端** (含 adapters + plugins) | ~79% |
| **最大剩余缺口** | 真实 `SubprocessAcpRuntime` (R369+) |

---

## 🎯 R369+ 候选

1. **R369-370**:真实 `SubprocessAcpRuntime` 实现 — spawn acpx 子进程并 wire stdin/stdout/stderr JSON-RPC,~3-4 轮
2. **R371+**:Budgets 完整迁移（B2）— 计费/限流模块,~3-4 轮

