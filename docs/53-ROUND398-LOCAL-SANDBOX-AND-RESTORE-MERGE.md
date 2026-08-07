# R398 — Local Process Sandbox + Workspace Restore Merge (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapter-utils/src/` 中两个
adapter-utils 模块完整移植到 `crates/pc-acpx/src/`:

1. `local-process-sandbox.ts` (509 行) → `local_process_sandbox.rs` (402 行)
   - **纯解析部分 5 个函数**完整移植
   - **async `buildLocalProcessSandboxSpawnTarget` 未移植** (需 bwrap + 平台特定)
2. `workspace-restore-merge.ts` (259 行) → `workspace_restore_merge.rs` (602 行)
   - **完整移植**所有 5 个导出函数

## Node 函数 / 类型映射

### local-process-sandbox.ts

| Node | Rust | 说明 |
|---|---|---|
| `LocalProcessSandboxAccess` ("ro" \| "rw") | `LocalProcessSandboxAccess` (enum) | `Ro` / `Rw` |
| `LocalProcessNetworkScope` ("deny" \| "allowlist") | `LocalProcessNetworkScope` (enum) | `Deny` / `Allowlist` |
| `LocalProcessSandboxPath` (interface) | `LocalProcessSandboxPath` (struct) | `path` / `access` |
| `LocalProcessSandboxPathAlias` (interface) | `LocalProcessSandboxPathAlias` (struct) | `path` / `target` |
| `LocalProcessSandboxOptions` (interface) | `LocalProcessSandboxOptions` (struct) | 完整 9 字段镜像 |
| `LocalProcessSandboxSpawnTarget` (interface) | `LocalProcessSandboxSpawnTarget` (struct) | 4 字段镜像 |
| `parseLocalProcessNetworkAllowlist` | `parse_local_process_network_allowlist` | URL 解析 + 主机名校正 + 通配符拒绝 |
| `parseLocalProcessNetworkScope` | `parse_local_process_network_scope` | null/空 → None; invalid → None |
| `parseLocalProcessFilesystemScope` | `parse_local_process_filesystem_scope` | null/空 → None |
| `parseLocalProcessSandboxExtraPaths` | `parse_local_process_sandbox_extra_paths` | 支持 string 或 `{ path, access }` |
| `buildLocalProcessSandboxSpawnTarget` | ❌ 未移植 | 需 bwrap + 平台特定 (Unix-only) |

### workspace-restore-merge.ts

| Node | Rust | 说明 |
|---|---|---|
| `SnapshotEntry` (union type) | `SnapshotEntry` (enum, tagged) | `Dir` / `File{mode,hash}` / `Symlink{target}` |
| `DirectorySnapshot` (interface) | `DirectorySnapshot` (struct) | `exclude` + `entries: BTreeMap` |
| `hashFile` (internal) | `hash_file` (pub) | sha256 via `sha2` crate |
| `walkDirectory` (internal) | `walk_directory` (private) | async tokio::fs |
| `readSnapshotEntry` (internal) | `read_snapshot_entry` (pub) | async tokio::fs |
| `entriesMatch` (internal) | `entries_match` (pub) | full equality |
| `acquireDirectoryMergeLock` (internal) | `acquire_merge_lock` (private) | async with stale detection |
| `withDirectoryMergeLock` | `with_directory_merge_lock` | generic T, 30s deadline |
| `captureDirectorySnapshot` | `capture_directory_snapshot` | 完整 mirror |
| `copySnapshotEntry` (internal) | `copy_snapshot_entry` (private) | async, mode-preserving |
| `mergeDirectoryWithBaseline` | `merge_directory_with_baseline` | 完整 mirror (3-phase: delete leaves → delete dirs → copy changed) |
| `directoryEntryMatchesBaseline` | `directory_entry_matches_baseline` | 完整 mirror |

## 关键设计决策

### 1. local-process-sandbox: async spawn builder 延后
Node `buildLocalProcessSandboxSpawnTarget` 需要 `bubblewrap` (`bwrap`),
`node:net`, `node:http` proxy, Unix socket 等 — 平台特定且复杂。
本次仅移植**纯解析部分**(5 个函数),async spawn builder 作为后续
R400+ 任务 (依赖 `command-managed-runtime` 等)。

### 2. workspace-restore-merge: 完整 async 移植
所有 I/O 使用 `tokio::fs`,hash 使用 `sha2` crate。`Map<string, SnapshotEntry>`
→ `BTreeMap` (保持确定性迭代顺序)。`permissions_to_mode` 提取 `0o7777`
(掩码掉文件类型位)以与 Node `stats.mode` 语义对齐。

### 3. Stale-lock detection 复用现有 `is_pid_alive`
复用 `pc_acpx::log_redaction::is_pid_alive` (R384 实现),避免引入 `libc`
依赖。

### 4. SnapshotEntry tagged enum
使用 `#[serde(tag = "kind", rename_all = "snake_case")]` 让 enum 可以
与 Node JSON 序列化兼容 (`{"kind":"file","mode":420,"hash":"..."}`),
便于未来跨语言互操作。

## 单元测试

### local_process_sandbox: 23 tests

- `access_enum_round_trips` — as_str / Display
- `parse_network_allowlist_*` (8 tests) — 空数组 / 非数组 / hostname / hostname:port / origin URL / 大小写 / 通配符拒绝 / 空字符串拒绝 / 非 string 拒绝
- `parse_network_scope_*` (5 tests) — deny / allowlist / null / empty / invalid
- `parse_filesystem_scope_*` (3 tests) — workspace / null / empty
- `parse_extra_paths_*` (5 tests) — string array / object array with access / relative path 拒绝 / 非数组 / 无效 object 跳过

### workspace_restore_merge: 9 tests

- `entries_match_handles_all_combinations` — Dir/File/Symlink × null/equal/diff
- `hash_file_computes_sha256` — sha256("hello world") 已知值
- `capture_snapshot_returns_entries_for_simple_tree` — 文件/子目录递归
- `capture_snapshot_respects_exclude` — node_modules 排除
- `read_snapshot_entry_returns_none_for_missing` — missing → None
- `read_snapshot_entry_returns_file_entry` — sha256("hello") 已知值
- `directory_entry_matches_baseline_true` — 一致 → true
- `directory_entry_matches_baseline_false_when_changed` — 不一致 → false
- `merge_directory_with_baseline_copies_new_files` — 新文件被复制

## 集成测试 (round398): 7 tests

### local-process-sandbox 集成
- `sandbox_full_config_parsing` — 完整 JSON config 全字段解析
- `sandbox_empty_config_returns_defaults` — 空 config 全空

### workspace-restore-merge 集成
- `merge_round_trip_full_lifecycle` — baseline → source → target 完整生命周期
- `merge_preserves_target_local_files` — target 本地文件保留
- `directory_entry_matches_baseline_works_for_changed_files` — 变更检测
- `capture_snapshot_excludes_node_modules` — 多模式排除
- `merge_handles_before_and_after_callbacks` — before/after 回调触发

## 验证结果

```bash
cargo test -p pc-acpx --lib -- local_process_sandbox
# 23 passed

cargo test -p pc-acpx --lib -- workspace_restore_merge
# 9 passed

cargo test -p pc-acpx --test round398_local_sandbox_and_restore_merge
# 7 passed

cargo test -p pc-acpx --lib
# 559 passed (up from 527, +32)
```

## 与 Node parity 检查

| Node export | Rust | Unit | Integration |
|---|---|---|---|
| `LocalProcessSandboxAccess` | ✓ | 1 | — |
| `LocalProcessNetworkScope` | ✓ | — | 1 |
| `LocalProcessSandboxPath` | ✓ | — | — |
| `LocalProcessSandboxPathAlias` | ✓ | — | — |
| `LocalProcessSandboxOptions` | ✓ | — | — |
| `LocalProcessSandboxSpawnTarget` | ✓ | — | — |
| `parseLocalProcessNetworkAllowlist` | ✓ | 8 | 1 |
| `parseLocalProcessNetworkScope` | ✓ | 5 | 1 |
| `parseLocalProcessFilesystemScope` | ✓ | 3 | 1 |
| `parseLocalProcessSandboxExtraPaths` | ✓ | 5 | 1 |
| `buildLocalProcessSandboxSpawnTarget` | ❌ 延后 | — | — |
| `SnapshotEntry` | ✓ | — | — |
| `DirectorySnapshot` | ✓ | — | 1 |
| `hashFile` | ✓ | 1 | — |
| `readSnapshotEntry` | ✓ | 2 | — |
| `entriesMatch` | ✓ | 1 | — |
| `withDirectoryMergeLock` | ✓ | — | — |
| `captureDirectorySnapshot` | ✓ | 2 | 1 |
| `mergeDirectoryWithBaseline` | ✓ | 1 | 3 |
| `directoryEntryMatchesBaseline` | ✓ | 2 | 1 |

**Local-process-sandbox: 5/6 exports (83% Node parity, async builder 延后)**
**Workspace-restore-merge: 100% Node parity**

## 累计进度

| 状态 | 模块 | Node 行数 | Rust 行数 |
|---|---|---|---|
| ✅ R396 | billing | 20 | 151 |
| ✅ R396 | exclude-patterns | 28 | 145 |
| ✅ R396 | sandbox-shell | 7 | 73 |
| ✅ R396 | command-redaction | 58 | 217 |
| ✅ R396 | remote-execution-env | 49 | 169 |
| ✅ R396 | sandbox-install-command | 46 | 126 |
| ✅ R397 | runtime-progress | 170 | 496 |
| ✅ R397 | session-compaction | 187 | 498 |
| ✅ R398 | local-process-sandbox | 509 (部分) | 402 |
| ✅ R398 | workspace-restore-merge | 259 | 602 |
| **小计** | **10 个模块** | **1333** | **2879** |

**pc-acpx 累计模块数**: 54 → 56 (+2)
**pc-acpx 累计 lib 测试**: 527 → 559 (+32)
**pc-acpx 累计集成测试文件**: 32 → 33 (+1)

## 下一步 (R399+ 候选)

- **R399**: `git-workspace-sync.rs` (433 行) + `remote-managed-runtime.rs` (239 行)
- **R400**: `command-managed-runtime.rs` (570 行) + `sandbox-callback-bridge.rs` (1262 行)
- **R401**: `sandbox-managed-runtime.rs` (1224 行)
- **R402**: `execution-target.ts` (1877 行) — 最大模块
- **R403**: `ssh.ts` (1862 行)
