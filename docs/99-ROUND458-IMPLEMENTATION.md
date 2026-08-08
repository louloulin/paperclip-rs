# R458 — Claude testEnvironment 决策表（Rust 化）

## 目标

把 Node `claude-local/server/test.ts` 中的 **纯决策逻辑** 复刻为 Rust：
- `summarizeStatus`、`isNonEmpty` 等 guards
- `firstNonEmptyLine`、`lastNonInitStdoutLine`、`truncateDetail`、`summarizeProbeDetail` 等文本解析
- `canRunProbe` 前置错误码过滤
- `detectBedrockAuth` / `detectAnthropicApiKey` 环境变量检测
- `helloProbeOutcome` 5 分支决策（timed_out / auth_required / passed / unexpected_output / failed）

依赖：现有 `parse.ts` Rust 端口（`claude_errors.rs`、`claude_stream_json.rs`）。

### 设计目标

1. **零 I/O 依赖**：本模块只包含纯函数；真实 probe / sandbox install / ssh 由 `pc-acpx::execution_target` + `pc-adapter-process` 提供
2. **决策可独立测试**：每个 check 的生成逻辑都可单元测试，无需 mock 子进程
3. **route 层零额外工作**：`pc-http` `test_environment` 路由只调本模块的决策 + acpx 的执行器

---

## Node → Rust 端口

### `summarizeStatus` → `summarize_status`

```rust
pub fn summarize_status(checks: &[AdapterEnvironmentCheck]) -> TestStatus {
    if checks.iter().any(|c| c.level == CheckLevel::Error) { TestStatus::Fail }
    else if checks.iter().any(|c| c.level == CheckLevel::Warn) { TestStatus::Warn }
    else { TestStatus::Pass }
}
```

### `lastNonInitStdoutLine` → `last_non_init_stdout_line`

跳过 `{"type":"system","subtype":"init"}` 行，找到最后一个其他行。

### `summarizeProbeDetail` → `summarize_probe_detail`

从 stdout / stderr 找首个 `system/init` event 的 `message` 字段。

### `canRunProbe` → `can_run_probe`

哪些 check code 触发跳过 probe：
- `claude_cwd_invalid`
- `claude_command_unresolvable`
- `claude_managed_config_dir_failed`

### `helloProbeOutcome` → `hello_probe_outcome`

5 分支决策：

| 分支 | level | 说明 |
|---|---|---|
| `TimedOut` | warn | `probe.timedOut == true` |
| `AuthRequired { login_url }` | warn | stdout/stderr 含 login-required 短语 |
| `Passed { detail }` | info | exit_code 0 且 result 含 `hello` |
| `UnexpectedOutput { detail }` | warn | exit_code 0 但无 `hello` |
| `Failed { detail, transient }` | error / warn | 退出非 0，transient 决定 level |

---

## 测试覆盖（38 个新增）

### summarize_status（4）
- 仅 info → pass
- 含 warn → warn
- 含 error → fail
- 空 → pass

### 文本解析（10）
- `is_non_empty` × 3（truthy / 空 / 空白）
- `first_non_empty_line` × 3
- `last_non_init_stdout_line` × 3
- `summarize_probe_detail` × 4
- `truncate_detail` × 3

### can_run_probe（4）
- 无 blocking error → true
- `claude_cwd_invalid` → false
- `claude_command_unresolvable` → false
- `claude_managed_config_dir_failed` → false

### bedrock / api_key 检测（7）
- Bedrock: `CLAUDE_CODE_USE_BEDROCK=1` / `ANTHROPIC_BEDROCK_BASE_URL` / truthy 变体 / 无 env
- API key: `ANTHROPIC_API_KEY=sk-test` / 空 / 缺失

### hello_probe_outcome（5）
- timed_out
- auth_required (含 login_url)
- passed (含 hello)
- unexpected_output (无 hello)
- failed (non_transient / transient)

### is_login_required（2）
- 多种短语匹配
- 正常输出不匹配

---

## 文件清单

- **新建**：`crates/pc-adapter-claude-local/src/claude_test.rs`（635 行）
- **修改**：`crates/pc-adapter-claude-local/src/lib.rs`（注册 `pub mod claude_test;`）
- **依赖**：复用现有 `regex-lite`（已在 R457 加入）

## 测试结果

```
claude_test::tests: 38 passed, 0 failed
pc-adapter-claude-local: 200 passed (162 prior + 38 new)
pc-acpx: 883 passed
pc-adapter-codex-local: 260 passed
pc-adapter-process: 6 passed (一次 flaky 重跑通过)
pc-activity: 14 passed
pc-adapter-quota: 39 (上次验证)
合计: 1638 passed (was 1600, +38)
```

---

## 后续 R459+

- **R459** pc-repos / pc-heartbeat 深化
- **R460** 把 `testEnvironment` 端到端集成到 `pc-http` route（已具备决策表，只差 wiring）
- **R461** execute.ts 完整复刻（claude-local 1270 行，最大单文件缺口）

## 当前差距

| 维度 | 已经实现 | 后续 |
|---|---|---|
| codex 适配器 | ~98% | （接近完成） |
| **claude 适配器** | **~96%** | R461 |
| pc-acpx 核心 | ~95% | （少量边界） |
| pc-http routes | ~96% | R460 |
| quota / heartbeat | ~85% | R457（已部分） |
| 其他 adapter | 0% | R456（延后） |
