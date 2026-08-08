# R421：cursor-local execute 助手函数复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/cursor-local/src/server/execute.ts`
  - `resolveCursorBillingType`（L70-74）：CURSOR_API_KEY / OPENAI_API_KEY → Api。
  - `resolveCursorBiller`（L76-86）：OpenRouter / cursor / provider。
  - `resolveProviderFromModel`（L87-95）：model 字符串启发式识别 provider。
  - `normalizeMode`（L97-101）："plan" / "ask" 模式校验。
  - `cursorSkillsHome`（L117-119）：`<homedir>/.cursor/skills`。
  - `renderPaperclipEnvNote` / `renderApiAccessNote`（L103-115）：**已在 pc-acpx 复用**。

Rust 原实现：

- `billing_type` 硬编码 `"subscription"`，未根据 env 动态判定。
- `biller` 字段从未写入 `result_json`。
- model provider 识别能力缺失（影响后续 billing 归因）。
- mode 校验缺失。

## 实现

### 1. `pc_adapter_cursor_local::execute_helpers`（新增）

- `CursorBillingType` 枚举：`Api` / `Subscription`（带 `as_str()`）。
- `resolve_cursor_billing_type(env)`：双 key 兼容。
- `resolve_cursor_biller(env, billing_type, provider)`：OpenRouter 优先 / subscription → cursor / 否则 provider 或 cursor fallback。
- `CursorMode` 枚举：`Plan` / `Ask`（带 `as_str()`）。
- `normalize_mode(raw_mode)`：trim + lowercase 校验。
- `resolve_provider_from_model(model)`：`provider/model` 拆分 + sonnet/claude/gpt/o 启发式。
- `cursor_skills_home(homedir)`：接受 homedir 参数便于测试。

### 2. 复用 pc-acpx 通用工具

- `render_paperclip_env_note` / `render_api_access_note` 已在 `pc_acpx::session_config_options`（R408+），cursor-local 直接调用，不重复实现。

### 3. `CursorLocalAdapter::execute` 接线

- `billing_type` 改为 `resolve_cursor_billing_type(&context.env).as_str()`。
- `result_json` 中新增 `"biller"` 字段，调用 `resolve_cursor_biller(&context.env, billing_type, resolve_provider_from_model(...))`。

### 4. 依赖注入

- `Cargo.toml` 新增 `pc-acpx` 依赖（之前 cursor-local 没有 pc-acpx 依赖）。

## 验证

- `cargo test -p pc-adapter-cursor-local`：全量 71 passed（32 lib + 1 round + 6 adapter_real + **32 round421**）。
- `cargo test -p pc-adapter-cursor-local --lib execute_helpers`：24 passed（涵盖 billing/biller/mode/provider 启发式/skills_home）。
- `cargo test -p pc-adapter-cursor-local --test round421_cursor_execute_helpers`：32 passed（涵盖综合企业/个人/开发场景、provider 隐式识别、mode 合法性）。

## 关键设计决策

- **`resolve_provider_from_model` 大小写不敏感**：Node 行为 `model.trim().toLowerCase()` 后再匹配，本实现统一转 lowercase。
- **`resolve_provider_from_model` 短路顺序**：先按 `provider/model` 拆分（前缀优先），再启发式匹配。
- **`resolve_cursor_biller` 中 provider 用 `deref()` 复用**：避免 `provider.map(|s| s.as_str())` 双层包装。
- **`cursor_skills_home` 用 `homedir` 参数**：与 `gemini_skills_home` 一致，便于测试注入，生产环境传 `std::env::var("HOME")`。
- **Cargo.toml 添加 `pc-acpx` 依赖**：之前 cursor-local 没有这个依赖；R421 是 cursor-local 第一次消费 pc-acpx 工具。

## 兼容性

- `pc_adapter_cursor_local` 新增导出，老 fixture 不依赖。
- `billing_type` 现在根据 env 动态计算（无 key → `"subscription"`，有 key → `"api"`），与原硬编码行为一致（仅在有 CURSOR_API_KEY/OPENAI_API_KEY 时不同）。
- `result_json` 新增字段，老 consumer 忽略。

## 剩余差距

- cursor execute.ts 还有 `renderPaperclipEnvNote`（已迁移到 pc-acpx）、`buildCursorSkillsDir`（fs I/O）、`ensureCursorSkillsInjected`、retry loop 未实现。
- `inferOpenAiCompatibleBiller` 已 port，`buildCursorBiller` 完整覆盖。
- opencode-local / grok-local 下一轮处理。

## 文件清单

- 新增 `crates/pc-adapter-cursor-local/src/execute_helpers.rs`（约 350 行 + 24 单测）。
- 修改 `crates/pc-adapter-cursor-local/src/lib.rs`（注册新模块、execute 接线 billing_type / biller / provider）。
- 修改 `crates/pc-adapter-cursor-local/Cargo.toml`（新增 `pc-acpx` 依赖）。
- 新增 `crates/pc-adapter-cursor-local/tests/round421_cursor_execute_helpers.rs`（32 集成测试）。
