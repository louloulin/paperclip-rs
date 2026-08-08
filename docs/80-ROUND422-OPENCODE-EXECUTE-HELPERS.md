# R422：opencode-local execute 助手函数 + pc-acpx model_id 抽取

## 差距依据

Node 参考：

- `paperclip/packages/adapters/opencode-local/src/server/execute.ts`
  - `parseModelProvider`（L70-75）：与 pi-local 同款。
  - `resolveOpenCodeBiller`（L77-79）：OpenAI-compatible / provider / unknown。
  - `claudeSkillsHome`（L155-157）：opencode 使用 `~/.claude/skills`（与 Claude 本地共享）。

Rust 原实现：

- `parseModelProvider` 重复实现风险（pi-local 已有 `model_provider`）。
- `biller` 字段从未写入 `result_json`。
- `billing_type` 默认为 `None`。

## 实现

### 1. `pc_acpx::model_id`（新增通用模块）

- `parse_model_provider(model: Option<&str>) -> Option<String>`：复用 pi-local / opencode-local 共享逻辑。
- `parse_model_id(model: Option<&str>) -> Option<String>`：model id 后缀解析。
- 10 个单测覆盖标准拆分 / 大小写 / 无斜杠 / 空前缀 / 空输入 / 多斜杠。

后续 pi-local 也会改用 `pc_acpx::model_id`（R423+ 收尾）。

### 2. `pc_adapter_opencode_local::execute_helpers`（新增）

- `resolve_opencode_biller(env, provider)`：OpenRouter / provider / unknown 三层 fallback。
- `claude_skills_home(homedir)`：接受 homedir 参数便于测试。
- 9 个单测覆盖 biller 各种场景、skills_home 路径、pc_acpx 复用验证。

### 3. `OpencodeLocalAdapter::execute` 接线

- `result.provider`：用 `pc_acpx::model_id::parse_model_provider` 拆分 model，得到真实 provider（如 `"anthropic"`），缺失时 fallback 到 `ADAPTER_TYPE`。
- `result.billing_type`：置 `"unknown"`（与 Node 等价）。
- `result_json` 中新增 `"biller"` 字段。

## 验证

- `cargo test -p pc-acpx --lib model_id`：10 passed（新增通用模块）。
- `cargo test -p pc-adapter-opencode-local`：全量 64 passed（30 lib + 1 round + 10 adapter_real + 5 round415 + **18 round422**）。
- `cargo test -p pc-adapter-opencode-local --lib execute_helpers`：9 passed。
- `cargo test -p pc-adapter-opencode-local --test round422_opencode_execute_helpers`：18 passed。

## 关键设计决策

- **`pc_acpx::model_id` 独立模块**：与 `paths` / `env_helpers` / `billing` 等并列，专注 model 解析，避免 `paths` 模块膨胀。
- **opencode-local `claude_skills_home` 而非 `opencode_skills_home`**：保留 Node 原命名（即使奇怪），因为 OpenCode 本地 CLI 确实把 skill 注入到 `~/.claude/skills`（与 Claude 本地共享）。
- **`resolve_opencode_biller` 与 pi-local 同款**：`infer_openai_compatible_biller(env, None) ?? provider ?? "unknown"`。后续可考虑也抽取到 pc-acpx。

## 兼容性

- `pc_adapter_opencode_local` 新增导出，老 fixture 不依赖。
- `result.provider` 现在根据 model 拆分（之前固定 `ADAPTER_TYPE`），更精确。
- `result.billing_type` 默认 `"unknown"`（之前 `None`），与 Node parity。
- `result_json` 新增字段，老 consumer 忽略。

## 剩余差距

- opencode execute.ts 还有很多：完整 retry loop、login flow、runtime config 准备。
- `pc_acpx::model_id` 与 pi-local 重复实现：R423+ 收尾时把 pi-local 改为 re-export。
- grok-local 下一轮处理。

## 文件清单

- 新增 `crates/pc-acpx/src/model_id.rs`（约 110 行 + 10 单测）。
- 修改 `crates/pc-acpx/src/lib.rs`（注册 `model_id` 模块）。
- 新增 `crates/pc-adapter-opencode-local/src/execute_helpers.rs`（约 130 行 + 9 单测）。
- 修改 `crates/pc-adapter-opencode-local/src/lib.rs`（注册新模块、execute 接线 provider / billing_type / biller）。
- 新增 `crates/pc-adapter-opencode-local/tests/round422_opencode_execute_helpers.rs`（18 集成测试）。
