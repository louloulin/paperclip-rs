# R469-R473 完成 — 远程 SSH workspace 决策 + bridge env 注入 + quota 封装

## 1. 目标

按用户约束（adapter 优先 claude-local + codex-local，中文说明，高内聚低耦合），
对齐 Node `claude-local` / `codex-local` 的远程执行分支与 `quota.ts`：

- R469：远程 SSH workspace 决策纯函数（两个 adapter）
- R470：paperclipBridge env 注入决策
- R471：claude quota 封装（复用 pc-adapter-quota）
- R472：远程 resume 语义精确化（发现并修复 pc-acpx 真实差距）
- R473：全量测试验证 + 测试并发缺陷修复

## 2. 新增模块

### R469 — `claude_remote_workspace.rs`（claude-local，约 420 行，26 测试）

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `resolve_remote_workspace_dir` | `remoteDir ?? spec.remoteCwd` | remoteDir 缺省回退 |
| `managed_remote_runtime_workspace_dir` | managedRemoteWorkspace | `.paperclip-runtime/runs/<runId>/workspace` |
| `remote_execution_uses_paperclip_bridge` | `adapterExecutionTargetUsesPaperclipBridge` | 远程才启动 bridge |
| `remote_session_identity_matches` | `adapterExecutionTargetSessionMatches` | SSH 4 元组 + remoteCwd |
| `should_resume_remote_session` | `canResumeSession`（claude 版） | UUID + promptBundle + MCP + cwd + identity |
| `is_valid_uuid` | `/^[0-9a-f]{8}-...$/i` | 手写 UUID v4 检测 |
| `remote_env_replaces_workspace_cwd` | `rewriteWorkspaceCwdEnvVarsForExecution` | `*_WORKSPACE_CWD` 重写决策 |
| `remote_sync_excludes` | `prepareWorkspaceForSshExecution` | git / 非 git 排除项 |

### R469 — `codex_remote_workspace.rs`（codex-local，约 380 行，21 测试）

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| 同上基础函数 | 同左 | 同上 |
| `remote_codex_home_dir` | `${managedRemoteWorkspace}/.paperclip-runtime/codex/home` | 远程 home 资产 |
| `codex_home_sync_allowlist` | `CODEX_SYNC_ALLOWLIST` | config.json/toml/instructions.md/auth.json/skills |

### R470 — `codex_bridge_env.rs`（codex-local，约 260 行，13 测试）

| Rust 函数 | Node 对应 | 说明 |
|---|---|---|
| `should_start_paperclip_bridge` | `adapterExecutionTargetUsesPaperclipBridge` | 远程 target |
| `resolve_bridge_runtime_root_dir` | `runtimeRootDir ?? remoteCwd/.paperclip-runtime/<adapterKey>` | POSIX join |
| `resolve_bridge_host_api_url` | `hostApiUrl > PAPERCLIP_RUNTIME_API_URL > PAPERCLIP_API_URL > 默认` | URL 优先级 |
| `bridge_env_from_handle` | `paperclipBridge.env` | 4 个 env 注入 |
| `merge_bridge_env` | `Object.assign(env, bridge.env)` | 覆盖语义 |

### R471 — `claude_quota.rs`（claude-local，约 120 行，6 测试）

复用 `pc-adapter-quota` 已实现的 claude 配额逻辑，统一 re-export：

- `claude_config_dir` / `read_claude_token` / `claude_to_percent`
- `map_anthropic_oauth_usage` / `parse_claude_cli_usage_text` / `probe_claude_local`
- `CLAUDE_USAGE_SOURCE_OAUTH` / `CLAUDE_USAGE_SOURCE_CLI`

## 3. 真实差距修复（R472 关键发现）

### pc-acpx `adapter_execution_target_session_matches` 缺少 remoteCwd 检查

Node `remoteExecutionSessionMatches`（remote-managed-runtime.ts L89-101）明确要求
SSH 身份匹配包含 **remoteCwd** 5 元组：

```ts
asString(parsedSaved.transport) === currentIdentity.transport &&
asString(parsedSaved.host) === currentIdentity.host &&
asNumber(parsedSaved.port) === currentIdentity.port &&
asString(parsedSaved.username) === currentIdentity.username &&
asString(parsedSaved.remoteCwd) === currentIdentity.remoteCwd
```

Rust 原实现只检查 4 元组（transport/host/username/port），**缺少 remoteCwd** →
已修复 `crates/pc-acpx/src/execution_target.rs`。

### should_resume_remote_session 语义精确化

- **codex-local**：`sessionId 非空 && (cwd 空 || resolve(cwd)==resolve(effectiveCwd)) && sessionMatches(runtimeRemoteExecution, target)`
- **claude-local**：在 codex 基础上增加 `isValidUuid && hasMatchingPromptBundle && hasMatchingMcpServers`，
  cwd 匹配在远程时恒 true（`executionTargetIsRemote || cwd空 || resolve==resolve`）

### 测试并发缺陷修复

`tempdir()` 用 `SystemTime::now().as_nanos()` 做唯一性，并发运行多个 acp 测试时
两个测试可能拿到相同纳秒 → 共享目录 → `find_ancestor_bin` 交叉命中 →
`default_*_acp_fallback_reason_missing_command_returns_reason` 偶发失败。
已在 codex-local / claude-local 的 `tempdir()` 追加 `uuid::Uuid::new_v4().simple()` 保证唯一。

## 4. 集成测试

| 文件 | 场景 |
|---|---|
| `tests/round469_remote_workspace.rs`（codex） | 11 测试：受管目录 / home 资产 / bridge env / resume 决策 / sync excludes |
| `tests/round469_remote_workspace.rs`（claude） | 6 测试：受管目录 / env 重写 / resume 两分支 / sync excludes |

对齐 Node `execute.remote.test.ts` 3 个场景（prepares workspace / no resume / resume match）。

## 5. 测试快照

| Crate | R465 后 | R473 后 | Δ |
|---|---|---|---|
| pc-acpx | 883 | 883 | 0（+1 断言修复） |
| pc-adapter-claude-local | 437 | 475 | +38 |
| pc-adapter-codex-local | 384 | 429 | +45 |
| pc-activity | 14 | 14 | 0 |
| pc-adapter-process | 6 | 6 | 0 |
| pc-adapter-quota | 39 | 39 | 0 |
| **合计** | **1763** | **1846** | **+83** |

## 6. 后续计划

| 优先级 | 模块 | 内容 |
|---|---|---|
| P1 | codex-local `execute.remote` 深化 | stageCodexHomeForSync 决策 + 远程 runtime 组合 |
| P1 | claude-local `claude_session_params` | 远程 sessionParams 序列化对齐 |
| P2 | pc-acpx `remote_managed_runtime` | prepareRemoteManagedRuntime 决策（workspaceRemoteDir / runtimeRootDir / assets） |
| P2 | `sandbox_callback_bridge` 决策补全 | bridge 路由/header 决策已实现，补 worker/server 决策 |
| P3 | 其他 adapter（cursor/gemini/grok/opencode/pi） | 保持低优先级，按用户约束延后 |
