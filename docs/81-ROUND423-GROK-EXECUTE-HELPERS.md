# R423：grok-local execute 助手函数复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/grok-local/src/server/execute.ts`
  - `resolveBillingType`（L188-190）：`XAI_API_KEY` 非空 → `Api`。
  - `renderPaperclipEnvNote` / `renderApiAccessNote`（L61-91）：**已在 pc-acpx 复用**。

Rust 原实现：

- `billing_type` 从未设置（依赖默认值 `None`）。

## 实现

### 1. `pc_adapter_grok_local::execute_helpers`（新增）

- `GrokBillingType` 枚举：`Api` / `Subscription`（带 `as_str()`）。
- `resolve_grok_billing_type(env)`：`XAI_API_KEY` 非空 → `Api`。
- 4 个单测覆盖默认 / Api / 空白 / as_str。

### 2. 复用 pc-acpx 通用工具

- `render_paperclip_env_note` / `render_api_access_note` 已在 `pc_acpx::session_config_options`（R408+），grok-local 直接调用。

### 3. `GrokLocalAdapter::execute` 接线

- `billing_type` 改为 `resolve_grok_billing_type(&context.env).as_str()`。

### 4. 依赖注入

- `Cargo.toml` 新增 `pc-acpx` 依赖。

## 验证

- `cargo test -p pc-adapter-grok-local`：全量 33 passed（18 lib + 1 round + 7 adapter_real + **7 round423**）。
- `cargo test -p pc-adapter-grok-local --lib execute_helpers`：4 passed。
- `cargo test -p pc-adapter-grok-local --test round423_grok_execute_helpers`：7 passed。

## 关键设计决策

- **GrokLocalAdapter 复用现有 `render_*_note`**：之前 grok-local 自己实现了 `render_paperclip_env_note` 但未挂到 `result_json`；R423 暂不改动这部分，留给后续 round 收尾所有 adapter 的 prompt 注入。
- **grok-local 模块最小**：Node 端也相对简单（只有 1 个高 ROI helper），与 R419-R422 模板保持一致但工作量轻。

## 兼容性

- `pc_adapter_grok_local` 新增导出，老 fixture 不依赖。
- `billing_type` 现在根据 env 动态计算（无 key → `"subscription"`，有 key → `"api"`），与原 `None` 默认不同但更精确。

## 剩余差距

- grok-local execute.ts 完整 runtime config / retry loop 未实现。
- 7 个 local adapter 的 `render_paperclip_env_note` / `render_api_access_note` 现在统一消费 pc-acpx，但 gemini-local R420 是自己实现（与 pc-acpx 略不同——双 `\n\n` 结尾），后续可以收尾统一。
- pi-local `model_provider` / `model_id` 与 `pc_acpx::model_id` 重复实现：R424 收尾时把 pi-local 改为 re-export。

## 文件清单

- 新增 `crates/pc-adapter-grok-local/src/execute_helpers.rs`（约 80 行 + 4 单测）。
- 修改 `crates/pc-adapter-grok-local/src/lib.rs`（注册新模块、execute 接线 billing_type）。
- 修改 `crates/pc-adapter-grok-local/Cargo.toml`（新增 `pc-acpx` 依赖）。
- 新增 `crates/pc-adapter-grok-local/tests/round423_grok_execute_helpers.rs`（7 集成测试）。
