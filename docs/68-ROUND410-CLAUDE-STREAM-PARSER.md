# R410：claude-local stream-json parser 复刻

## 差距依据

对照 Node 文件：

- `paperclip/packages/adapters/claude-local/src/server/parse.ts`
- `paperclip/packages/adapters/claude-local/src/server/parse.test.ts`

Rust 原实现已有早期 `parse_claude_jsonl`，但它面向旧的 Paperclip 事件命名，只提取少量 token 字段，缺少 Node 当前 Claude CLI `stream-json` 的最终结果语义、per-model usage ledger 以及登录/恢复错误判断。

## 实现内容

新增 `crates/pc-adapter-claude-local/src/claude_stream_json.rs`：

- `parse_claude_stream_json`：解析 `system/init`、`assistant`、`result` 事件。
- `claude_model_usage_totals`：跨模型汇总 usage，`cacheCreationInputTokens` 计入输入，`cacheReadInputTokens` 单独作为缓存输入。
- result usage 回退到 `input_tokens`、`output_tokens`、`cache_read_input_tokens`。
- 解析 session、model、cost、stop reason、result JSON 和 error message。
- 无 result 时用 assistant text 作为 fallback summary。
- 实现登录要求、登录 URL、未知 session、图片处理错误识别。

真实 execute 路径已切换到新 parser，同时保留旧 `parse_claude_jsonl` API 兼容已有调用者和历史输出。

## 验证

- `cargo test -p pc-adapter-claude-local`：26 个单测、2 个既有集成测试、11 个 skills 集成测试全部通过。
- `cargo test -p pc-adapter-claude-local --test round410_claude_stream_parser`：6 个新增集成测试全部通过。
- 覆盖 init/assistant/result 链路、结果 session 覆盖、modelUsage 优先、无 result fallback、登录 URL、未知 session、图片错误、非法事件。
- 后续执行 workspace check，确保新增模块不破坏其它 crate。

## 剩余差距

Node parser 还包含更细的 transient upstream、provider quota、model-not-found、max-turns、refusal、poisoned previous message、reset 时间解析等分类；本轮先完成主解析和高频恢复判断，剩余分类将在后续错误策略模块中继续按纯函数拆分复刻。
