# R420：gemini-local execute 助手函数复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/gemini-local/src/server/execute.ts`
  - `resolveGeminiBillingType`（L75-79）：`GEMINI_API_KEY` 或 `GOOGLE_API_KEY` 任一非空 → `Api`。
  - `buildGeminiHeadlessEnv`（L81-93）：TERM / COLORTERM / NO_BROWSER / NO_COLOR 规范化。
  - `geminiSkillsHome`（L131-133）：`<homedir>/.gemini/skills`。
  - `renderPaperclipEnvNote`（L103-115）：列出所有 `PAPERCLIP_*` 变量名（不含值）。
  - `renderApiAccessNote`（L117-129）：curl 用法示例。

Rust 原实现：

- `billing_type` 从未设置（依赖默认值 `None`）。
- `paperclipEnvNote` / `apiAccessNote` 从未注入到 `result_json`。
- headless env 规范化、skills home 路径拼接未抽 helper。

## 实现

### 1. `pc_adapter_gemini_local::execute_helpers`（新增）

- `GeminiBillingType` 枚举：`Api` / `Subscription`（带 `as_str()`）。
- `resolve_gemini_billing_type(env)`：双 key 兼容。
- `build_gemini_headless_env(env)`：TERM / COLORTERM / NO_BROWSER / NO_COLOR 规范化。
- `gemini_skills_home(homedir)`：接受 homedir 参数便于测试（生产环境传 `std::env::var("HOME")`）。
- `render_paperclip_env_note(env)`：仅列表变量名（避免敏感值泄露）。
- `render_api_access_note(env)`：当 PAPERCLIP_API_URL + KEY 都有时返回 curl 示例。

### 2. `GeminiLocalAdapter::execute` 接线

- `billing_type` 改为 `resolve_gemini_billing_type(&context.env).as_str()`。
- `result_json` 的 object map 中新增 `paperclipEnvNote` 和 `apiAccessNote` 字段（覆盖原 `parsed.result_json` 中的同名字段）。

## 验证

- `cargo test -p pc-adapter-gemini-local`：全量 96 passed（48 lib + 1 round + 10 adapter_real + 7 round413 + **30 round420**）。
- `cargo test -p pc-adapter-gemini-local --lib execute_helpers`：27 passed（涵盖 billing 双 key、headless 各 TERM/COLORTERM 边界、skills_home 路径拼接、env_note 排序、api_access_note 空白值）。
- `cargo test -p pc-adapter-gemini-local --test round420_gemini_execute_helpers`：30 passed（涵盖综合企业/个人/订阅场景、headless 完整 / 保留自定义、不修改变量、prompt 注入两段）。

## 关键设计决策

- **`render_paperclip_env_note` 仅列变量名**：避免敏感信息（API key、auth token）泄露到 prompt 头部。
- **`gemini_skills_home` 接受 homedir 参数**：避免依赖 `std::env::var("HOME")`，保持纯函数可测。
- **`build_gemini_headless_env` 用 `BTreeMap`**：与现有 pc-acpx env 工具保持一致（有序便于日志）。
- **`result_json` 字段合并策略**：从 `parse_gemini_stream_json` 拿到 result_json 后，向 object map 中插入 `paperclipEnvNote` / `apiAccessNote`；如果原 result_json 不是 object 则 fallback 到 `{}`，避免 panic。
- **`render_api_access_note` 短路径早返**：两个 env 变量任一缺失立即返回空字符串，避免不必要字符串拼接。

## 兼容性

- `pc_adapter_gemini_local` 新增导出，老 fixture 不依赖。
- `billing_type` 现在根据 env 动态计算（无 key → `"subscription"`，有 key → `"api"`），与原 `None` 默认不同但更精确。
- `result_json` 新增 2 个字段，老 consumer 忽略。

## 剩余差距

- gemini execute.ts 还有 `buildGeminiRuntimeEnv`（合并 process.env + headless + ensurePathInEnv）、`ensureGeminiSkillsInjected`、retry loop 未实现。
- `GEMINI_CLI_HOME` 转绝对路径的逻辑没在 `buildGemini_headless_env` 中实现（Node 那部分依赖 fs I/O）。
- 其他 3 个 adapter（cursor / opencode / grok）的 execute helpers 尚未抽取。

## 文件清单

- 新增 `crates/pc-adapter-gemini-local/src/execute_helpers.rs`（约 340 行 + 27 单测）。
- 修改 `crates/pc-adapter-gemini-local/src/lib.rs`（注册新模块、execute 接线 billing_type 与 result_json 合并）。
- 新增 `crates/pc-adapter-gemini-local/tests/round420_gemini_execute_helpers.rs`（30 集成测试）。
