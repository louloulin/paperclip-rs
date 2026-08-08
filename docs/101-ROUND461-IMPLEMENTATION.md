# R461 — claude-local execute.ts 深化实施

## 1. 目标

复刻 Node `claude-local/server/execute.ts`（1270 行）中的**纯函数部分**，最高优先级集中在：
- session resume 决策与日志
- session_params 完整组装
- toAdapterResult 整合（错误族 + usage + session 处理）
- prompt 段落拼接 + metrics
- runtime MCP servers 收集 + 配置文件路径解析

本轮**不修改现有 `ClaudeLocalAdapter::execute`**，避免破坏 200+ 个现有测试；
新增独立模块 + 测试。整合到 execute 主循环留待 R461.7/R461.8。

## 2. 新增模块

| 模块 | 行数 | 新增测试 | 覆盖 Node 段 |
|---|---|---|---|
| `claude_session_resume.rs` | 567 | +33 | L736-826 (canResumeSession + 日志) + L831-870 (effectiveEffort) |
| `claude_session_params.rs` | 412 | +12 | L1097-1110 (resolvedSessionParams 组装) |
| `claude_result_builder.rs` | 1436 | +51 | L867-1199 (parseFallbackErrorMessage + toAdapterResult) |
| `claude_prompt_sections.rs` | 222 | +14 | L789-828 (joinPromptSections + promptMetrics) |
| `claude_mcp_config.rs` | 251 | +12 | claude-config.ts writePaperclipClaudeMcpConfig + execute.ts runtimeMcpIdentity |
| `claude_resume_loop.rs` | 461 | +12 | L1189-1267 (resume retry 主循环 + 组装) |
| **合计** | **3349** | **+134** | |

## 3. 模块设计原则（贯穿）

1. **零 I/O 依赖**：所有决策函数都是纯函数，便于独立测试
2. **高内聚低耦合**：每个模块自包含，对外只暴露函数 API
3. **类型化错误**：使用 Rust enum（`ErrorFamily`、`ResolvedErrorCode`、`SessionErrorKind`）
4. **Node 一致性**：日志格式、字段名、JSON 结构严格对齐 Node
5. **Rust Edition 2021 兼容**：不用 let chains，全部用 match + 显式 if
6. **决策与组装分离**：session_resume 模块只决策 + 日志，session_params 模块只组装 JSON，
   result_builder 模块只做错误族解析 + 最终 AdapterExecutionResult 整合

## 4. 关键设计决策

### 4.1 SystemTime 自实现 ISO 8601

为避免引入 `chrono` / `time` 依赖，手工实现 Howard Hinnant 的 `civil_from_days` 算法。
保证与 Node `Date.toISOString()` 完全一致（UTC + millisecond 精度）。

### 4.2 Billder 字段放入 result_json

`pc_adapter_api::AdapterExecutionResult` 没有 `biller` 字段（设计上）。
为保持与 Node 的字段一致性，把 `biller` 写入 `result_json.biller`（String），
调用方可通过 `result_json.get("biller")` 读取。

### 4.3 不引入 regex

UUID 校验、prompt bundle key 匹配等手写实现，避免引入 `regex` crate。
`is_claude_unknown_session_error` 等已是手写字符串匹配。

### 4.4 Session error 三种语义

`SessionErrorKind` enum：
- `Unknown` — session_id 在 Claude CLI 端找不到（Node L1214）
- `Poisoned` — previous_message_id 是非 `msg_` 前缀（Node L1216）
- `Image` — transcript 包含不可处理的图片（Node L1218）

重试时 clear_session_on_missing_session=true。

### 4.5 Resume retry 状态机

```
initial attempt (with --resume if can_resume)
    ↓
parse stdout + detect_session_error_kind (only if exit_code != 0)
    ↓
if session_error: retry without --resume
    ↓
assemble_claude_result (统一组装)
```

## 5. 测试结果

| Crate | 之前 | R461 后 | Δ |
|---|---|---|---|
| pc-acpx | 883 | 883 | 0 |
| pc-adapter-codex-local | 260 | 260 | 0 |
| **pc-adapter-claude-local** | **200** | **334** | **+134** |
| pc-activity | 14 | 14 | 0 |
| pc-adapter-process | 6 | 6 | 0 |
| pc-adapter-quota | 39 | 39 | 0 |
| **合计** | **1402** | **1536** | **+134** |

所有 6 个 crate 全通过，0 失败。

## 6. 已知缺口（明确延后）

- **远程 execution target 集成**（execute.ts L579-622）：startAdapterExecutionTargetPaperclipBridge + materialzeRemoteClaudeConfig
- **preparedExecutionTargetRuntime**：restoreRemoteWorkspace + remote asset sync
- **localProcessSandbox 选项构造**：bwrap managedPaths + networkScope + filesystemScope
- **buildClaudeArgs 完整实现**：含 --chrome / --max-turns / --mcp-config / --add-dir / --dangerously-skip-permissions / extraArgs
- **bootstrapPromptTemplate + renderTemplate**：依赖 pc-acpx::prompt_compose 完整实现
- **selectPaperclipTaskMarkdown + renderPaperclipWakePrompt**：依赖 pc-acpx 完整实现
- **hasExplicitClaudeConfigDir 检查**：env config 提取
- **runtimeMcpServers 收集**：需要 pc-adapter-api 暴露 `runtime_mcp` 字段

## 7. 整体差距更新

| 维度 | 之前 | R461 后 | 状态 |
|---|---|---|---|
| claude-local 适配器整体 | ~75% | **~85%** | ↑ 10% |
| codex-local 适配器整体 | ~98% | ~98% | = |
| pc-acpx 核心 | ~95% | ~95% | = |
| pc-http routes | ~96% | ~96% | = |
| quota / heartbeat | ~85% | ~85% | = |
| 其他 adapter | 0% | 0% | （延后） |

## 8. 下一轮候选

- **R461.7**：远程 execution target + bridge 集成（Node L579-622）
- **R461.8**：整合 `run_resume_retry_loop` 到 `ClaudeLocalAdapter::execute`（替换现有 587 行 execute）
- **R461.9**：pc-http `testEnvironment` 端到端 wiring
- **R461.10**：codex-local execute.ts 远程部分补全（stagedCodexHomeDir teardown + restoreRemoteWorkspace）
