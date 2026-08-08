# R464 + R463 完成报告 — claude-local 深化

## 1. R464 — poisoned session file cleanup

### 1.1 目标

对齐 Node execute.ts L1233-1248：当 session 错误为 `poisoned`（previous_message_id 不以 `msg_` 开头）时，
主动清理 Claude CLI 在 `~/.claude/projects/<encoded_cwd>/<session_id>.jsonl` 的会话缓存文件，
避免下次 `--resume` 仍然命中坏状态。

### 1.2 新增模块

`crates/pc-adapter-claude-local/src/claude_session_cleanup.rs`（167 行）

提供：
- `encode_project_cwd(cwd)` — 模拟 Claude Code project-dir 编码规则（非字母数字 → `-`）
- `build_poisoned_jsonl_path(config_dir, cwd, session_id)` — 计算目标路径
- `unlink_poisoned_session_file(config_dir, cwd, session_id)` — async best-effort unlink

### 1.3 集成

`claude_resume_loop.rs` 中，retry 块开头增加：

```rust
if matches!(session_error, Some(SessionErrorKind::Poisoned)) && !input.execution_target_is_remote {
    let claude_config_dir = resolve_shared_claude_config_dir(...);
    match unlink_poisoned_session_file(...).await {
        Ok(true) => { emit log "[paperclip] Removed poisoned session file: ..." }
        Ok(false) => { /* 文件不存在，不是错误 */ }
        Err(_) => { /* best-effort */ }
    }
}
```

### 1.4 新增测试

| 测试 | 类型 | 数量 |
|---|---|---|
| `claude_session_cleanup.rs` 内部单元测试 | encode + path + unlink | 8 |
| `tests/round464_claude_session_cleanup.rs` 集成测试 | 真实 fs 操作 | 4 |
| **R464 增量** | | **+12** |

## 2. R463 — buildClaudeArgs 完整实现

### 2.1 目标

对齐 Node execute.ts L831-870 完整 `buildClaudeArgs` 闭包逻辑：
- `--chrome` flag
- `--max-turns`（仅当 > 0）
- `--strict-mcp-config`（与 `--mcp-config` 配对）
- `--model` 在 Bedrock auth 模式下的 gating
- `--append-system-prompt-file` 在 resume 时跳过

### 2.2 新增模块

`crates/pc-adapter-claude-local/src/claude_cli_args.rs`（354 行）

提供：
- `ClaudeArgsInput` — 输入聚合结构
- `should_pass_model_for_bedrock(model, is_bedrock_auth)` — Bedrock 决策（独立可测）
- `build_claude_args_v2(input)` — 完整 args 构造（按 Node 顺序）

### 2.3 新增测试

20 个内部单元测试覆盖：
- base / resume / chrome / max-turns（正/负/零）/ mcp-config / bedrock gating（4 场景）/ 
  append-system-prompt resume 跳过 / add-dir / dangerously-skip-permissions / extra_args / 完整 args 顺序

### 2.4 待集成

`build_claude_args_v2` 目前为独立模块（+ 20 测试）。后续可替换 `execute_with_resume_retry` 中的
`build_claude_exec_args` 调用，实现真正的 Node `buildClaudeArgs` 闭包语义。
当前默认走 `build_claude_exec_args`（更简单版本）以保持现有 432 个测试稳定。

## 3. 测试快照

| Crate | R461 后 | R463+R464 后 | Δ |
|---|---|---|---|
| pc-acpx | 883 | 883 | 0 |
| pc-adapter-codex-local | 260 | 305 | +45（既有集成测试补充） |
| **pc-adapter-claude-local** | **344** | **432** | **+88** |
| pc-activity | 14 | 14 | 0 |
| pc-adapter-process | 6 | 6 | 0 |
| pc-adapter-quota | 39 | 39 | 0 |
| **合计** | **1546** | **1679** | **+133** |

## 4. 后续计划

### R466 — 集成 v2 + remote execution 基础
1. 将 `build_claude_args_v2` 接入 `execute_with_resume_retry`
2. 删除 `build_resume_claude_args` 间接层
3. 保持所有 432+ 测试通过

### R467 — pc-http testEnvironment 端到端 wiring
- `/v1/test-environment` 路由
- 调用 claude_test::hello_probe_outcome + acpx hello probe

### R468 — codex-local 远程补全
- stagedCodexHomeDir teardown
- restoreRemoteWorkspace
- remoteCodexConfigDir 决策
