# R417：pi-local execute 助手函数复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/pi-local/src/server/execute.ts`（L60-215 行核心 helper 部分）
  - `firstNonEmptyLine`（L60-67）
  - `parseModelProvider`（L69-74）
  - `parseModelId`（L76-81）
  - `resolvePiBiller`（L135-137）
  - `buildSessionPath` / `buildRemoteSessionPath`（L144-152）
  - `normalizeExecutionCwd`（L154-156）
  - `executionCwdsMatch`（L158-160）
  - `readSessionHeaderCwd`（L162-176）
  - `readSavedSessionCwd`（L178-214）
  - `isPiUnknownSessionError`（已 R416）

R416 后 `pi_stream_json.rs` 已具备 unknown session 检测能力，但 `execute_helpers` 全部缺失，导致：

- 模型 provider 拆分（用于 billing 归因）走默认 fallback。
- Session header 解析缺失，无法判断能否 resume。
- cwd 规范化逻辑缺失，resume 比对走字符串比对。
- Biller 解析（OpenRouter 等）缺失。
- `clear_session` 标志从未被触发。

## 实现

新增 `crates/pc-adapter-pi-local/src/execute_helpers.rs`（约 220 行 + 24 单测）：

- `model_provider(model: Option<&str>) -> Option<String>`：拆分 "provider/model" 的 provider 前缀。
- `model_id(model: Option<&str>) -> Option<String>`：拆分 model id 后缀；无 `/` 时整串返回。
- `resolve_pi_biller(env, provider)`：调用 `pc_acpx::billing::infer_openai_compatible_biller`，无 OpenAI hint 时回退到 provider，最后 "unknown"。
- `parse_session_header_cwd(raw)`：从 session JSONL 第一行（`{type:"session",cwd:"..."}`）提取 cwd。
- `normalize_cwd(candidate)`：清理 `./` / `../`，保留绝对前缀（避免 join 产生双斜杠）。
- `cwds_match(saved, current)`：规范化后字符串比较（POSIX 大小写敏感，与 Node 一致）。
- `should_clear_session(stdout, stderr)`：转调 `is_pi_unknown_session_error`。
- `should_resume(saved_cwd, current_cwd)`：saved_cwd 非空且与 current 匹配。

`PiLocalAdapter::execute` 已集成：

- `result.provider`：用 `model_provider()` 拆分得到的 provider，缺失时 fallback 到 `ADAPTER_TYPE`。
- `result.billing_type = Some("unknown")`：与 Node 一致（Node `toResult` 中写死 `"unknown"`）。
- `result.result_json.biller`：通过 `resolve_pi_biller` 计算并写入（用于上层追溯）。
- `result.clear_session`：通过 `should_clear_session(stdout, stderr)` 触发，配合后续 retry without `--resume`。

## 验证

- `cargo test -p pc-adapter-pi-local`：全量 130 passed（58 lib + 1 round395 + 10 adapter_real + 28 round416 + 33 round417）。
- `cargo test -p pc-adapter-pi-local --lib execute_helpers`：24 passed（涵盖 model_provider / model_id / resolve_pi_biller / parse_session_header_cwd / normalize_cwd / cwds_match / should_clear_session / should_resume 各场景）。
- `cargo test -p pc-adapter-pi-local --test round417_pi_execute_helpers`：33 passed（涵盖标准拆分、带空格、无斜杠、空前缀、空后缀、空输入、多斜杠、biller 三种 fallback、session header 多种 type/格式/cwd/cwd-empty/损坏 JSON、前导空行、cwd 绝对/相对/根/空输入、大小写敏感、resume 三种决策、综合 resume 流程）。
- `cargo check --workspace --tests`：workspace 编译验证通过。

## 关键设计决策

- **单 `pub mod` 而非多文件**：所有 helper 都在 `execute_helpers.rs` 内，按主题分组；避免 6 个 50 行小文件造成的目录噪音。
- **`normalize_cwd` 不调用 `dunce::canonicalize`**：Node `path.resolve` 在 pi-local 路径下不会做文件系统解析（仅字符串处理），Rust 保持相同语义可避免 fs I/O。
- **`normalize_cwd` 双斜杠陷阱**：直接 `out.join("/")` 在 RootDir 前缀处会产生 `//a/b`。修复方案是 RootDir 只置 `absolute = true` 不入栈，最后用 `format!("/{}", ...)` 拼回。
- **`infer_openai_compatible_biller` 通过 `pc_acpx::billing` 调用**：避免重复实现，遵循 R396 起建立的"helper 在 pc-acpx，adapter 调用"分层。
- **`should_clear_session` 显式重导出**：保留 `is_pi_unknown_session_error` 作为低层语义函数，新增 `should_clear_session` 作为高层决策函数，便于将来添加 `clearSessionOnMissingSession` 之类的开关。
- **`parse_session_header_cwd` 容忍 JSONL 头空行**：trim 后用 `find(|line| !line.is_empty())` 取第一个非空行，与 Node 行为一致。

## 兼容性

- 未破坏现有 fixture：所有 R395/R416 测试均通过。
- `result.provider` 仍兜底 `ADAPTER_TYPE`，老配置不会失败。
- `result.clear_session` 默认 `false`，仅在 `should_clear_session(stdout, stderr)` 命中时置 true（与 Node 等价的最小触发面）。

## 剩余差距

- Node execute.ts 还有大量未复刻：skills 注入（已在 R395 skills.rs 中部分）、env 注入（`buildPaperclipEnv` 已 port）、`buildSessionPath` / `buildRemoteSessionPath` 路径构造（需 fs）、retry loop（需要 `--resume` → 失败 → 不带 `--resume` 重试）、ACP fallback（需 runtime adapter）。
- resume 决策的"读 session 文件并比对 cwd"流程已具备 helper，但完整 retry wiring 需 fs I/O，留给后续轮次。
- biller fallback 仅覆盖 `infer_openai_compatible_biller` 已支持的 OpenRouter / OpenAI-compatible 系列；其他 provider hint（如 Anthropic 直接）未扩展（与 Node 等价）。

## 文件清单

- 新增 `crates/pc-adapter-pi-local/src/execute_helpers.rs`（约 220 行，含 24 单测）。
- 修改 `crates/pc-adapter-pi-local/src/lib.rs`（注册新模块、execute 接线 provider/billing_type/biller/clear_session、新 export）。
- 新增 `crates/pc-adapter-pi-local/tests/round417_pi_execute_helpers.rs`（33 集成测试）。
