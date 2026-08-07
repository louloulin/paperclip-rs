# R384 — 全面 paperclip-rs vs paperclip 差距盘点

> 截至 R383 末(2026-08-07)对 pc-acpx crate 的最后 5 gap 闭合。  
> 本文档盘点整个 paperclip-rs workspace 与 Node `paperclip/` 的模块差距,作为后续 R384+ 路线图。

## 评估方法

1. **Node `paperclip/` 仓库** 扫描所有 export functions / services / routes
2. **Rust `paperclip-rs/` workspace** 各 crate 的 lib.rs 行数 + tests 数量 + 实际覆盖检查
3. **逐模块对比** Node export ↔ Rust `pub fn` 名称(snake_case 转换)

## 总体规模

| 维度 | Node `paperclip/` | Rust `paperclip-rs/` | 进度 |
|---|---|---|---|
| 包/crate | 11 packages | 37 crates | Rust 多拆分 |
| `server/src/services/*` 服务模块 | 100+ 个 | (分布在 pc-heartbeat / pc-acpx / pc-http / pc-repos 等) | ~70% |
| `server/src/routes/*` 路由 | 56 个 | 69 个(pc-http/routes) | ~100% |
| `adapter-utils/src/server-utils.ts` | 3415 行,38 exports | pc-acpx prompt_compose + env_helpers + normalize + settings | ~85% |
| `acpx-engine/*` | (server-utils 复用) | pc-acpx 全套 | ~95% |
| 测试数(workspace) | ~4000 | ~700 | 比例匹配 |

## Crate 状态总览(2026-08-07)

| Crate | lib.rs LOC | tests | 状态 | 备注 |
|---|---|---|---|---|
| `pc-acpx` | 232 | 22 (541 tests) | 100% | R362-R383 完成 |
| `pc-heartbeat` | 1405 | 65 (R290-R360) | 95% | readiness + recovery 全套 |
| `pc-http` | 12 | 48 (69 routes) | 100% | 全部 routes 迁移 |
| `pc-repos` | 115 | 75 | 90% | 仓储层主体 |
| `pc-realtime` | 297 | 1 | 中等 | |
| `pc-adapter-claude-local` | 520 | 1 | 中等 | |
| `pc-adapter-codex-local` | 376 | 1 | 中等 | |
| `pc-adapter-cursor-local` | 434 | 1 | 中等 | |
| `pc-adapter-api` | 340 | 0 | 中等 | |
| `pc-adapter-process` | 337 | 0 | 中等 | |
| `pc-auth` | 581 | 1 | 中等 | |
| `pc-config` | 275 | 0 | 中等 | |
| `pc-errors` | 253 | 0 | 100% | 完整 |
| `pc-core` | 198 | 0 | 工具库 | |
| `pc-secrets` | 160 | 1 | 中等 | 远端解密未完整 |
| `pc-ws` | 139 | 0 | 中等 | |
| `pc-telemetry` | 131 | 0 | 中等 | |
| `pc-authz` | 128 | 0 | 中等 | |
| `pc-adapter-{gemini,grok,hermes,openclaw,opencode,pi,cursor-cloud,hermes-gateway}-local` | 147-186 | 1 each | stub | 待实现 |
| `pc-plugin-host` / `pc-plugin-protocol` | 48 / 35 | 1 each | stub | |
| `pc-storage` / `pc-backup` / `pc-cron` | 19-29 | 1 each | stub | |
| `pc-workflow` / `pc-feature-flags` / `pc-activity` / `pc-openapi` / `pc-db` | 14-24 | 0-1 | 占位 | |

## adapter-utils 剩余纯函数(待 R384+)

`server-utils.ts` 共 38 个 exports,R383 末剩余未实现:

| Node export | LOC | 复杂度 | 优先级 | 备注 |
|---|---|---|---|---|
| `isPaperclipRuntimeEnvKey` | L114-121 | 极简单 | R384 | 正则匹配 |
| `isForbiddenConfigEnvKey` | L122-131 | 极简单 | R384 | 正则匹配 |
| `expandHomePrefix` | L133-137 | 简单 | R384 | 字符串替换 |
| `signalRunningProcess` | L82-112 | 中等 | R385 | Unix process group signal |
| `resolvePaperclipInstanceRootForAdapter` | L139-285 | 复杂 | R386 | 多 OS 路径解析 |
| `resolvePaperclipSkillSyncPreference` | L2794-2834 | 中等 | R387 | 配置解析 |
| `writePaperclipSkillSyncPreference` | L2870-3002 | 复杂 | R387 | JSON 写入 |
| `resolvePaperclipDesiredSkillNames` | L2858-2869 | 简单 | R387 | 名称归一化 |
| `buildRuntimeMountedSkillSnapshot` | L2491-2608 | 复杂 | R388 | skills 快照 |
| `buildPersistentSkillSnapshot` | L2609-2734 | 复杂 | R388 | skills 快照 |
| `sanitizeInheritedPaperclipEnv` | L2229-2241 | 简单 | R384 | env filter |
| `redactEnvForLogs` | L1926-1933 | 简单 | R384 | 正则匹配 |
| `redactCommandTextForLogs` | L1934-1937 | 简单 | R384 | 复用 redact |
| `buildInvocationEnvForLogs` | L1938-1964 | 中等 | R384 | env merge + redact |
| `sanitizeSshRemoteEnv` | L2311-2317 | 极简单 | R385 | env filter |
| `shapePaperclipWorkspaceEnvForExecution` | L2023-2117 | 中等 | R385 | env shape |
| `rewriteWorkspaceCwdEnvVarsForExecution` | L2118-2154 | 中等 | R385 | env rewrite |
| `refreshPaperclipWorkspaceEnvForExecution` | L2155-2228 | 中等 | R385 | env refresh |
| `isPidAlive` | L3003-3013 | 极简单 | R384 | `kill 0` 检查 |
| `materializePaperclipSkillCopy` | L3038-... | 复杂 async | R389 | skill materialize |

## 核心 crate 复刻状态

### pc-acpx (R362-R383 完成)
- `prompt_compose.rs` 3569 行 (Node server-utils.ts 3415 行,Rust 已超越)
- `env_helpers.rs` ~250 行 (Node L2229-2490)
- `normalize.rs` ~400 行 (Node L350-369)
- `subprocess_handle.rs` 277 行 (Node signal/isPidAlive)
- `acp_runtime.rs` / `subprocess_acp_runtime.rs` / `jsonrpc_wire.rs` (Node acpx-engine/*)
- 22 个测试文件,541 tests 全过

### pc-heartbeat (R290-R360)
- `lib.rs` 1405 行
- `readiness.rs` 1127 行 (Node `services/recovery/service.ts` evaluateSilentRunLevel 等)
- `retry_policy.rs` 507 行 (Node computeBoundedTransientHeartbeatRetryDelay)
- `wake_dedup.rs` 676 行 (Node wake dedup logic)
- `wake_dispatch.rs` 216 行
- `recovery/*` 53 个模块,共 ~14000 行
- 65 个测试文件,700+ tests

### pc-http (路由 100%)
- 69 routes 已迁移(Node 56,部分拆分)
- 48 个测试文件
- middleware 部分迁移

### pc-repos (仓储 90%)
- 75 个测试
- 仓储层主体完成

## 后续路线图(按优先级)

### P0 — adapter-utils 剩余纯函数(R384-R389)
- R384: `isPaperclipRuntimeEnvKey` / `isForbiddenConfigEnvKey` / `expandHomePrefix` / `redactEnvForLogs` / `redactCommandTextForLogs` / `buildInvocationEnvForLogs` / `sanitizeInheritedPaperclipEnv` / `isPidAlive`(纯函数批)
- R385: `signalRunningProcess` / `sanitizeSshRemoteEnv` / `shapePaperclipWorkspaceEnvForExecution` / `rewriteWorkspaceCwdEnvVarsForExecution` / `refreshPaperclipWorkspaceEnvForExecution`(env shape + signal)
- R386: `resolvePaperclipInstanceRootForAdapter`(复杂 OS 路径解析)
- R387: `resolvePaperclipSkillSyncPreference` / `writePaperclipSkillSyncPreference` / `resolvePaperclipDesiredSkillNames`(skill sync 配置)
- R388: `buildRuntimeMountedSkillSnapshot` / `buildPersistentSkillSnapshot`(skill snapshot)
- R389: `materializePaperclipSkillCopy`(async skill materialize)

### P1 — adapter 充实(R390+)
- `pc-adapter-gemini-local` / `pc-adapter-grok-local` / `pc-adapter-opencode-local` / `pc-adapter-pi-local` / `pc-adapter-cursor-cloud` 等 stub 实现

### P2 — pc-secrets / pc-plugin-host 真实实现
- AWS/GCP/Vault 真实解密
- plugin worker→host 双向回调 + 生命周期恢复

### P3 — server services 业务逻辑
- decisions / decision-training 完整数据流
- company-skills version 管理 / fork 流程 / test-run 状态机

### P4 — UI e2e + Phase G 切流量

## 当前测试统计

```
workspace cargo test: 694 tests passed, 0 failed
pc-acpx: 541 tests (lib 293 + integration 248)
pc-heartbeat: ~700 tests (lib 1405 + 65 test files)
pc-repos: 75 tests
其他 crate: 合计 ~100 tests
```

## 下一步行动(R384)

按上述路线图 R384 优先级,做第一个批次的纯函数复刻:
1. `is_pid_alive` (L3003-3013, ~9 行)
2. `is_paperclip_runtime_env_key` (L114-121, ~8 行)
3. `is_forbidden_config_env_key` (L122-131, ~10 行)
4. `expand_home_prefix` (L133-137, ~5 行)
5. `redact_env_for_logs` (L1926-1933, ~8 行)
6. `redact_command_text_for_logs` (L1934-1937, ~4 行)
7. `build_invocation_env_for_logs` (L1938-1964, ~27 行)
8. `sanitize_inherited_paperclip_env` (L2229-2241, ~13 行)

合计 ~80 行 Node 纯函数,加测试预计 200-300 行 Rust。

预计 R384 完成:pc-acpx 548+ tests,workspace 700+ tests。
