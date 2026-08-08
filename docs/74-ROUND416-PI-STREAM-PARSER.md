# R416：pi-local stream-json parser 复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/pi-local/src/server/parse.ts`
  - `parsePiJsonl`（事件类型分流）
  - `isPiUnknownSessionError`（未知 session 提示识别）

Rust 原实现只有 legacy `parse_pi_output`，按文本字段取最后一处值，缺少 Node 的：

- RPC 内部事件（`response`/`extension_*`/`agent_start`/`turn_start`）跳过。
- `agent_end` 取最后一条 assistant content 作为 `finalMessage`。
- `auto_retry_end` 失败时把 `finalError` 加入 errors。
- `turn_end` 累加 usage/cost，并把 `toolResults[]` 写回对应 toolCall。
- `message_update.text_delta` 追加到 `messages` 最后一条。
- `tool_execution_start`/`end` 维护 `toolCalls`（按 `toolCallId` 匹配，含兜底创建）。
- `usage` / `event.usage` 累加（兼容 Pi 原生格式 `input/output/cacheRead/cost.total` 与 generic 格式 `inputTokens/outputTokens/cachedInputTokens/costUsd`）。
- `isPiUnknownSessionError` 识别 unknown session / session not found / session X not found / no session。

## 实现

新增 `crates/pc-adapter-pi-local/src/pi_stream_json.rs`：

- `PiToolCall`：单条 tool call 完整记录（toolCallId / toolName / args / result / isError）。
- `PiUsage`：Pi usage 累加（input / output / cachedInput / costUsd）。
- `ParsedPiOutput`：`parse_pi_jsonl` 的完整结果，含 session_id / messages / errors / usage / finalMessage / toolCalls。
- `to_usage_summary`：将 `PiUsage` 映射到 `pc_adapter_api::UsageSummary`。
- `parse_pi_jsonl`：按行解析 JSONL，分流所有事件类型。
- `is_pi_unknown_session_error`：识别 Pi 报错的"未知 session"提示。

事件分流行为：

| 事件类型 | 行为 |
|---|---|
| `response`/`extension_ui_request`/`extension_ui_response`/`extension_error`/`agent_start`/`turn_start` | 跳过（RPC 内部协议） |
| `agent_end` | 取 `messages[]` 最后 assistant content |
| `auto_retry_end` | `success !== true` → errors.push(finalError 或默认提示) |
| `turn_end` | 提取 content → messages.push + finalMessage；累加 message.usage；toolResults 写回已有 toolCall |
| `message_update` | assistantMessageEvent.type == "text_delta" → 追加 delta 到 messages 末尾 |
| `tool_execution_start` | push 新 PiToolCall（toolCallId/toolName/args） |
| `tool_execution_end` | 按 toolCallId 匹配已有 toolCall 写回 result/isError；无匹配且 toolName 非空则兜底创建 |
| `usage` 或 `event.usage` | 兼容 Pi 格式 + generic 格式累加 input/output/cacheRead/cost |
| `error` | 非空 message → errors.push |

真实 `PiLocalAdapter::execute` 已切换到新 parser，输出包含：

- `session_id`：从顶层 `sessionId`/`sessionID` 取首个非空。
- `usage`：`UsageSummary { input_tokens, output_tokens, cached_input_tokens }`。
- `cost_usd`：> 0 时写入。
- `error_message`：parser errors 优先；非零退出且 stderr 非空时也写入。
- `summary`：finalMessage 优先；否则拼接 messages。
- `result_json`：`{ "toolCalls": [...], "messages": [...], "errors": [...] }`。

`sessionId` 顶层命名兼容多种（`sessionId` / `sessionID`），session_id 取首个非空值。

## 验证

- `cargo test -p pc-adapter-pi-local`：全量 73 passed（34 lib + 1 round395 + 10 adapter_real + 28 round416）。
- `cargo test -p pc-adapter-pi-local --lib pi_stream_json`：14 passed（涵盖 turn_end / agent_end / auto_retry_end / content array 提取 / tool execution state machine / message_update text delta / standalone usage 两种格式 / error 事件 / RPC 跳过 / unknown session / 非 JSON 忽略 / to_usage_summary）。
- `cargo test -p pc-adapter-pi-local --test round416_pi_stream_parser`：28 passed（涵盖事件分流各路径、toolCall 匹配、turn_end usage 与 standalone usage 叠加、sessionId 多命名兼容、空输入默认值）。
- `cargo check --workspace --tests`：workspace 编译验证通过。

## 兼容性

- legacy `parse_pi_output` 保留，老 fixture 不破坏（返回 `Option<String>`，按字段命中取最后一处值）。
- 新 `parse_pi_jsonl` 是权威路径，execute 已切换。

## 关键设计决策

- **`raw_string_field` 单独 helper**：流式 text delta 的 `assistantMessageEvent.delta` 不能 trim，否则会丢失末尾空格（Node `asString` 同样不 trim）。
- **`extract_text_content` 用空串 join**：Node 用 `arr.filter().map().join("")` 直接拼接，保留原文空白；Rust 不能用 `"\n"` join，否则会改变语义。
- **`tool_execution_end` 兜底创建**：某些 Pi 实现只发 end 不发 start，按 toolName 非空判断并补一条记录，避免丢工具调用。
- **`accumulate_usage` 兼容 Pi + generic**：合并 `input + inputTokens`、`output + outputTokens`、`cacheRead + cachedInputTokens`，cost 优先 `cost.total` 再回退 `costUsd`。
- **`cost_usd` 在 `AdapterExecutionResult` 中过滤**：`> 0.0` 才写入，避免 0 值被误识别为有 cost。

## 剩余差距

- Node 错误正则覆盖更多自然语言变体（如 `cannot find session`、`session has expired` 等），Rust 当前覆盖生产核心文案（unknown session / session not found / session X not found / no session）。
- Pi execute.ts 中 resume、clearSession、ACP fallback 等行为未复刻（计划 R417+）。
- 真实 CLI smoke 验证需要外部 `pi` CLI 环境。

## 文件清单

- 新增 `crates/pc-adapter-pi-local/src/pi_stream_json.rs`（约 410 行，含 14 单测）。
- 修改 `crates/pc-adapter-pi-local/src/lib.rs`（注册新模块、execute 接线、新 export）。
- 新增 `crates/pc-adapter-pi-local/tests/round416_pi_stream_parser.rs`（28 集成测试）。
