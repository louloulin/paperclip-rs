# R411：Claude 错误分类与重试提示复刻

## 目标

继续复刻 Node `packages/adapters/claude-local/src/server/parse.ts` 中独立于进程执行的错误分类逻辑，以纯函数模块保持高内聚、低耦合。

## 实现

新增 `crates/pc-adapter-claude-local/src/claude_errors.rs`：

- `describe_claude_failure`
- `is_claude_model_not_found_error`
- `is_claude_max_turns_result`
- `is_claude_refusal_result`
- `is_claude_poisoned_previous_message_id_error`
- `is_claude_transient_upstream_error`
- `is_claude_provider_quota_error`
- `is_claude_unknown_session_error`
- `is_claude_login_required`
- `extract_claude_retry_not_before`

分类顺序保留 Node 的关键互斥关系：确定性失败、登录错误和 provider quota 不会被错误归类为普通 transient upstream。

重试时间支持 `4pm`、`3:15 AM`，当天时间已过时滚动到次日。当前实现按 UTC Unix 日计算，未引入重量级时区数据库；Node 的 IANA timezone wall-clock 转换仍是后续差距。

## 验证

- `cargo test -p pc-adapter-claude-local --test round411_claude_errors`：7 passed。
- 覆盖 failure 描述、模型不存在、max turns、refusal、previous_message_id 污染、transient/quota 互斥、登录排除和重试时间滚动。
- `cargo test -p pc-adapter-claude-local`：全量验证。
- `cargo check --workspace`：workspace 编译验证。

## 剩余差距

- Node `extractClaudeRetryNotBefore` 支持 IANA 时区和 DST；Rust 当前仅提供 UTC/固定 Unix 日语义。
- Node 正则允许更多自然语言、标点和 Unicode 撇号变体；Rust 已覆盖核心生产文案，后续可通过 fixture parity 测试继续扩展。
- 错误分类尚未接入上层 heartbeat recovery policy；当前模块保持协议层纯函数边界。
