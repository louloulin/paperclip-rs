# R415：opencode-local stream-json parser 复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/opencode-local/src/server/parse.ts`
- `paperclip/packages/adapters/opencode-local/src/server/parse.test.ts`

Rust 原实现只有 legacy `parse_opencode_output`，按文本字段取最后一处值，缺少 Node 的：
- `text`、`step_finish` 完整 token/cost 累加。
- `tool_use` 失败与主错误分离。
- `isOpenCodeUnknownSessionError`。

## 实现

新增 `crates/pc-adapter-opencode-local/src/opencode_stream_json.rs`：

- `parse_opencode_stream_json`：解析 `text`、`step_finish`、`tool_use`、`error`。
- 累加 tokens：
  - input
  - output + reasoning
  - cache.read
- 累加 cost。
- 解析 sessionID。
- 区分 toolErrors 与 errorMessage。
- 嵌套 `error.data.message` 作为错误文本。
- `is_opencode_unknown_session_error` 识别 unknown session / notfounderror / resource not found。

真实 `OpencodeLocalAdapter::execute` 已切换到新 parser，输出包含 `session_id`、`usage`、`cost_usd`、`error_message` 和 `toolErrors`。

## 验证

- `cargo test -p pc-adapter-opencode-local`：全量通过。
- `cargo test -p pc-adapter-opencode-local --test round415_opencode_stream_parser`：5 passed。
- 覆盖 text+step_finish+error 合并、tool_use 与主错误分离、嵌套 data.message、unknown session、非法行忽略。
- `cargo check --workspace`：workspace 编译验证。

## 剩余差距

- Node 错误正则仍覆盖更多自然语言变体，Rust 当前覆盖生产核心文案。
- OpenCode 远程执行、resume、ACP fallback 仍需真实 CLI 验证。
