# R389 — Skill Materialize (Node-faithful rewrite)

## 目标

按 `comet-open` + `RTK` 思路,**完全重写** `materializePaperclipSkillCopy`
(Node L3038-3120) 与所有相关内部 helpers(`hashSkillDirectory` L2920-2966
/ `materializedSkillFingerprintMatches` L2968-2976 /
`acquireMaterializeLock` L2978-3000 /
`removeStaleMaterializeLock` L3003-3026 / `isPidAlive` L3006-3013),
精确镜像 Node 语义。**修复 root cause**:现有 R367 实施是简化版
(`source == target → no-op`,无 fingerprint cache / lock / sentinel),
与 Node 行为偏差较大,本次彻底重写。

## 范围

- 重写 `crates/pc-acpx/src/skill_materialize.rs` 中的
  `materialize_paperclip_skill_copy` 函数体(零行为变更点保留)
- 新增 6 个公开 helper + 3 个常量 + 4 个错误变体
- 新增 12 个单测 + 1 个修改测试(`materialize_rejects_self_reference`)
- 新增 `crates/pc-acpx/tests/round389_skill_materialize_lock.rs`(412 行 19 集成测试)
- 新增 `AcpxError::MaterializeSelfReference` / `MaterializeSymlinkRoot`
  / `MaterializeNotDirectory` / `MaterializeLockTimeout` 错误变体
- 更新 `crates/pc-acpx/src/lib.rs` 的 skill_materialize re-export
- 跟 Node `paperclip/packages/adapter-utils/src/server-utils.ts`
  L129-131 / L2920-3120 行精确对齐

## Node 函数 / 常量 / 类型

### 常量(完全镜像 L129-131)
```rust
pub const MATERIALIZED_SKILL_SENTINEL: &str = ".paperclip-materialized-skill.json";
pub const MATERIALIZED_SKILL_LOCK_OWNER: &str = "owner.json";
pub const MATERIALIZED_SKILL_LOCK_STALE_MS: u64 = 30_000;
```

### 重写的主函数
`materialize_paperclip_skill_copy(source, target) -> Result<MaterializedSkillCopyResult, AcpxError>`

### 新增 helper
- `hash_skill_directory(root: &Path) -> Result<String, AcpxError>`(L2920-2966,byte-for-byte 镜像 Node `createHash("sha256")` 序列)
- `materialized_skill_fingerprint_matches(target_root, fingerprint) -> bool`(L2968-2976)
- `acquire_materialize_lock(lock_dir) -> Result<Box<dyn FnOnce() -> BoxFuture<...>>>`(L2978-3000)
- `remove_stale_materialize_lock(lock_dir, stale_ms) -> bool`(L3003-3026)
- `is_pid_alive(pid: u32) -> bool`(L3006-3013 — 与 `log_redaction::is_pid_alive` 共存,语义一致)
- `random_uuid_string() -> String`(L3073 镜像 Node `randomUUID()`)

### 4 个错误变体(精确镜像 Node 错误文本)
- `MaterializeSelfReference { source_path, target_path }` — "Refusing to materialize a skill into itself, an ancestor, or one of its descendants."
- `MaterializeSymlinkRoot { path }` — "Refusing to materialize a skill root that is itself a symlink."
- `MaterializeNotDirectory { path }` — "Paperclip skills must be directories."
- `MaterializeLockTimeout { lock_dir }` — "Timed out waiting for Paperclip skill materialization lock at <lock_dir>."

## Node 语义完整镜像

### Self / ancestor / descendant 拒绝(L3053)
原 R367 简化版:`source_resolved == target_resolved` 时返回 no-op。
Node:`source == target` → `source` 是 `target` 的 ancestor → `target` 是 `source` 的 ancestor,**全部抛错**。

新实现使用 lexical `pathdiff`:
```rust
let relative_target = pathdiff(&source_root, &target_root);
let relative_source = pathdiff(&target_root, &source_root);
if same_path || relative_target.is_some() || relative_source.is_some() {
    return Err(AcpxError::MaterializeSelfReference { ... });
}
```

### Root symlink / 非目录拒绝(L3056, L3059)
原 R367 简化版:接受任何 `fs::copy` 走得通的输入。
Node:用 `lstat` 拒绝 symlink 根 + 非目录根。

### Fingerprint 缓存(L2968-2976 + L3089)
原 R367 完全没有缓存。
Node:每个 materialize 先计算源 SHA-256 fingerprint,若目标 `.paperclip-materialized-skill.json` 记录的 fingerprint 相同 → 直接返回结果(零拷贝)。

新实现:写入 sentinel 包含 `{ version: 1, sourceFingerprint, copiedFiles, skippedSymlinks }`,重复调用直接 cache hit。

### Materialize lock + stale recovery(L2978-3000, L3003-3026)
原 R367 完全没有锁。
Node:在 `<target>.lock` 目录互斥锁,30 秒 stale 阈值。stale 检测通过:
1. 读 `owner.json` 中的 `pid` + `createdAt`
2. PID 死亡(`kill -0` 返回非零)或 age 超阈值 → 删除锁
3. 否则继续等待 / 重试

新实现:用 `mkdir` 原子创建锁目录 + 写 owner.json。`is_pid_alive` 通过 shell `kill -0` 探测(满足 `unsafe_code = "forbid"`)。

### 临时目录 + 原子 rename(L3073-3118)
原 R367 直接覆盖 target。
Node:写入 `<target>.tmp-<pid>-<uuid>`,写完 sentinel 后检查 fingerprint(防止竞态),最后 `rename` 到 target。

新实现:temp dir 命名严格镜像 Node 格式,`rename` 失败时清理 temp_root。

### 哈希格式 byte-for-byte 镜像(L2920-2966)
Node 哈希序列:
- `symlink:<rel>\n` (不跟随)
- `dir:<rel>\n` (递归 sorted by localeCompare)
- `file:<rel>:<mode>\n` + 内容 + `\n`
- `other:<rel>:<mode>\n`

新实现精确镜像,包括 `mode` 字段。

### File mode 保留(L3098)
新实现使用 `tokio::fs::set_permissions` 在每次文件复制后恢复 mode(unix only)。

## 关键设计决策

### `Box<dyn FnOnce() -> BoxFuture>` lock release
Node 的 `releaseLock` 是 closure 捕获 `lockDir`,在 `finally` 块中调用。
Rust 等价:返回一个 boxed closure,捕获 `lock_path_for_release` clone,
在 `finally` 块中 `release_lock().await`。`unsafe_code = "forbid"` 完全兼容。

### `chrono_like_parse_ms` 内联实现
避免引入 `chrono` 或 `time` crate;Node `Date.parse()` 接受 ISO8601,
最小化 inline parser 处理 `YYYY-MM-DDTHH:MM:SS.sssZ` 格式已足够。

### ISO8601 字符串生成
`now_iso8601()` 用 Howard Hinnant 的 `civil_from_days` 算法做 unix → civil date 转换,纯整数算术,无需外部时间库。

### `random_uuid_string` fallback
通过 `/dev/urandom` 16 字节 + time + pid 一起 SHA-256,生成 32 字符 hex。
Node 使用 Web Crypto `randomUUID()`(RFC4122 v4),两者都是加密随机,hash 版本保证 deterministic,UUID 版本保证标准兼容。

### `path_clean` + `pathdiff` 纯字符串算术
避免文件系统 I/O,仅做 lexical 路径解析,与 Node `path.resolve` / `path.relative` 语义一致。

### `unsafe_code = "forbid"` 兼容
全部使用 `tokio::fs::*` + shell-out `kill -0` + pure string arithmetic。

## 测试

### 单测(20 个,8 旧 + 12 新)
- 8 个旧测试,1 个被替换(`materialize_returns_self_when_source_equals_target` → `materialize_rejects_self_reference`)
- 12 个新测试覆盖:
  - ancestor target 拒绝(L3053)
  - descendant source 拒绝(L3053)
  - symlink root 拒绝(L3056)
  - 非目录 root 拒绝(L3059)
  - sentinel 写入 + 验证
  - cache hit(重复调用 0 拷贝)
  - cache invalidate(源变更后重新拷贝)
  - `hash_skill_directory` deterministic
  - `hash_skill_directory` 内容敏感
  - `materialized_skill_fingerprint_matches` 缺失 sentinel → false
  - `materialized_skill_fingerprint_matches` 版本不匹配 → false
  - `is_pid_alive` 0/self/MAX 行为

### 集成测试(19 个)
- 3 个常量字面量验证
- 4 个 materialize 拒绝语义(self / ancestor / descendant / symlink root / non-directory root)
- 2 个 cache 行为(repeated call short-circuit / mutation invalidates)
- 1 个 sentinel 内容验证(version + fingerprint + copiedFiles)
- 1 个 file mode 保留验证(unix only)
- 1 个外部 symlink drop 验证
- 2 个 `hash_skill_directory` 行为(deterministic + order invariant)
- 3 个 `materialized_skill_fingerprint_matches`(missing / version mismatch / match)
- 3 个 stale lock 行为(dead pid / live pid / old age)
- 1 个 `is_pid_alive` 边界
- 2 个 date helpers(`chrono_now_iso` + `chrono_now_iso_offset`)

合计 R389 新增 **31 个测试**(12 单测 + 19 集成测试),全部绿色。

## 验证

```
cd paperclip-rs && cargo test -p pc-acpx
```

结果:**773 个 pc-acpx tests 通过** (R388 是 742,+31),0 失败 0 回归。

```
cd paperclip-rs && cargo fmt --check
```

clean。

## 下一步

adapter-utils 中 R380-R389 共 10 轮、约 100 个函数已**完整移植到 Rust**。
剩余模块:

### R390+ 候选(按 docs/38-MODULE-GAP-AUDIT.md 路线)
- **P0**: heartbeat 依赖 readiness + staleness recovery
- **P1**: company-skills 深度 / tools OAuth / plugin worker→host 回调
- **P2**: folders / labels / routines / pipelines 完整迁移
- **Adapter 充实**: 13 个 adapter stubs 实质实现
- **Secrets 真实解密**: AWS / GCP / Vault
