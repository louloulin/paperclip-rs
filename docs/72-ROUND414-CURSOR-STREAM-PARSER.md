# R414：cursor-local stream-json parser 复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/cursor-local/src/server/parse.ts`
- `paperclip/packages/adapters/cursor-local/src/shared/stream.ts`
- `paperclip/packages/adapters/cursor-local/src/server/parse.test.ts`

Rust 原实现已支持 system/assistant/result/error 等核心事件，但缺 Node 的 `normalize_cursor_streamLine`、`step_finish` cost/usage 累加和 `isCursorUnknownSessionError`。

## 实现内容

新增 `crates/pc-adapter-cursor-local/src/cursor_stream_json.rs`：

- `normalize_cursor_stream_line`
- `parse_cursor_stream_json`
- `is_cursor_unknown_session_error`
- 解析 `system`、`assistant`、`result`、`error`、`text`、`step_finish`
- 累加多种 usage wire format 和 cost
- 在 result 文本优先时覆盖 assistant 摘要，保留 Node fallback
- 解析结构化 error 和 cost JSON

真实 `CursorLocalAdapter::execute` 已切换到新 parser 提取 session_id、usage、cost、result_json 等结构化数据。

## 验证

- `cargo test -p pc-adapter-cursor-local`：全量通过。
- `cargo test -p pc-adapter-cursor-local --test round414_cursor_stream_parser`：6 passed。
- 覆盖 stream 前缀、result 覆盖、step_finish usage/cost 累加、错误事件、session 不可恢复和非法行忽略。
- `cargo check --workspace`：workspace 编译验证。

## 剩余差距

- Cursor Node 错误正则仍覆盖更多自然语言变体，Rust 当前覆盖生产核心文案。
- Cursor 的 resume/clearSession/ACP fallback 仍需真实 CLI/API 验证。
