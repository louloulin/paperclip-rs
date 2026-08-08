# R419：codex-local execute 助手函数复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/codex-local/src/server/execute.ts`
  - `hasNonEmptyEnvValue`（R418 已 port 到 `pc_acpx::env_helpers`）
  - `resolveCodexBillingType`（L159-162）：OPENAI_API_KEY 是否存在。
  - `resolveCodexBiller`（L164-168）：openrouter 优先，subscription → chatgpt，api → openai-compatible 或 openai。
  - `resolveCodexSkillsDir`（L237-239）：`<codex_home>/skills`。
  - `readCodexTransientFallbackMode`（L254-265）：从 context 解析 4 种合法值。
  - `fallbackModeUsesSaferInvocation` / `fallbackModeUsesFreshSession`（L267-273）。

Rust 原实现：

- `billing_type` 硬编码 `"subscription"`（R412），未根据 env 动态判定。
- `biller` 字段从未写入 `result_json`。
- `transient_fallback_mode` 解析能力缺失，无法支持 retry 策略。
- `skills_dir` 路径拼接未抽 helper。

## 实现

### 1. `pc_adapter_codex_local::execute_helpers`（新增）

- `CodexBillingType` 枚举：`Api` / `Subscription`（带 `as_str()`）。
- `resolve_codex_billing_type(env)`：根据 `OPENAI_API_KEY` 返回 billing 类型。
- `resolve_codex_biller(env, billing_type)`：OpenRouter 优先，否则按 billing type 走 chatgpt / openai / openai-compatible。
- `CodexTransientFallbackMode` 枚举：`SameSession` / `SaferInvocation` / `FreshSession` / `FreshSessionSaferInvocation`（带 `as_str()`）。
- `read_codex_transient_fallback_mode(context)`：从 JSON context 解析合法值。
- `fallback_mode_uses_safer_invocation(mode)`：`SaferInvocation` 或 `FreshSessionSaferInvocation`。
- `fallback_mode_uses_fresh_session(mode)`：`FreshSession` 或 `FreshSessionSaferInvocation`。
- `resolve_codex_skills_dir(codex_home)`：字符串拼接，处理 `/` 边界与尾随 `/`。

### 2. `CodexLocalAdapter::execute` 接线

- `billing_type` 改为 `resolve_codex_billing_type(&context.env).as_str()`。
- `result_json` 中新增 `"biller"` 字段，写入 `resolve_codex_biller(&context.env, billing_type)`。

## 验证

- `cargo test -p pc-adapter-codex-local`：全量 67 passed（26 lib + 1 round + 7 adapter_real + 8 round412 + **25 round419**）。
- `cargo test -p pc-adapter-codex-local --lib execute_helpers`：14 passed（涵盖 billing/biller 各种组合、fallback mode 4 种合法值 + 非法值 + 字符串 trim + 字段缺失、safer_invocation/fresh_session 决策、基本/根/空/相对 skills_dir）。
- `cargo test -p pc-adapter-codex-local --test round419_codex_execute_helpers`：25 passed（涵盖综合企业/个人/开发场景、as_str 映射、context 解析与策略判断）。

## 关键设计决策

- **`CodexBillingType` 枚举 vs 字符串字面量**：编译期防止 typo（如 `subscripton`），与 R418 `ClaudeBillingType` 风格一致。
- **`resolve_codex_biller` 复用 `pc_acpx::billing::infer_openai_compatible_biller`**：openrouter 检测逻辑统一在 pc-acpx，codex-local 仅做 billing_type 分支。
- **`resolve_codex_skills_dir` 处理 `"/"` 边界**：Node `path.join("/", "skills")` 返回 `"/skills"`，但 Rust 简单 `trim_end_matches('/')` 会把 `"/"` 变成 `""`。特判保留 `/` 前缀。
- **`read_codex_transient_fallback_mode` 接受 `&serde_json::Value`**：context 是任意 JSON 形状，适配 `serde_json::Value` 比解析结构体更省事。
- **`fallback_mode_uses_safer_invocation` 接受 `Option<...>`**：None 表示未启用，比 `bool` 更明确。

## 兼容性

- `pc_adapter_codex_local` 新增导出，老 fixture 不依赖。
- `billing_type` 默认仍为 `"subscription"`（env 中无 `OPENAI_API_KEY` 时），与原行为一致。
- `result_json` 新增 `"biller"` 字段，老 consumer 忽略。

## 剩余差距

- codex execute.ts 还有很多未复刻：`buildLoginResult` / `ensureCodexSkillsInjected` / `assertCodexCredentialsLaunchable` / `buildCodexTransientHandoffNote` / `managedMcpGatewaysFromContext`。
- 完整 retry loop 暂未实现（依赖 transient fallback mode 决策，目前仅暴露 helpers）。
- 其他 4 个 adapter（gemini / cursor / opencode / grok）的 execute helpers 尚未抽取。

## 文件清单

- 新增 `crates/pc-adapter-codex-local/src/execute_helpers.rs`（约 320 行 + 14 单测）。
- 修改 `crates/pc-adapter-codex-local/src/lib.rs`（注册新模块、execute 接线 billing_type 与 biller）。
- 新增 `crates/pc-adapter-codex-local/tests/round419_codex_execute_helpers.rs`（25 集成测试）。
