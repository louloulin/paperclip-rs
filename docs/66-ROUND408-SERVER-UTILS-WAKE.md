# R408：server-utils wake payload 核心复刻

## 目标

继续将 Node `paperclip/packages/adapter-utils/src/server-utils.ts` 的唤醒载荷核心同步逻辑移植到 Rust，保持模块独立、纯函数可测，并不改变现有 `prompt_compose` 的调用路径。

## 实现

- 新增 `pc-acpx::server_utils_wake` 模块并在 `lib.rs` 注册。
- 复刻两个默认提示模板：`DEFAULT_PAPERCLIP_AGENT_PROMPT_TEMPLATE`、`WATCHDOG_DEFAULT_MANDATE`。
- 建模 `PaperclipWakeIssue`、`PaperclipWakeRecovery`、`PaperclipWakeAgentMessage`、`PaperclipWakePayload`，使用 `camelCase` JSON wire format。
- 实现恢复、问题、agent message、完整 wake payload 的归一化；控制字符过滤与 Node 一致。
- 实现 wake payload JSON 字符串化、可选省略问题描述、恢复唤醒判断、工作模式读取、assignment-shaped reason 判断及任务 Markdown 选择。
- 对尚未需要 Rust 领域建模的复杂 wake 子结构采用 `serde_json::Value` 透传，避免模块间耦合并保留后续扩展空间。

## 验证

- `cargo build -p pc-acpx`：通过。
- `cargo test -p pc-acpx --lib server_utils_wake`：23 passed。
- `cargo test -p pc-acpx --test round408_server_utils_wake`：10 passed。
- 集成测试覆盖模板、嵌套工作模式、控制字符、恢复原因、摘要选择和描述脱敏。

## 已知边界

完整 `renderPaperclipWakePrompt` 已在 `prompt_compose` 中存在并继续作为现有 prompt 入口；本轮专注于 server-utils 的 wake 数据边界和路由纯函数。复杂子结构的强类型建模、各 adapter 的 parse/execute 运行时接线在后续轮次处理。
