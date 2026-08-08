# R418：claude-local execute 助手函数 + pc-acpx 通用工具抽取

## 差距依据

Node 参考：

- `paperclip/packages/adapters/claude-local/src/server/execute.ts`
  - `claudeSessionCwdMatchesExecutionTarget`（L120-127）：session cwd 与当前 cwd 比对。
  - `hasNonEmptyEnvValue`（L143-146）：env 键的非空判断（多 adapter 通用）。
  - `isBedrockAuth`（L148-154）：AWS Bedrock 认证检测。
  - `resolveClaudeBillingType`（L156-159）：billing 类型分类（api / subscription / metered_api）。

Rust 原实现：

- `billing_type` 硬编码为 `"subscription"`（R411 设置），未根据 env 动态判定。
- session cwd 比对走默认 `true`（无 helper），无法在 Node `claudeSessionCwdMatchesExecutionTarget` 等价逻辑下做 resume 决策。
- `hasNonEmptyEnvValue` 是通用工具但散在 claude-local 内部，没有 pc-acpx 通用化。
- `normalize_cwd` / `cwds_match` 在 pi-local 已经实现（R417），但未抽取到 pc-acpx 导致跨 adapter 复用需复制粘贴。

## 实现

### 1. `pc_acpx::env_helpers::has_non_empty_env_value`（新增通用工具）

- 复用现有 `env_helpers` 模块（与 `ensure_path_in_env` / `resolve_runtime_env` 同位置）。
- 2 个单测覆盖命中 / 空值 / 缺失场景。

### 2. `pc_acpx::paths::normalize_cwd` + `cwds_match`（从 pi-local 抽取）

- 把 R417 写在 `pi-local::execute_helpers` 的两个通用工具提升到 `pc_acpx::paths`。
- 6 个单测覆盖绝对/相对路径、根路径、空输入、大小写敏感。
- `pi-local::execute_helpers` 改为 `pub use pc_acpx::paths::{cwds_match, normalize_cwd}`，
  保留原 `pub` 导出（向后兼容）。
- 删除了 pi-local 中的重复单测（3 个 normalize_cwd / cwds_match 单测移至 pc-acpx）。

### 3. `pc_adapter_claude_local::execute_helpers`（新增）

- `ClaudeBillingType` 枚举：`Api` / `Subscription` / `MeteredApi`（带 `as_str()` 映射到 Node wire-format）。
- `is_bedrock_auth(env)`：检查 `CLAUDE_CODE_USE_BEDROCK ∈ {"1", "true"}` 或 `ANTHROPIC_BEDROCK_BASE_URL` 非空。
- `resolve_claude_billing_type(env)`：Bedrock → MeteredApi；否则有 API key → Api；否则 Subscription。
- `claude_session_cwd_matches_execution_target(runtime_session_cwd, effective_execution_cwd, is_remote)`：
  - remote 或 runtime_cwd 空 → `true`（宽松通过）。
  - 否则调用 `pc_acpx::paths::cwds_match` 比较。
- 15 个单测覆盖 Bedrock 各 env 组合、Bedrock 优先于 API key、空值/缺值/空白、各种 cwd 比对场景。

### 4. `ClaudeLocalAdapter::execute` 接线

- `result.billing_type` 改为调用 `resolve_claude_billing_type(&context.env).as_str()` 动态计算（替换原硬编码 `"subscription"`）。
- `execute_helpers` 模块在 `lib.rs` 注册并 re-export。

## 验证

- `cargo test -p pc-acpx --lib env_helpers paths`：8 passed（2 has_non_empty + 6 path_utils）。
- `cargo test -p pc-acpx --lib`：全量通过（无回归）。
- `cargo test -p pc-adapter-claude-local`：全量 93 passed（41 lib + 2 round391 + 11 adapter_real + 6 round410 + 7 round411 + **26 round418**）。
- `cargo test -p pc-adapter-pi-local`：全量 127 passed（55 lib + 1 round395 + 10 adapter_real + 28 round416 + 33 round417，normalize_cwd/cwds_match 单元测试已迁移到 pc-acpx 后减少 3 个）。
- `cargo test -p pc-adapter-pi-local --test round417_pi_execute_helpers`：33 passed（继续通过，验证 re-export）。
- `cargo test -p pc-adapter-claude-local --test round418_claude_execute_helpers`：26 passed（涵盖 Bedrock 各 env 组合、Billing 三种类型、session cwd remote/空/一致/不一致/大小写、综合企业/个人/订阅场景）。

## 关键设计决策

- **`normalize_cwd` 提升到 `pc_acpx::paths`**：避免 R417 后 pi-local 与 claude-local 各持一份实现，符合"高内聚低耦合"。`pi-local::execute_helpers` 通过 `pub use` 保留 re-export，老调用点不破坏。
- **`has_non_empty_env_value` 提升到 `pc_acpx::env_helpers`**：env 工具本来就属于 pc-acpx 的 helper 层（与 `ensure_path_in_env` / `resolve_runtime_env` 同模块），而非 adapter-specific。
- **`ClaudeBillingType` 枚举 vs 字符串字面量**：枚举 + `as_str()` 比 `&'static str` 常量更类型安全，编译期防止 typo（如 `mterered_api`）。
- **`is_bedrock_auth` 委托到 `has_non_empty_env_value`**：保持 Node 等价的 `hasNonEmptyEnvValue(env, "ANTHROPIC_BEDROCK_BASE_URL")` 行为，但触发条件单独判断 `CLAUDE_CODE_USE_BEDROCK` 的精确值（不能用 `has_non_empty`，因为 `"0"` 非空但不算触发）。
- **`claude_session_cwd_matches_execution_target` 复用 `pc_acpx::paths::cwds_match`**：避免再实现一遍 cwd 规范化，统一两个 adapter 的 cwd 比较语义。
- **保留 `pi-local::normalize_cwd` / `cwds_match` 作为 re-export**：用户已经依赖的 API 路径不破坏，新代码可以直接走 `pc_acpx::paths`。

## 兼容性

- **pi-local**：
  - 老的 `use pc_adapter_pi_local::{normalize_cwd, cwds_match}` 仍然可用（来自 `pub use pc_acpx::paths::{cwds_match, normalize_cwd}`）。
  - 删除了 3 个 normalize_cwd/cwds_match 单测（迁移到 pc-acpx），其他测试无回归。
- **claude-local**：
  - `pc_adapter_claude_local::{is_bedrock_auth, resolve_claude_billing_type, claude_session_cwd_matches_execution_target, ClaudeBillingType}` 为新导出，老 fixture 不依赖。
  - `result.billing_type` 现在动态计算，配置 Bedrock / API key 时分别得到 `"metered_api"` / `"api"`；未配置时仍为 `"subscription"`（与原行为一致）。

## 剩余差距

- claude execute.ts 还有：完整 env 注入（`buildPaperclipEnv` 已 port）、`buildClaudeRuntimeConfig`（workspaceCwd / workspaceStrategy / workspaceBranch 等）、retry loop（需要 `--resume` → 失败 → 重试）、login flow。
- 5 个其他 adapter 的同类 helpers（codex / gemini / cursor / opencode / grok）尚未抽取。
- `has_non_empty_env_value` 当前只在 claude-local 使用；codex-local 等大概率也类似，可以后续统一消费。

## 文件清单

- 新增 `crates/pc-adapter-claude-local/src/execute_helpers.rs`（约 200 行 + 15 单测）。
- 修改 `crates/pc-adapter-claude-local/src/lib.rs`（注册新模块、execute 接线 billing_type、新 export）。
- 修改 `crates/pc-acpx/src/paths.rs`（新增 `normalize_cwd` + `cwds_match` + 6 单测）。
- 修改 `crates/pc-acpx/src/env_helpers.rs`（新增 `has_non_empty_env_value` + 2 单测）。
- 修改 `crates/pc-adapter-pi-local/src/execute_helpers.rs`（委托到 `pc_acpx::paths`、移除重复定义与单测）。
- 新增 `crates/pc-adapter-claude-local/tests/round418_claude_execute_helpers.rs`（26 集成测试）。
