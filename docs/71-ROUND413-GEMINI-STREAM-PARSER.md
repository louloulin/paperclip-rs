# R413：gemini-local stream-json parser 复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/gemini-local/src/server/parse.ts`
- `paperclip/packages/adapters/gemini-local/src/server/parse.test.ts`

Rust 原实现只有 legacy `parse_gemini_output`，按历史字段取最后文本，缺少 Gemini CLI 当前 stream-json 的离散 assistant message、session 多字段、usage 累加、question 交互和错误分类。

## 实现内容

新增 `crates/pc-adapter-gemini-local/src/gemini_stream_json.rs`：

- 解析 `assistant`、`message(role=assistant)`、`result`、`error`、`system/error`、`text`、`step_finish`。
- 支持 `session_id`、`sessionId`、`sessionID`、`checkpoint_id`、`thread_id`。
- 解析文本、question prompt、choice key/label/description。
- 累加多种 usage wire format：snake_case、camelCase、Gemini `usageMetadata`、stats。
- 提取 cost、result JSON 和结构化错误。
- 实现 session unrecoverable、transient network、auth、quota、turn limit 判断。

真实 `GeminiLocalAdapter::execute` 已切换到新 parser，旧 `parse_gemini_output` 保留为 legacy API 兼容层。

## 验证

- `cargo test -p pc-adapter-gemini-local`：全量通过。
- `cargo test -p pc-adapter-gemini-local --test round413_gemini_stream_parser`：7 passed。
- 覆盖 message/assistant 聚合、session 覆盖、多种 usage 累加、cost、question、错误、auth/quota、网络瞬态、session 不可恢复和 turn limit。
- `cargo check --workspace`：workspace 编译验证。

## 剩余差距

- Gemini Node 的错误正则仍覆盖更多自然语言变体，Rust 当前覆盖生产核心文案。
- question 尚未接入上层交互 continuation policy。
- Gemini 真实 CLI 的 auth、quota、resume 和 turn-limit smoke 仍需外部 CLI 环境。
