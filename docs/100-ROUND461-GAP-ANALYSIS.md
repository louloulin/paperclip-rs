# R459/R461 差距分析 — claude-local execute.ts 复刻缺口

## 1. 当前快照

- **分支**：`main`，领先 `origin/main` 26+ commits
- **未提交**：9 个文件（pc-adapter-api 改类型 + pc-adapter-claude-local/codex-local 修补）
- **测试**：1402 全通过（pc-acpx 883 + pc-adapter-codex-local 260 + pc-adapter-claude-local 200 + pc-activity 14 + pc-adapter-process 6 + pc-adapter-quota 39）

## 2. claude-local 模块覆盖矩阵

| Node 文件 | 行数 | Rust 复刻 | 缺口 |
|---|---|---|---|
| acp.ts | 554 | acp.rs (1611 行) | 100% |
| claude-config.ts | 244 | claude_config.rs (164 行) | ~80%（Node 含 materializeRemoteClaudeConfig + writePaperclipClaudeMcpConfig 远程同步） |
| cli-capabilities.ts | 94 | cli_capabilities.rs (289 行) | 100% |
| config-schema.ts | 73 | config_schema.rs (256 行) | 100% |
| **execute.ts** | **1270** | **lib.rs (587 行)** | **~50% ← 最大缺口** |
| index.ts | 95 | lib.rs (descriptor + execute 入口) | ~70% |
| models.ts | 164 | claude_models.rs (211 行) | 100% |
| parse.ts | 507 | claude_stream_json.rs + claude_errors.rs (514 行) | ~95% |
| permissions.ts | 43 | claude_permissions.rs (137 行) | 100% |
| prompt-cache.ts | 174 | claude_prompt_cache.rs (271 行) | 100% |
| quota.ts | 541 | pc-adapter-quota 覆盖子集 | ~50% |
| skills.ts | 64 | skills.rs (402 行) | 100% |
| test.ts | 463 | claude_test.rs (635 行) + acp.rs 部分 | ~60%（端到端 wiring 缺） |

**claude-local 整体覆盖率：~75%**

## 3. R461 execute.ts 缺口细化

### 3.1 当前 Rust `ClaudeLocalAdapter::execute` 已实现

1. `default_command(config)` → 默认 claude
2. `build_claude_exec_args(config)` → ClaudeExecArgs
3. `execute_process_capture(spec, context, events)` → ExecutionResult
4. `parse_claude_stream_json(stdout)` → ParsedClaudeStreamJson
5. `resolve_claude_billing_type(env)` → 写入 billing_type
6. `decide_retry(ClaudeRetryInput)` → ClaudeRetryDecision（错误族 + clear_session）
7. stop_reason 决策（max_turns / poisoned / refusal）
8. 合并 result_json（sawProtocolEvent / stopReason / costUsd / claudeResult / errorFamily / paperclipEnvNote 等）

### 3.2 未实现（按重要性）

| 缺口 | Node 位置 | 重要性 | 实现复杂度 |
|---|---|---|---|
| session_resume_loop（unknown / poisoned / image 三种 session 错误重试） | execute.ts L1189-1227 | 最高 | 中（纯决策） |
| session_params 组装（含 promptBundleKey / mcpServerIdentity / remoteExecution / workspaceId / repoUrl / repoRef） | execute.ts L1100-1127 | 最高 | 中（纯函数） |
| session resume 决策（UUID 校验 + hasMatchingPromptBundle + hasMatchingMcpServers + sessionCwdMatchesExecutionTarget） | execute.ts L736-770 | 高 | 中（纯决策） |
| canResumeSession / effectiveEffort / log 输出 | execute.ts L770-826 | 中 | 低（纯决策 + 日志） |
| buildClaudeArgs（带 --resume 的参数构造） | execute.ts L831-870 | 高 | 中（需 MCP/permission 决策） |
| toAdapterResult（统一结果汇总：usage / error / session / biller / costUsd / resultJson 合并） | execute.ts L959-1175 | 最高 | 高（融合 + 大量错误族判断） |
| loginResult / loginMeta 检测 | execute.ts L117、execute.ts L961 | 中 | 低（复用 detect_claude_login_required） |
| errorMessage / fallbackErrorMessage / describeClaudeFailure | execute.ts L867-878、L1033 | 高 | 中 |
| claudeModelUsageTotals fallback | execute.ts L1009-1027 | 中 | 低（复用 claude_model_usage_totals） |
| claudeRefusal / poisoned / maxTurns / failed 决策 | execute.ts L1031-1039 | 高 | 中（纯决策） |
| clearSessionOnMaxTurns / clearSessionForPoisoned / clearSessionOnError | execute.ts L1158-1175 | 高 | 低（决策） |
| bootStrapPromptTemplate / wakePrompt / sessionHandoff / taskContext / prompt 拼接 | execute.ts L789-820 | 中 | 中（模板） |
| promptMetrics | execute.ts L820-826 | 中 | 低（字符串长度） |
| runtimeMcpServers 收集 / runtimeMcpIdentity JSON | execute.ts L506-510 | 中 | 低 |
| remoteConfigDir / runtime_state_dir / writeMcpConfig | execute.ts L515-524 | 中 | 中（需 fs） |
| executionTarget 注入 / remote runtime 准备 / bridge 启动 | execute.ts L435-487、L579-622 | 高（依赖 pc-acpx） | 高（跨 crate 协作） |
| prepareClaudePromptBundle / readSkillEntries | execute.ts L512-514 | 中 | 中（依赖 pc-acpx） |
| localProcessSandbox 选项构造 | execute.ts L530-555 | 中 | 中（依赖 pc-acpx） |
| chmodJsonlPath / unlink poisoned session file | execute.ts L1208-1224 | 低 | 低（需 fs） |

### 3.3 子模块独立性分析

可以完全独立复刻为新模块的部分（无跨模块依赖、纯函数）：

1. **session_resume_decision.rs**（约 200 行 + 12 测试）
   - is_valid_uuid(s)
   - has_matching_prompt_bundle(runtime_key, current_key)
   - has_matching_mcp_servers(runtime_identity, current_identity)
   - decide_claude_session_resume(input) → ResumeDecision { resume_session_id, log_lines }
   - resolve_effective_effort(config, target_is_sandbox, supports_effort)

2. **session_params.rs**（约 120 行 + 8 测试）
   - build_resolved_session_params(input) → Option<serde_json::Value>
   - 输入：session_id, cwd, prompt_bundle_key, mcp_server_identity, workspace_id, repo_url, repo_ref, execution_target_session_identity
   - 输出：完整 sessionParams JSON（对齐 Node L1100-1127）

3. **result_builder.rs**（约 300 行 + 15 测试）
   - ResultBuilder::new(...) → 累计字段
   - decide_error_family(parsed, fallback_error_message, login_required, ...)
   - resolve_error_code(error_family, ...)
   - build_result_json(...) → 合并 resultJson
   - assemble_claude_result(attempt, input) → 完整 AdapterExecutionResult
   - 包含：usage 计算（parsed.usage / fallbackModelUsage / fallbackParsedUsage）、session_id（raw vs resolved）、errorMessage / fallbackErrorMessage / describeClaudeFailure、provider / biller / model / costUsd、clearSession 决策

4. **prompt_sections.rs**（约 150 行 + 10 测试）
   - join_prompt_sections(sections: Vec<Option<&str>>) -> String
   - build_prompt_metrics(sections: &PromptSections) -> PromptMetrics
   - 包含 PromptSections 结构

5. **mcp_config.rs**（约 150 行 + 8 测试）
   - resolve_local_mcp_config_path(state_dir, run_id)
   - collect_runtime_mcp_identity(servers) → JSON string
   - write_paperclip_claude_mcp_config(state_dir, run_id, servers) → path

### 3.4 必须整合的部分

- resume retry loop 主循环（Node L1189-1227 + poisoned session 文件清理） → 改 ClaudeLocalAdapter::execute
- runtime 准备 + bridge 启动 → 调用 pc-acpx 模块

## 4. R461 实施计划

按 6 个独立子模块展开，逐步添加测试，确保每步可独立验证。

| 子模块 | 预计新增测试 | 累计覆盖 |
|---|---|---|
| R461.1 session_resume_decision.rs（决策 + 日志） | +12 | 78% |
| R461.2 session_params.rs（组装） | +8 | 79% |
| R461.3 result_builder.rs（错误族 + usage + session 整合） | +15 | 81% |
| R461.4 prompt_sections.rs（拼接 + metrics） | +10 | 82% |
| R461.5 mcp_config.rs（runtime MCP servers 收集 + 写盘） | +8 | 83% |
| R461.6 ClaudeLocalAdapter::execute 整合（resume retry loop + session_params + result_builder） | +6 | 84% |
| R461.7 claude_runtime.rs（远程 bridge 准备 + 移除 poisoned jsonl） | +5 | 85% |

## 5. 优先级（用户约束）

- adapter 优先只做 claude-local + codex-local（hermes/cursor-cloud/openclaw 等延后）
- 复刻最核心的功能 → 先做 R461.1-R461.4（最关键、最独立、测试覆盖最高）
- 高内聚低耦合 → 每模块独立、可单独测试、对外只暴露纯函数

## 6. 跳过的范围（明确延后）

- codex-local execute.ts 中远程执行路径（execution target + bridge 启动 + stagedCodexHomeDir teardown + restoreRemoteWorkspace） → 需要 pc-acpx::execution_target 完整集成
- claude-local execute.ts 中远程执行路径同上
- codex-home.ts 中 materializeRemoteCodexConfig / materializeRemoteCodexAuthJson 远程分支 → 同上
- quota.ts 完整复刻 → R457 后续
- pc-repos / pc-heartbeat 深化 → R459（已大致完成）
