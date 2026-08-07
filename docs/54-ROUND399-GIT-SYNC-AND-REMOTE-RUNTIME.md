# R399 — Git Workspace Sync + Remote Managed Runtime (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapter-utils/src/` 中两个
adapter-utils 模块完整移植到 `crates/pc-acpx/src/`:

1. `git-workspace-sync.ts` (433 行) → `git_workspace_sync.rs` (304 行)
   - **纯函数部分 5 个函数**完整移植 (constant + 4 functions)
   - **async `git` CLI 执行部分延后** (需实际 git 命令)
2. `remote-managed-runtime.ts` (239 行) → `remote_managed_runtime.rs` (332 行)
   - **纯函数部分 4 个函数**完整移植
   - **async staging 部分延后** (依赖 `ssh.ts` 未移植)

## Node 函数 / 类型映射

### git-workspace-sync.ts

| Node | Rust | 说明 |
|---|---|---|
| `GitCommandResult` (interface) | ❌ 延后 | 需实际 git exec |
| `GitWorkspaceSnapshot` (interface) | ❌ 延后 | 需实际 git exec |
| `GIT_ARCHIVE_EXCLUDES` (const) | `GIT_ARCHIVE_EXCLUDES` (`&[&str]`) | `[".git", ".git/*"]` |
| `GIT_MISSING_PREREQUISITE_MARKERS` (internal) | `GIT_MISSING_PREREQUISITE_MARKERS` (pub) | 3 个 marker |
| `shellQuote` (internal) | `shell_quote` (private) | POSIX single-quote |
| `runLocalGit` | ❌ 延后 | 需实际 git exec |
| `readGitWorkspaceSnapshot` | ❌ 延后 | 需实际 git exec |
| `withShallowGitWorkspaceClone` | ❌ 延后 | 需实际 git exec |
| `createImportedGitRef` | `create_imported_git_ref` | uuid-based ref name |
| `createRemoteGitExportRef` | `create_remote_git_export_ref` | uuid-based ref name |
| `deleteLocalGitRef` | ❌ 延后 | 需实际 git exec |
| `fetchGitBundleIntoLocalRef` | ❌ 延后 | 需实际 git exec |
| `isMissingGitPrerequisiteError` | `is_missing_git_prerequisite_error` | marker substring check |
| `buildRemoteGitDeltaBundleScript` | `build_remote_git_delta_bundle_script` | shell script builder |
| `integrateImportedGitHead` | ❌ 延后 | 需实际 git exec |
| `resetLocalGitIndexToHead` | ❌ 延后 | 需实际 git exec |

### remote-managed-runtime.ts

| Node | Rust | 说明 |
|---|---|---|
| `RemoteManagedRuntimeAsset` (interface) | 延后 | 需 `SandboxManagedRuntimeAssetRestoreContext` |
| `PreparedRemoteManagedRuntime` (interface) | 延后 | 需 async staging |
| `SshRemoteExecutionSpec` (from ssh.ts) | `SshRemoteExecutionSpec` (本地) | 4 字段镜像 |
| `buildRemoteExecutionSessionIdentity` | `build_remote_execution_session_identity` | identity struct |
| `remoteExecutionSessionMatches` | `remote_execution_session_matches` | 5 字段比较 |
| `prepareRemoteManagedRuntime` | ❌ 延后 | 需 async staging + ssh |
| `REMOTE_ADDITIONAL_SOURCE_HEAVY_DIR_EXCLUDES` (internal) | `REMOTE_ADDITIONAL_SOURCE_HEAVY_DIRS` + `expand_heavy_dir_excludes` | 10 base × 4 shapes |
| 路径计算 (workspaceRemoteDir / runtimeRootDir / assetDirs) | `resolve_run_workspace_remote_dir` / `resolve_runtime_root_dir` / `resolve_asset_remote_dir` | 纯路径构建 |
| 排除列表 (git_backed / non_git_backed) | `git_backed_workspace_excludes` / `non_git_backed_workspace_excludes` | 完整 mirror |

## 关键设计决策

### 1. git-workspace-sync: async git CLI 延后
所有调用 `runLocalGit` 的函数 (7 个) 需要实际 git CLI 执行,需要
`tokio::process::Command` + git 二进制。本次仅移植:
- 常量 + ref-name 生成 (2 个)
- 错误分类器 (1 个)
- shell 脚本构建器 (1 个)

后续 R402+ 可基于 `tokio::process::Command` 完整实现 async 部分。

### 2. remote-managed-runtime: async staging 延后
`prepareRemoteManagedRuntime` 依赖 `ssh.ts` 中的 `prepareWorkspaceForSshExecution`、
`syncDirectoryToSsh` 等。本次移植:
- SSH spec 镜像 (本地结构体)
- session identity 构造与比较
- 路径构建 helpers (3 个)
- 排除列表生成 (2 个)
- 重目录排除扩展 (1 个)

### 3. SshRemoteExecutionSpec 本地镜像
由于 `ssh.ts` 未移植,本地定义最小化的 `SshRemoteExecutionSpec` 结构体
(4 字段)。后续 R403 移植 `ssh.ts` 后,可统一引用。

### 4. exclude_pattern_matches 复用
复用 R396 的 `crate::exclude_patterns::exclude_pattern_matches`,避免
重复实现 glob 匹配逻辑。

## 单元测试

### git_workspace_sync: 11 tests

- `git_archive_excludes_constant` — 值校验
- `create_imported_git_ref_uses_scope_and_uuid` — 格式 + UUID 长度
- `create_remote_git_export_ref_uses_scope_and_uuid` — 格式 + UUID 长度
- `refs_are_unique_across_calls` — UUID 唯一性
- `is_missing_prerequisite_detects_known_markers` — 3 个 marker + 负例
- `is_missing_prerequisite_anyhow_wrapper` — string 版本
- `build_delta_bundle_script_basic` — 基本字段都在
- `build_delta_bundle_script_force_full` — force_full 跳过 merge-base
- `build_delta_bundle_script_with_status_path` — cleanup + status + cat
- `shell_quote_escapes_single_quotes` — `'s` → `'\"'\"'s'`

### remote_managed_runtime: 16 tests

- `ssh_spec_construction` — 基本构造
- `expand_heavy_dir_excludes_generates_four_shapes_per_base` — 10×4=40 patterns
- `resolve_runtime_root_dir_appends_paperclip_runtime_and_adapter` — 路径拼接
- `resolve_run_workspace_remote_dir_builds_per_run_path` — per-run 路径
- `resolve_asset_remote_dir_nests_under_runtime_root` — asset 路径
- `session_identity_is_built_from_spec` — 5 字段
- `session_identity_is_none_for_none_spec` — None spec → None identity
- `session_matches_with_equal_spec` — 完全匹配 → true
- `session_mismatch_on_different_host` — host 不同 → false
- `session_mismatch_on_different_port` — port 不同 → false
- `session_mismatch_on_different_cwd` — cwd 不同 → false
- `session_match_returns_false_when_current_is_none` — None current → false
- `session_match_returns_false_for_non_object_saved` — 非对象 → false
- `session_match_handles_missing_port` — null port 兼容
- `should_exclude_heavy_dir_matches_node_modules` — 4 种 node_modules 形态
- `git_backed_workspace_excludes_includes_git_and_paperclip_runtime` — 包含 .git + .paperclip-runtime
- `non_git_backed_workspace_excludes_only_paperclip_runtime` — 仅 .paperclip-runtime

## 集成测试 (round399): 8 tests

### git_workspace_sync 集成
- `git_refs_have_correct_format_and_uniqueness` — 4 个 ref 类型
- `is_missing_prerequisite_classifies_known_errors` — 3 marker + 1 负例
- `build_delta_bundle_produces_valid_shell` — 完整字段验证
- `build_delta_bundle_force_full_skips_merge_base` — force_full 行为

### remote_managed_runtime 集成
- `remote_runtime_path_layout` — 完整路径层级 (workspace → runtime_root → asset)
- `remote_session_identity_round_trip` — identity 序列化 → 比较
- `heavy_dir_excludes_block_node_modules_in_nested_paths` — 4 种嵌套形态
- `workspace_exclude_lists_are_consistent` — git_backed + heavy 排除

## 验证结果

```bash
cargo test -p pc-acpx --lib -- git_workspace_sync
# 11 passed

cargo test -p pc-acpx --lib -- remote_managed_runtime
# 16 passed

cargo test -p pc-acpx --test round399_git_sync_and_remote_runtime
# 8 passed

cargo test -p pc-acpx --lib
# 586 passed (up from 559, +27)
```

## 与 Node parity 检查

### git-workspace-sync
| Node export | Rust | Unit | Integration |
|---|---|---|---|
| `GIT_ARCHIVE_EXCLUDES` | ✓ | 1 | — |
| `GIT_MISSING_PREREQUISITE_MARKERS` | ✓ (pub) | — | — |
| `createImportedGitRef` | ✓ | 3 | 1 |
| `createRemoteGitExportRef` | ✓ | 1 | (via refs test) |
| `isMissingGitPrerequisiteError` | ✓ | 2 | 1 |
| `buildRemoteGitDeltaBundleScript` | ✓ | 3 | 2 |
| 7 个 async 函数 | ❌ 延后 | — | — |

**Git-workspace-sync: 6/13 exports (46% Node parity, async 部分延后)**

### remote-managed-runtime
| Node export | Rust | Unit | Integration |
|---|---|---|---|
| `SshRemoteExecutionSpec` (local mirror) | ✓ | 1 | — |
| `buildRemoteExecutionSessionIdentity` | ✓ | 2 | 1 |
| `remoteExecutionSessionMatches` | ✓ | 7 | (via identity test) |
| 路径 helpers (3 个) | ✓ | 3 | 1 |
| 排除列表生成 (3 个) | ✓ | 2 | 1 |
| `prepareRemoteManagedRuntime` | ❌ 延后 | — | — |

**Remote-managed-runtime: 5/6 exports (83% Node parity, async 部分延后)**

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
| ✅ R399 | git-workspace-sync | 433 (部分) | 304 |
| ✅ R399 | remote-managed-runtime | 239 (部分) | 332 |
| **小计** | **12 个模块** | **2005** | **3515** |

**pc-acpx 累计模块数**: 56 → 58 (+2)
**pc-acpx 累计 lib 测试**: 559 → 586 (+27)
**pc-acpx 累计集成测试文件**: 33 → 34 (+1)

## 下一步 (R400+ 候选)

- **R400**: `command-managed-runtime.rs` (570 行) — 部分依赖 ssh
- **R401**: `sandbox-callback-bridge.rs` (1262 行) — bridge 协议
- **R402**: `sandbox-managed-runtime.rs` (1224 行) — sandbox 生命周期
- **R403**: `execution-target.ts` (1877 行) — 最大模块
- **R404**: `ssh.ts` (1862 行)
