# R409：grok-local JSONL parser 复刻

## 差距依据

Node 参考实现：

- `paperclip/packages/adapters/grok-local/src/server/parse.ts`
- `paperclip/packages/adapters/grok-local/src/server/parse.test.ts`
- `paperclip/packages/adapters/grok-local/src/shared/turn-boundary.ts`

Rust 原实现只有 `parse_grok_output`，按若干历史 JSON 形状取最后一个文本字段，无法完整表达 Node parser 的 session、thought、error、stopReason、requestId，也没有未知 session 错误识别。

## 实现内容

新增 `crates/pc-adapter-grok-local/src/grok_jsonl.rs`：

- `ParsedGrokJsonl` 结构化结果。
- `parse_grok_jsonl`：逐行解析 `thought`、`text`、`end`、`error` 事件。
- `error_text`：支持字符串错误和 message/error/detail/code 对象错误。
- `is_grok_unknown_session_error`：识别恢复会话失效。
- Rust 复刻 Node turn-boundary 算法，仅对 thought 流插入跨回合换行。

执行路径改为使用结构化 parser，并将 session、错误、summary 和 `thought/stopReason/requestId` 写入现有 `AdapterExecutionResult`，不改变外部 trait。

历史 `parse_grok_output` 保留兼容 fallback，避免旧输出格式回归。

## 验证

- `cargo test -p pc-adapter-grok-local`：14 个单测、1 个既有集成测试和 doc tests 全部通过。
- `cargo test -p pc-adapter-grok-local --test round409_grok_parser`：7 个集成测试全部通过。
- 覆盖流式聚合、结束元数据覆盖、结构化错误、非法行、thought 回合边界、未知 session 错误和 legacy 输出兼容。

## 后续差距

仍需继续复刻其它 adapter parser（优先 `claude-local`、`codex-local`、`gemini-local`），随后将 parser 结果完整接入各自 execute；再处理远程执行、ACP fallback、session resume 和真实 CLI smoke 验证。
