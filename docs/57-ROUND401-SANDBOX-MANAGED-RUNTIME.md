# R401 — Sandbox Managed Runtime (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapter-utils/src/` 中
最大单核心 sandbox 模块完整移植到 `crates/pc-acpx/src/`:

- `sandbox-managed-runtime.ts` (1224 行) → `sandbox_managed_runtime.rs`
  (1108 行)
- **纯函数部分 13 个函数 + 14 个类型 + 1 组常量**完整移植
- **async 部分延后** (FS / tar CLI / ssh runtime / bubblewrap plumbing)

## Node 函数 / 类型映射

### 延后 (async / FS / process exec)

| Node | Rust | 原因 |
|---|---|---|
| `createTarballFromDirectory` | ❌ 延后 | 需 `tar` CLI + FS access |
| `mirrorDirectory` | ❌ 延后 | 需 FS access |
| `extractTarballToDirectory` | ❌ 延后 | 需 FS access |
| `walkDirectory` / `copyWorkspaceEntry` / `copySelectedWorkspaceEntries` | ❌ 延后 | 需 FS access |
| `prepareSandboxManagedRuntime` | ❌ 延后 | 需 ssh runner + sandbox runtime |
| `withTempDir` / `execTar` / `emitRuntimeStatus` | ❌ 延后 | 需 `fs.mkdtemp` / process exec / async sink |
| `SandboxManagedRuntimeClient` (全方法) | ⏸ 类型已镜像,方法体延后 | 需 async runtime |
| `PreparedSandboxManagedRuntime.restoreWorkspace` | ⏸ 类型已镜像,异步体延后 | 需 async runtime |

### 已移植 (纯函数 / types)

| Node | Rust | 说明 |
|---|---|---|
| `SandboxRemoteExecutionSpec` | `SandboxRemoteExecutionSpec` | full 移植 |
| `SandboxManagedRuntimeAssetProvisionContext` | `SandboxManagedRuntimeAssetProvisionContext` | full |
| `SandboxManagedRuntimeStageFile` (新) | `SandboxManagedRuntimeStageFile` | Rust 把 stage files 转为 `{name, contents: String}`,byte buffer 随 async tar 一起延后 |
| `SandboxManagedRuntimeAssetProvision` | `SandboxManagedRuntimeAssetProvision` | builder 函数降级为预计算的 command 字段 |
| `SandboxManagedRuntimeAssetProvisionPostUploadCommand` (新) | `SandboxManagedRuntimeAssetProvisionPostUploadCommand` | marker 类型记录已构建的 shell 命令 |
| `SandboxManagedRuntimeAssetRestoreContext` | `SandboxManagedRuntimeAssetRestoreContext` | async `readFile` 降级为 `has_read_file: bool` flag |
| `SandboxManagedRuntimeAsset` | `SandboxManagedRuntimeAsset` | async `restore` builder 降级为 `has_restore: bool` flag |
| `SandboxAdditionalSource` | `SandboxAdditionalSource` | full |
| `SandboxTransferProgressOptions` | `SandboxTransferProgressOptions` | async `onProgress` 降级为 `has_on_progress: bool` |
| `SandboxSyncFileMapping` | `SandboxSyncFileMapping` | full,加 `access_or_default()` 默认 "ro" |
| `SandboxPostUploadCommand` | `SandboxPostUploadCommand` | full |
| `SandboxSyncOperation` | `SandboxSyncOperation` | full,加 `post_upload_commands_or_empty()` 把 None 当空 slice |
| `SandboxSyncResult` | `SandboxSyncResult` (+ `SandboxSyncResultOperation`) | full |
| `SandboxManagedRuntimeClient` | `SandboxManagedRuntimeClient` | async methods 降级为 `has_native_sync_in/out: bool` flag |
| `PreparedSandboxManagedRuntime` | `PreparedSandboxManagedRuntime` | async `restoreWorkspace` 降级为 `has_restore_workspace` flag |
| `AdditionalSourceStagingFailure` | `AdditionalSourceStagingFailure` | full |
| **常量** `SANDBOX_WORKSPACE_HEAVY_DIR_NAMES` | 同名 (`&[&str]`) | 9 个 dir name |
| **派生** `SANDBOX_WORKSPACE_HEAVY_DIR_EXCLUDES` | `sandbox_workspace_heavy_dir_excludes()` 函数 | 运行时展开 4 pattern/name |
| `asObject` / `asString` / `asNumber` | `as_object` / `as_string` / `as_number` | serde_json::Value 适配 |
| `shellQuote` | `shell_quote` (private) | POSIX single-quote |
| `posixIsAbsolute` / `posixNormalize` | `posix_is_absolute` / `posix_normalize` | 纯字符串处理 |
| `buildUniqueStagingPath` | `build_unique_staging_path` | UUID v4 后缀 |
| `mergeExcludes` | `merge_excludes` | BTreeSet 去重保序 |
| `preserveFindArgs` | `preserve_find_args` | 转 `! -name 'x'` 前缀 |
| `tarExcludeFlags` | `tar_exclude_flags` | 自动前置 `._*` (Mac resource fork) |
| `parseSandboxRemoteExecutionSpec` | `parse_sandbox_remote_execution_spec` | full 移植 |
| `buildSandboxExecutionSessionIdentity` | `build_sandbox_execution_session_identity` (+ `SandboxExecutionSessionIdentity`) | 4-tuple 简化 |
| `sandboxExecutionSessionMatches` | `sandbox_execution_session_matches` | 比对 transport/provider/sandboxId/remoteCwd |
| `assertSyncOperationsConfined` | `assert_sync_operations_confined` (+ `SyncConfinementRoots`) | **核心安全防护**:sourceRoots + targetRoots 双重校验 |
| `buildDefaultExtractRuntimeAssetCommand` | `build_default_extract_runtime_asset_command` | rm → mkdir → tar → rm sequence |
| `buildWorkspaceTarExtractCommand` | `build_workspace_tar_extract_command` | overlay 或 destroy-then-replace 两种形式 |
| `buildRemoveDeletedPathsCommand` | `build_remove_deleted_paths_command` | cd + rm -rf quoted paths |
| `createRemoteTarballFromDirectoryCommand` | `create_remote_tarball_from_directory_command` | mkdir + cd + glob dots + tar 全脚本 |

## 关键设计决策

1. **Async 部分降级为 bool flag, 不重写 behavior**:
   - `SandboxManagedRuntimeClient` 接口的所有 async method 降级为
     `has_native_sync_in: bool` / `has_native_sync_out: bool`,等真实 runtime
     接入时再补
   - `restore` builder / `readFile` 闭包 / `onProgress` 闭包 同样降级
   - 这种降级保留了**调用契约**(调用方知道 runtime 该有哪些能力),而不会
     把 async 行为错误地塞到 pure helper 里

2. **stage files 限定为 String**:
   - Node 原始支持 `Buffer | string`,先做 String 这条主线
   - Binary buffer 跟随 async tar runtime 一起实现

3. **confinement guard 严格 1:1 移植 Node 语义**:
   - 输入:operations + `SyncConfinementRoots { source_roots, target_roots }`
   - 两个拒绝分支:
     - 早 reject:normalized 是非绝对 / 等于 `".."` / 含 `"/../"` / 以 `"/.."` 结尾
     - 晚 reject:normalize 后不在任一 root 下
   - 关键测试 `confinement_rejects_dotdot_target_normalizes_then_escapes` 验证 normalize 后
     仍触发 "escapes" 分支
   - 关键测试 `confinement_rejects_relative_target_with_dotdot` 验证早分支

4. **tar exclude flags 一律前置 `._*`**:
   - Node 硬编码 Mac resource fork metadata 是 tar archive 中的常见噪音
   - Rust 实现 1:1 跟随,避免 Node 端拿到未过滤 tar 包

5. **`post_upload_commands: Option<Vec<...>>` → 默认空 slice**:
   - Node 接口允许字段 absent(以 byte-identical 方式等于空数组)
   - Rust 通过 `post_upload_commands_or_empty()` 方法保持一致语义

## 集成测试覆盖 (25 个)

### spec / identity (5 个)
- `heavy_dir_excludes_include_all_heavy_names_at_every_nesting` — 9 dirs × 4 patterns
- `parse_realistic_e2b_spec` / `parse_rejects_non_sandbox_transport` /
  `parse_rejects_empty_provider_or_sandbox_id`
- `session_identity_kept_through_round_trip` / `session_match_ignores_extra_fields_in_saved` /
  `session_mismatch_with_different_provider`

### confinement (4 个)
- `happy_path_full_sync_in_flow` — multi-file 全 pass
- `confinement_rejects_source_outside_root` — error message 含 "source" + "escapes"
- `confinement_rejects_target_outside_root` — error message 含 "target" + "escapes"
- `confinement_rejects_dotdot_in_normalized_target` — 真正 normalize 后逃逸

### builders (7 个)
- `extract_runtime_asset_command_emits_full_rm_mkdir_tar_sequence`
- `workspace_tar_extract_overlay_form_used_for_git_overlay` (wipe=None)
- `workspace_tar_extract_destroy_then_replace_preserves_named_dirs` (wipe=Some)
- `remove_deleted_paths_quotes_each_path_individually` (含特殊字符路径)
- `remote_tarball_command_includes_archive_dir_creation`
- `remote_tarball_command_with_excludes_pipes_through_tar_exclude_flags`
- `build_unique_staging_path_uses_uuid_v4` (验证 36-char UUID)

### types (5 个)
- `sandbox_sync_file_mapping_defaults_access_to_ro`
- `sandbox_post_upload_command_carries_cwd_and_timeout`
- `additional_source_round_trip` (serde 序列化反序列化)
- `asset_with_provision_and_restore_round_trips`
- `prepared_runtime_round_trip_with_collection` (BTreeMap 完整 round-trip)
- `identity_struct_serializes_as_expected`

### cross-module smoke (1 个)
- `cross_module_smoke_full_sync_in_pipeline` — 完整 sync-in 6 步流水线:
  parse → identity → confine → unique staging → tar → workspace extract → asset extract

## 编译 / 测试数据

```
pc-acpx lib : 658 passed; 0 failed (was 619 at start of R401, +39)
pc-acpx tests: 35 integration test files (was 34, +1 round401)
round401 test file: 25 tests; 0 failed

新增 .rs 代码:1108 行 (Node 1224 行 → Rust 1108 行,parity 90%+ 
                    因纯函数子集不包含 SSH/tar async 部分)
新增测试: 39 unit + 25 integration = 64 个
```

## 高内聚低耦合验证

- `sandbox_managed_runtime.rs` 与 `command_managed_runtime.rs` 完全独立
  (两者都不互相 `use`)
- 仅依赖 stdlib + `serde` + `serde_json` + `uuid`
- 可单独 mock / 测试 / 替换
- 与 `executor.rs` 集成点(`assert_sync_operations_confined` +
  `build_*`)在 adapter 层调用,pc-acpx 不耦合 adapter

## 结论

R401 完成 sandbox_managed_runtime 纯函数 1:1 移植 (13 fn + 14 type +
1 const + 1 derived set),async 部分按既定原则明确标注 "延后"。所有
测试通过、无回归。下一步 R402 (execution_target.ts, 1877 行) 是剩余最大
单缺口,预计 2 轮完成。
