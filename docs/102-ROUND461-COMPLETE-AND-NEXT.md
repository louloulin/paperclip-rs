# R461 完成 + 后续计划 (R462-R470)

## 1. R461 完成状态

**R461.7 修复完成**（commit 78daa50）：
- ✅ 3 个端到端测试用 fixtures 方式重写（避开 Rust 字符串嵌套转义地狱）
- ✅ 2 个新增 fixture 脚本（claude_retry_unknown_session.sh + claude_invalid_uuid.sh）
- ✅ 1 个 helper `copy_fixture_to_temp` 抽象
- ✅ MCP identity 测试改用 raw string `r#"[{"name":"a"}]"#`
- ✅ `clear_session` 断言对齐 Node L1202（resolvedSessionId 有值时为 false）

**测试快照（commit 78daa50）**：
```
pc-acpx:                  883 passed
pc-adapter-codex-local:   260 passed
pc-adapter-claude-local:  344 passed (R461.1-7 全部编译通过)
pc-activity:               14 passed
pc-adapter-process:        6 passed
pc-adapter-quota:          39 passed
─────────────────────────────
合计:                    1546 passed, 0 failed
```

## 2. claude-local 整体覆盖率提升

| 维度 | R461 前 | R461 后 | 提升 |
|---|---|---|---|
| execute.ts L736-870（resume 决策 + effectiveEffort） | 0% | 100% | ✅ |
| execute.ts L867-1199（toAdapterResult 整合） | 50% | 90% | +40% |
| execute.ts L1189-1267（resume retry 主循环） | 0% | 100% | ✅ |
| execute.ts L789-828（joinPromptSections + promptMetrics） | 0% | 100% | ✅ |
| execute.ts L506-524（runtimeMcpServers + writeMcpConfig） | 0% | 100% | ✅ |
| **claude-local 整体覆盖率** | **~75%** | **~85%** | **+10%** |

## 3. 后续差距清单

### 3.1 claude-local execute.ts 剩余 15%

| 缺口 | Node 位置 | 重要性 | 实现复杂度 |
|---|---|---|---|
| 远程 execution target 集成 | L570-690 | 高 | 高 |
| preparedExecutionTargetRuntime（远程 workspace + asset sync） | L570-622 | 高 | 高 |
| materializeRemoteClaudeConfig（远程 claude config 同步） | L665-690 | 中 | 中 |
| startAdapterExecutionTargetPaperclipBridge（远程 bridge） | L679-690 | 高 | 高 |
| restoreRemoteWorkspace（结果回传） | L628-635 | 中 | 中 |
| buildClaudeArgs 完整实现（含 --chrome / --max-turns / --mcp-config） | L831-870 | 中 | 中 |
| localProcessSandbox 选项构造（bwrap + network + fs scope） | L530-555 | 中 | 高 |
| chmodJsonlPath + unlink poisoned session file | L1208-1224 | 低 | 低 |

### 3.2 codex-local execute.ts 剩余 2%

- 远程执行路径（同 claude-local）：stagedCodexHomeDir teardown + restoreRemoteWorkspace
- remoteCodexConfigDir 决策
- 预计增量 +5 测试

### 3.3 pc-http 路由层缺口

| 路由 | 缺口 | 重要性 |
|---|---|---|
| `/v1/test-environment` | 端到端 wiring（调用 claude_test::hello_probe_outcome + acpx hello probe） | 中 |
| `/v1/agent-runs/:id/rerun` | codex-local 远程 rerun 路径 | 低 |
| Adapter provider quota 路由 | 完整 quota.ts 复刻 | 低 |

### 3.4 pc-acpx 深化

| 模块 | 缺口 |
|---|---|
| execution_target.rs | SSH target process_session bridge（已有 61 测试，但 Node 的 paperclipBridge 抽象未完整对齐） |
| prepared_runtime.rs | remote asset sync 中的 skill 同步决策（与 skills.rs 联动） |
| prompt_compose.rs | bootstrapPromptTemplate + renderTemplate（与 claude-prompt 联动） |

### 3.5 其他（明确延后）

- hermes / cursor-cloud / openclaw adapter（用户明确要求只做 claude-local + codex-local）
- pc-repos / pc-heartbeat 深化
- 完整 quota.ts 复刻
- pc-gateway

## 4. 优先级计划（按价值/复杂度比）

### R462 — claude/codex 远程 execution target 基础 (重要性高，复杂度高)
- claude-local: 整合 prepared_runtime + materializeRemoteClaudeConfig + paperclipBridge 启动
- codex-local: stagedCodexHomeDir teardown + restoreRemoteWorkspace
- 预期增量 +12 测试，覆盖率 +5%

### R463 — buildClaudeArgs 完整实现 (重要性中，复杂度中)
- 整合 --chrome / --max-turns / --mcp-config / --add-dir / --dangerously-skip-permissions
- 预期增量 +8 测试

### R464 — chmodJsonlPath + poisoned session file cleanup (重要性低，复杂度低)
- 直接 fs 操作，独立模块
- 预期增量 +4 测试

### R465 — pc-http testEnvironment 端到端 wiring (重要性中，复杂度中)
- 调用 claude_test::hello_probe_outcome + acpx hello probe
- 预期增量 +6 测试

### R466 — codex-local 远程补全 (重要性中，复杂度中)
- stagedCodexHomeDir + remoteCodexConfigDir + remote auth json
- 预期增量 +5 测试

### R467-R470 — 长期深化
- localProcessSandbox 选项构造
- bootstrapPromptTemplate + renderTemplate 完整化
- quota.ts 完整复刻
- pc-repos / pc-heartbeat

## 5. 推荐下一个 R 阶段：R462

**理由**：
1. 用户约束：「adapter 优先只实现 claude-local 和 codex-local」
2. R462 直接补全 claude/codex 适配器的远程执行能力，是这两条适配线的最后大缺口
3. 已有 pc-acpx::execution_target.rs (61 测试) + prepared_runtime.rs (9 测试) 基础
4. 实现后可使两个 adapter 覆盖率均达 ~90%
