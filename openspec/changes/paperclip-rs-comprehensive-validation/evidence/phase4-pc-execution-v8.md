# Phase 4 — pc-execution 远程 execution 基础设施（V8）

## 目标

实现 V8 远程 execution 路径：SSH bridge 抽象 + restore_remote_workspace + materialize_remote_claude_config，1:1 镜像 Node `workspace-runtime.ts`。

## 实现

### 新增 crate `pc-execution`

| 模块 | 用途 |
|---|---|
| `lib.rs` | 模块导出 + crate doc |
| `ssh.rs` | `SshSession` trait + `SshAuth` enum + `SshSessionConfig` + `SshConnection` + `SshError` + `RemoteEvent`/`EventStream` + `NoopSshSession` + `RecordingSshSession` |
| `workspace_handle.rs` | `RemoteWorkspaceHandle` value object + `RemoteWorkspaceId` + `RemoteWorkspaceState`（Pending/Discovered/Restored/Failed）|
| `restore.rs` | `RestorePlan` + `RestoreStage` enum + `RestoreOutcome` + `RestoreError` + `RestoreStageError` + `classify_restore_error` + `restore_remote_workspace` async orchestrator + `stream_has_exit_zero` helper |
| `materialize.rs` | `ClaudeConfigSource` enum (Remote/Snapshot/Inline) + `ClaudeConfigMaterialization` + `MaterializeError` + `derive_target_path` + `count_encrypted_secrets` + `materialize_remote_claude_config` |

### Cargo.toml 调整

- 在 workspace root `Cargo.toml` 的 `members` 列表加入 `crates/pc-execution`

### 测试覆盖（29 unit tests）

| 模块 | 测试 |
|---|---|
| ssh | 7 tests (Password / PublicKey / Config new / Noop fail / Recording capture / Error variants) |
| workspace_handle | 5 tests (ID unique / handle new/discovered/restored/failed / state as_str) |
| restore | 7 tests (default plan / classify Ssh/Probe/Transfer/Snapshot / reject empty paths / success with recording) |
| materialize | 10 tests (derive basic / strips slash / sanitizes host / reject empty / count secrets / materialize remote/snapshot/inline) |

## 设计要点

- `SshSession` trait 抽象让单元测试用 `RecordingSshSession` 替代真实 ssh2（V8.7 deferred 至真实环境）
- `EventStream` 用 `tokio::sync::mpsc` 提供 backpressure 友好的事件流
- `RestoreStage` 是 `Copy + Eq + Hash` enum，便于 stage-level dedup / 测试
- `RestoreError` enum 用 `#[from]` 派生自动从 `SshError` 转换
- 全部 pure functions（不依赖全局 env / IO）
- `forbid(unsafe_code)` workspace 级强制
- 1:1 镜像 Node `restoreRemoteWorkspace` 的 5 阶段 pipeline

## 测试结果

```
cargo test -p pc-execution --lib
running 29 tests
test materialize::tests::derive_target_path_basic ... ok
test materialize::tests::derive_target_path_strips_trailing_slash ... ok
... (29 tests)
test restore::tests::restore_success_with_recording_session ... ok

test result: ok. 29 passed; 0 failed; 0 ignored
```

```
cargo test --workspace --lib --exclude pc-adapter-process
TOTAL PASS: 8454 (8425 → 8454, +29)
```

## 后续（仍 deferred）

- 4.5 pc-http::routes::execution_workspaces 接入 pc-execution（需 integration 测试）
- 4.7 真实 SSH 集成测试（需 ssh2 crate + 测试 SSH 服务）

## 累计

- 新 crate pc-execution 加入 workspace（crates 数 101 → 102）
- 29 新单测
- workspace lib 8425 → 8454 PASS / 0 FAIL