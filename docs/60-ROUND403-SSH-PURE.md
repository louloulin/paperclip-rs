# R403 — SSH Pure Helpers (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapter-utils/src/ssh.ts`
(1862 行) 的纯函数部分抽离并独立模块化:

- **新增** `pc-acpx::ssh` 模块 (572 行 Rust)
- **重构** `execution_target.rs` 中 R402 内联的 `SshRemoteExecutionSpec` /
  `parse_ssh_remote_execution_spec` 改为 `pc_acpx::ssh::` 的 re-export
- **保持向后兼容**:`pc_acpx::execution_target::SshRemoteExecutionSpec` 名称仍
  可访问(指向 ssh 规范类型)
- **保留** `remote_managed_runtime.rs` 中简化版 `SshRemoteExecutionSpec`
  (用于 session identity,字段减少),不破坏 round399 集成测试

## Node 函数 / 类型映射

### 已移植 (纯函数 / types)

| Node | Rust | 说明 |
|---|---|---|
| `SshConnectionConfig` (interface) | `SshConnectionConfig` (struct) | host/port/username/remoteWorkspacePath/privateKey/knownHosts/strictHostKeyChecking |
| `SshCommandResult` (interface) | `SshCommandResult` | stdout/stderr 容器 |
| `SshRemoteExecutionSpec extends SshConnectionConfig` | `SshRemoteExecutionSpec` | 加 `remoteCwd`;带 `from_parts()` / `as_connection_config()` / `effective_remote_workspace_path()` |
| `shellQuote` | `shell_quote` | POSIX single-quote |
| `isValidShellEnvKey` (internal) | `is_valid_shell_env_key` | public,供 runtime 异步半使用 |
| `parseSshRemoteExecutionSpec` | `parse_ssh_remote_execution_spec` | full 移植 |
| `tarExcludeArgs` | `tar_exclude_args` | flatMap 成 `["--exclude", "._*", ...]` |
| `tarSpawnEnv` | `tar_spawn_env_defaults` | 返回 `BTreeMap<String, String>`,含 `COPYFILE_DISABLE=1` |
| `tarPatternToRegExp` | `tar_pattern_to_regexp` | 转义 regex special + `*` → `[^/]*`,`?` → `[^/]` |
| `buildKnownHostsEntry` (export) | `build_known_hosts_entry` | `[host]:port pubkey` |
| `KnownHostsEntryInput` (新) | `KnownHostsEntryInput` (struct) | host/port/publicKey |

### 延后 (async / SSH runtime / sshd 进程)

| Node | Rust | 原因 |
|---|---|---|
| `createSshCommandManagedRuntimeRunner` | ❌ 延后 | 需 ssh runtime |
| `runSshCommand` | ❌ 延后 | 需 ssh process |
| `buildSshSpawnTarget` | ❌ 延后 | 需 ssh + terminal |
| `syncDirectoryToSsh` / `syncDirectoryFromSsh` | ❌ 延后 | 需 stream + scp-like |
| `prepareWorkspaceForSshExecution` | ❌ 延后 | 需 SSH runtime |
| `restoreWorkspaceFromSshExecution` | ❌ 延后 | 需 SSH runtime |
| `ensureSshWorkspaceReady` | ❌ 延后 | 需 SSH runtime |
| `getSshEnvLabSupport` / `startSshEnvLabFixture` / `buildSshEnvLabFixtureConfig` | ❌ 延后 | 需 spawn sshd 进程 |
| `readSshEnvLabFixtureState` / `stopSshEnvLabFixture` / `readSshEnvLabFixtureStatus` / `isSshEnvLabFixtureProcess` / `fileExists` | ❌ 延后 | 需 fs + 进程操作 |
| `createSshAuthArgs` / `runLocalGit` / `commandExists` / `resolveCommandPath` | ❌ 延后 | 需 fs / exec |
| `estimateLocalDirSize` / `probeRemoteDirSize` | ❌ 延后 | 需 fs walk |
| `withTempFile` / `execFileText` / `spawnText` | ❌ 延后 | 需 process |

## 关键设计决策

1. **canonical spec 集中在 `pc-acpx::ssh`**:
   - `pc_acpx::ssh::SshRemoteExecutionSpec` 是 8 字段完整版
   - `pc_acpx::execution_target::SshRemoteExecutionSpec` 是 `pc_acpx::ssh::` 的 alias
   - `pc_acpx::remote_managed_runtime::SshRemoteExecutionSpec` 是简化版
     (4 字段 + `Option<u16>` port),继续供 session identity 比较使用
   - 三者不冲突,各自有不同的 `pub struct SshRemoteExecutionSpec`,
     它们在不同 module namespace 下,调用方通过 `use` 显式区分

2. **`#[serde(rename_all = "camelCase")]` 与 Node wire 对齐**:
   - 所有字段 JSON 序列化为 camelCase(`host`, `port`, `privateKey`,
     `remoteWorkspacePath`, `remoteCwd`, `strictHostKeyChecking`, etc.)
   - `parse_ssh_remote_execution_spec` 接收 camelCase 输入并解析到 snake_case
     Rust 字段,与 R402 的 wire format 完全一致
   - 这样 `pc_acpx::ssh::SshRemoteExecutionSpec` 和
     `pc_acpx::execution_target::SshRemoteExecutionSpec` wire 完全兼容

3. **`tar_pattern_to_regexp` 返回 `Result<Regex, String>`**:
   - Node 抛 `RegExp` 构造异常,Rust `regex` crate 相对宽松(自动 escape 不平衡
     括号),造成 parity 偏离
   - 实用取舍:Rust 实现保留 `Result` 形式,harness 可观察,但实际业务中
     的 tar exclude 模式不会触发错误路径
   - 移除 R402 早期尝试用 `(unclosed` 测试 error branch 的设计

4. **`tar_exclude_args` 输出 `Vec<String>` 而非 `Vec<&str>`**:
   - Node 版本用 `flatMap` 展开 `[--exclude, pattern]` 数组,语义清晰
   - Rust 使用 `Vec<String>` 避免生命周期烦恼

5. **`tar_spawn_env_defaults()` 是纯默认**:
   - Node `tarSpawnEnv` 是 `process.env` + `COPYFILE_DISABLE=1`
   - Rust 移植只返回 `{COPYFILE_DISABLE: "1"}`,避免依赖 `process.env`
   - 实际 SSH tar spawn 会由 async 半在调用时合并

6. **shell_quote 实现与 `command_managed_runtime::shell_quote` 重复**:
   - 两份独立实现,因为 Node ssh.ts 和 command-managed-runtime.ts 都各自
     有 `shellQuote` (TypeScript 不去重)
   - R404+ 可考虑合并到一个 `pc_acpx::shell::shell_quote` 但风险高(影响
     已有模块),先保留两份

## 集成测试覆盖 (19 个)

### ssh spec 解析 (5)
- `ssh_spec_round_trips_via_camelcase_json` — 8 字段完整 round-trip
- `ssh_spec_parser_accepts_string_port` — port 字段可为字符串
- `ssh_spec_parser_rejects_partial_payload` — 8 个反例 null / string / number /
  array / empty / 缺字段 / port=0 / port=70000
- `ssh_spec_parser_optional_fields_default_to_none` — privateKey/knownHosts 空
  时为 None, strictHostKeyChecking 默认 true
- `ssh_spec_workspace_path_defaults_to_remote_cwd` — remoteWorkspacePath 缺省时
  回退 remoteCwd

### tar helpers (5)
- `tar_exclude_args_prepends_resource_fork_pattern` — 输入 node_modules +
  target 输出 `--exclude, ._*, --exclude, node_modules, --exclude, target`
- `tar_exclude_args_with_empty_inputs_is_still_resource_fork` — None / Some([])
  都等价
- `tar_spawn_env_disables_mac_appledouble` — BTreeMap 含 `COPYFILE_DISABLE=1`
- `tar_pattern_to_regexp_literal_match` / `_handles_glob` / `_escapes_regex_metachars`
  — literal / `*` / `?` / `.` 转义正确

### shell_quote (2)
- `shell_quote_round_trips_safe_paths` — 3 个普通用例
- `shell_quote_handles_embedded_quote` — 2 个嵌入 `'`,输出 8 个 single quote

### shell env keys (1)
- `shell_env_keys_are_validated` — 5 个有效 + 7 个无效

### known_hosts (2)
- `known_hosts_entry_uses_bracketed_host_port_form` — 标准 `[h]:p pk`
- `known_hosts_entry_trims_whitespace_in_inputs` — 输入 host/pk 前后空格被 trim

### cross-module smoke + 连接配置 (3)
- `cross_module_smoke_ssh_target_full_flow` — JSON → parse → execution_target
  → tar exclude + env → known_hosts 一站式
- `connection_config_round_trip_with_spec` — `from_parts` / `as_connection_config`
  对称
- `execution_target_parser_supports_ssh_via_ssh_module` — execution_target 的
  `parseAdapterExecutionTarget` 通过 ssh::SshRemoteExecutionSpec 路径解析成功

## 编译 / 测试数据

```
pc-acpx lib : 733 passed; 0 failed (was 709 at start of R403, +24)
pc-acpx tests: 37 integration files (was 36, +1 round403)
round403 test file: 19 tests; 0 failed
```

### R403 间接影响

- `execution_target.rs` 净减少 ~30 行 (去掉 inline spec + parser body)
- 引入 `pub use crate::ssh::SshRemoteExecutionSpec;` re-export 保留向后兼容
- 已有 R402 集成测试 (round402) 全部继续通过 (无破坏)

## 高内聚低耦合验证

- `ssh.rs` **完全独立**,只依赖 stdlib + `serde` + `regex`(已有依赖)
- 不依赖 `pc-core` / `pc-agent` / `pc-server`
- 公共 API 由 `pc_acpx::ssh::*` 提供,与 `execution_target` 通过 re-export
  共享类型,避免 pc-acpx 内循环依赖
- 集成测试仅 import `pc_acpx::ssh::` + `pc_acpx::execution_target::`(后者只
  为了 cross-module smoke)

## 结论

R403 完成 ssh.ts 大单缺口:
- 新增 canonical `pc-acpx::ssh` 模块 (572 行 / 24 单元测试 / 19 集成测试)
- 把 R402 内联的 SSH spec + parser 干净地抽到独立 module
- 保持向后兼容(`pc_acpx::execution_target::SshRemoteExecutionSpec`
  仍是 re-export)
- 纯函数部分完整覆盖

剩余 ssh.ts 异步代码 (1600+ 行) 留给 PC core / adapter 集成时实现。
下一步 R404 计划:`pc-acpx::sandbox_run_log_stream.rs` (278 行,tokio stream)
