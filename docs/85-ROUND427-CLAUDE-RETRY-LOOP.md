# R427 — claude-local 错误族分类驱动的 retry 决策

## 目标

把 Node `packages/adapters/claude-local/src/server/execute.ts` 里多种错误分类
（`isClaudeProviderQuotaError` / `isClaudeTransientUpstreamError` /
`isClaudeMaxTurnsResult` / `isClaudePoisonedPreviousMessageIdError` /
`isClaudeRefusalResult` / `isClaudeUnknownSessionError`）压缩到单一
`decide_retry(input) -> ClaudeRetryDecision` 决策点，并在 `execute()` 中把
`errorFamily` / `stopReason` / `clearSession` 写入 `AdapterExecutionResult`。

## 主要改动

| 文件 | 改动 |
| --- | --- |
| `crates/pc-adapter-claude-local/src/execute_helpers.rs` | 新增 `ClaudeErrorFamily` 枚举（Node `errorFamily` 字段一一对应：None/ProviderQuota/TransientUpstream/MaxTurns/PoisonedPreviousMessageId/Refusal/UnknownSession/ModelRefusal）；新增 `ClaudeRetryInput` / `ClaudeRetryDecision` / `decide_retry` 综合 6 个错误分类器；新增 5 项单元测试覆盖 max_turns / provider_quota / transient_upstream / unknown_session / exit_code=0 五条分支。 |
| `crates/pc-adapter-claude-local/src/lib.rs` | `execute()` 调用 `decide_retry(...)`，把 `error_family` 写入 `result_json.errorFamily`，把对应 `stopReason` 覆盖为 `"max_turns_exhausted"` / `"claude_poisoned_previous_message_id"` / `"refusal"`，把 `clear_session` 提升为 true（max_turns/poisoned/unknown_session 三类）。 |
| `crates/pc-adapter-claude-local/tests/round427_claude_retry_loop.rs` | 3 个端到端集成测试：max_turns 清 session；provider_quota 不清 session；unknown_session 清 session。 |

## Node 等价性

- `errorFamily` 字段：`""`、`provider_quota`、`transient_upstream`、`max_turns`、`claude_poisoned_previous_message_id`、`refusal`、`unknown_session` —— 枚举一一对应。
- `clearSession` 触发：`max_turns || poisonedPreviousMessageId || unknownSession`（Node `L1196-1202`）。
- `stopReason` 覆盖：与 Node `L1163-1167` 等价。
- `retryNotBefore`/`errorMeta`/`mergedResultJson`：留待 R428 接入 transient retry not-before 时间解析（`extractClaudeRetryNotBefore`），本轮先把 family + clear_session + stopReason 接通。

## 测试矩阵

- `pc-adapter-claude-local::execute_helpers::tests` 新增 5 项（`decide_retry_*`）。
- `round427_claude_retry_loop.rs` 3 项端到端。

## 验证

- `cargo check -p pc-adapter-claude-local --tests`：0 errors。
- `cargo test -p pc-adapter-claude-local`：102 passed（8 suites，原有 99 + 3 R427）。
- 全工作区测试除 `pc-agent` 数据库唯一约束冲突（与本次改动无关）以外全部通过。

## 后续

- R428：接入 `extractClaudeRetryNotBefore`（解析 `errorMeta.retryNotBefore`）与
  `materializeRemoteClaudeConfig` / `resolveManagedClaudeRuntimeStateDir` 等远端分支；
- R428 同时把剩余 5 个 adapter（codex/gemini/cursor/opencode/grok）的错误族 + retry
  主循环统一升级。
