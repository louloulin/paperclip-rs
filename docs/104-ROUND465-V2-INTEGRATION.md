# R465 完成 — buildClaudeArgs v2 集成

## 1. 目标

把 `build_claude_args_v2`（R463）从独立模块状态接入 `execute_with_resume_retry` 主流程，
实现真正的 Node `buildClaudeArgs` 闭包语义（--chrome / --max-turns / --strict-mcp-config / Bedrock gating / resume skip instructions）。

## 2. 实施

### 2.1 新增 wrapper 函数

`lib.rs::build_claude_exec_args_v2(config, cwd, resume_session_id, is_bedrock_auth) -> ClaudeExecArgs`

从 adapter_config + context 提取字段，构造 `ClaudeArgsInput`，调用 `build_claude_args_v2`，
返回与 `build_claude_exec_args` 同型的 `ClaudeExecArgs` struct。

### 2.2 execute_with_resume_retry 切换到 v2

```rust
// 之前：
let built = build_claude_exec_args(&context.adapter_config);

// 现在：
let is_bedrock_auth = crate::claude_models::is_bedrock_env(&context.env);
let effective_execution_cwd_pre = context.cwd.as_ref().map(|p| ...);
let built = build_claude_exec_args_v2(
    &context.adapter_config,
    &effective_execution_cwd_pre,
    None,        // 初始 args 不含 --resume（resume 决策后由 resume loop 追加）
    is_bedrock_auth,
);
```

### 2.3 新增测试

| 测试 | 验证 |
|---|---|
| `build_claude_exec_args_v2_minimal` | 空 config 只产生 --print / --output-format / --verbose |
| `build_claude_exec_args_v2_full_features` | 全 feature 开启时 args 顺序对齐 Node |
| `build_claude_exec_args_v2_with_resume_skips_instructions` | resume 时跳过 --append-system-prompt-file |
| `build_claude_exec_args_v2_bedrock_auth_skips_anthropic_short_model` | Bedrock auth + 非 native model → 跳过 --model |
| `build_claude_exec_args_v2_bedrock_auth_keeps_bedrock_native` | Bedrock auth + native model → 保留 --model |

## 3. 测试快照

| Crate | R463+R464 后 | R465 后 | Δ |
|---|---|---|---|
| pc-acpx | 883 | 883 | 0 |
| pc-adapter-codex-local | 305 | 305 | 0 |
| **pc-adapter-claude-local** | **432** | **437** | **+5** |
| pc-activity | 14 | 14 | 0 |
| pc-adapter-process | 6 | 6 | 0 |
| pc-adapter-quota | 39 | 39 | 0 |
| **合计** | **1679** | **1684** | **+5** |

## 4. 现状总结

`execute_with_resume_retry` 现在使用完整 Node `buildClaudeArgs` 语义：
- ✅ `--chrome` flag（config.chrome）
- ✅ `--max-turns`（config.maxTurns > 0 时）
- ✅ `--strict-mcp-config`（与 --mcp-config 配对）
- ✅ `--model` 在 Bedrock auth 模式下 gating
- ✅ `--append-system-prompt-file` 在 resume 时跳过
- ✅ args 顺序严格对齐 Node L831-870
