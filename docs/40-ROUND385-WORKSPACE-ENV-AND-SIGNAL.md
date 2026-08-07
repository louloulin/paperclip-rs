# R385 — Workspace Env Shaping + Subprocess Signal Dispatch

## 目标

按 `comet-open` + `RTK` 思路,把 Node `adapter-utils/src/server-utils.ts`
中尚未在 `pc-acpx` 复刻的 5 个 workspace/env/signal 模块一次性补齐,
保持高内聚低耦合(2 个独立模块:`workspace_env` 和 `subprocess_signal`)。
全部纯函数 / 零 I/O / 零 unsafe。

## 范围

- 新增 `crates/pc-acpx/src/workspace_env.rs`(~720 行含 15 单测)
- 新增 `crates/pc-acpx/src/subprocess_signal.rs`(~310 行含 8 单测)
- `crates/pc-acpx/src/lib.rs` 增加 2 个模块 + re-exports
- 新增 `crates/pc-acpx/tests/round385_workspace_env_and_signal.rs`(22 集成测试)
- 跟 Node `paperclip/packages/adapter-utils/src/server-utils.ts`
  L82-112 / L2023-2228 / L2311-2317 行对齐,以及
  `paperclip/packages/adapter-utils/src/remote-execution-env.ts` L1-L44 对齐。

## 复刻的 5 个模块

### 1. `sanitize_ssh_remote_env` (Node L2311-2317)
包装 `sanitizeRemoteExecutionEnv`(`remote-execution-env.ts` L28-44):
- 非 identity-key(15 个 host identity: PATH/HOME/PWD/SHELL/USER/.../XDG_*)forward verbatim
- identity-key 且值匹配 inherited env → 丢弃(remote 重新推导)
- identity-key 但值不同 → forward
- `read_env_value_case_insensitive` 大小写不敏感查找

### 2. `shape_paperclip_workspace_env_for_execution` (Node L2023-2117)
- Local target:pass-through + trim + 空值 → None
- Remote target:rewrite `workspaceCwd` → `executionCwd`,null worktree,rewrite hints:
  - hint cwd 等于 local workspace cwd → 改 executionCwd(否则 strip)
  - hint 有 `projectId` 且 `stagedProjectDirs` 有 entry → 改 staged dir
  - 其他 hint → strip cwd(永不暴露未 staged 的本地路径)

### 3. `rewrite_workspace_cwd_env_vars_for_execution` (Node L2118-2154)
- 非 remote / 缺 cwd → no-op
- 对每个 `*_WORKSPACE_CWD` env,值等于 local workspace cwd → 替换成 remote cwd
- 注意:`executionCwd` 是 remote 路径,不需要 `path.resolve`(避免 host-Node 语义)

### 4. `refresh_paperclip_workspace_env_for_execution` (Node L2155-2228)
组合上述 helper:
1. shape env
2. 删 3 个 stale `PAPERCLIP_WORKSPACE_*` keys
3. apply 9 个 workspace env mappings
4. 有 hints → `PAPERCLIP_WORKSPACES_JSON`
5. user-config env 转发,但:
   - 禁止 `PAPERCLIP_API_KEY`
   - 不覆盖 runtime 已分配的 `PAPERCLIP_*` keys

### 5. `signal_running_process` (Node L82-112)
- Unix-only
- `child_already_exited=true` → `SkippedAlreadyExited`
- `process_group_id > 0` → 先 group signal,失败则 fallback 到 direct
- 否则 direct signal
- 失败 → `Failed { reason }`
- `Signal` enum:SIGHUP/SIGINT/SIGQUIT/SIGTERM/SIGKILL/SIGUSR1/SIGUSR2
- 同样 `forbid unsafe_code` 用 `sh -c "kill -<n> <pid>; echo $?"` 外部命令

## 关键设计决策

### `lexically_normalize` 路径解析
Node `path.resolve` 是 lexical(不解析 symlink),Rust 端:
```rust
fn lexically_normalize(mut path: PathBuf) -> PathBuf { ... }
```
手写 `.` / `..` 解析,零文件系统 I/O,完全等价于 Node 的语义。

### `dispatch_signal_i64` 跨平台策略
为避免 i32 overflow(`2147483646` cast 为 i32 负值会 wrap),
用 `i64` 表示 pid:`Some(2147483646_i64)` 保持正数。

### `process_group_id > 0` gate
Node 原代码 `pgid > 0` 守卫避免 `pgid == 0`(caller's own group)被误信号。
Rust 端完整保留。

### `child_already_exited` 短路
Node 等价于 `exitCode === null && signalCode === null`。Rust 端显式 boolean,
测试用 self pid + `true` 验证不会自杀。

## 测试

- 15 个新单元测试注入 `workspace_env::tests`
- 8 个新单元测试注入 `subprocess_signal::tests`
- 22 个新集成测试在 `tests/round385_workspace_env_and_signal.rs`

合计 R385 新增 45 个测试,全部绿色。

## 验证

```
cd paperclip-rs && cargo test -p pc-acpx
```

结果:619 个 pc-acpx tests 通过 (R384 是 576,+43),0 失败 0 回归。
`round385_workspace_env_and_signal.rs` 22 个新增测试全绿。

```
cd paperclip-rs && cargo fmt --check
```

clean。

## 下一步

R386 候选模块(复杂):
1. `resolve_paperclipInstanceRootForAdapter` (Node L139-285) — 复杂 OS 路径解析 + 多 OS 支持

R387 候选(skill sync):
2. `readPaperclipSkillSyncPreference` (L2794-2834)
3. `writePaperclipSkillSyncPreference` (L2870-3002)
4. `resolvePaperclipDesiredSkillNames` (L2858-2869)

R388 候选(skill snapshot):
5. `buildRuntimeMountedSkillSnapshot` (L2491-2608)
6. `buildPersistentSkillSnapshot` (L2609-2734)

R389 候选(async skill materialize):
7. `materializePaperclipSkillCopy` (L3038+)
