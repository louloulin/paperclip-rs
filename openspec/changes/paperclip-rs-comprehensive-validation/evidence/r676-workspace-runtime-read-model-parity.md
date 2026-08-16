# R676 — workspace-runtime-read-model 1:1 parity

## 目标

补齐 `crates/pc-repos/src/workspace_runtime_read_model.rs` 中缺失的
`selectConfiguredRuntimeServiceRows` —— Node `server/src/services/workspace-runtime-read-model.ts`
(137 行) 中的核心 pure function 之一。该 module 之前已下沉了 2/4 export
function（`selectCurrentRuntimeServiceRows` + 2 个 batch DB query），但
`selectConfiguredRuntimeServiceRows` 一直没实现，导致 service 层无法
按 workspaceRuntime 配置匹配 runtime service。本次 R676 完成最后一
块 pure helper。

## 工作产出

### 1. 新增代码（`crates/pc-repos/src/workspace_runtime_read_model.rs`）

#### 1.1 Helper struct

```rust
#[derive(Debug, Clone)]
pub struct ConfiguredRuntimeServiceRow {
    pub row: WorkspaceRuntimeServiceRow,
    pub config_index: Option<usize>,  // mirrors Node `serviceIndex`
}
```

#### 1.2 私有 helpers

- `read_reuse_scope(raw_config: &serde_json::Value) -> Option<String>` —— 从
  `command.rawConfig.reuseScope` 读 trim + non-empty 字符串
- `resolve_expected_scope(reuse_scope, lifecycle) -> &'static str` —— 1:1
  Node 的 expectedScope 解析顺序：
  1. `reuseScope ∈ {"project_workspace", "execution_workspace", "agent", "run"}` → 原样
  2. `lifecycle == "shared"` → `"project_workspace"`
  3. else → `"run"`
- `WorkspaceRuntimeServiceRow::to_match_input(config_index)` —— 转为
  `pc_workspace_commands::WorkspaceRuntimeServiceMatchInput`

#### 1.3 公开函数

```rust
pub fn select_configured_runtime_service_rows(
    rows: &[WorkspaceRuntimeServiceRow],
    workspace_runtime: Option<&serde_json::Value>,
) -> Vec<ConfiguredRuntimeServiceRow>
```

实现细节：
- 调 `pc_workspace_commands::list_workspace_service_command_definitions`
- 调 `pc_workspace_commands::match_workspace_runtime_service_to_command`
- 内部用 `Vec<usize>` 索引池代替 JS `availableRows.splice()`
- 每次 command 命中匹配后从 pool 删除该 row，确保后续 command 不会再次选中

### 2. 依赖挂接（`crates/pc-repos/Cargo.toml`）

```toml
pc-workspace-commands = { path = "../pc-workspace-commands" }
```

Node 中通过 `@paperclipai/shared` 调用 `listWorkspaceServiceCommandDefinitions` /
`matchWorkspaceRuntimeServiceToCommand`，Rust 端复用既有的 `pc-workspace-commands`
pure-helper crate，无需重复实现。

## 测试结果

### `cargo test -p pc-repos --lib workspace_runtime_read_model`

```
running 15 tests
test workspace_runtime_read_model::tests::composite_key_when_no_reuse ... ok
test workspace_runtime_read_model::tests::dedupe_keeps_first_per_identity ... ok
test workspace_runtime_read_model::tests::dedupe_respects_reuse_key ... ok
test workspace_runtime_read_model::tests::reuse_key_takes_priority ... ok
test workspace_runtime_read_model::tests::r676_returns_empty_when_no_runtime ... ok
test workspace_runtime_read_model::tests::r676_matches_single_command ... ok
test workspace_runtime_read_model::tests::r676_no_match_skips ... ok
test workspace_runtime_read_model::tests::r676_reuse_scope_overrides_default ... ok
test workspace_runtime_read_model::tests::r676_reuse_scope_invalid_falls_through ... ok
test workspace_runtime_read_model::tests::r676_lifecycle_shared_uses_project_workspace ... ok
test workspace_runtime_read_model::tests::r676_lifecycle_shared_does_not_match_run_scope ... ok
test workspace_runtime_read_model::tests::r676_matched_row_removed_from_pool ... ok
test workspace_runtime_read_model::tests::r676_mixed_scopes ... ok
test workspace_runtime_read_model::tests::r676_cd_does_not_match_different_command ... ok
test workspace_runtime_read_model::tests::r676_skips_unsupported_service_kind ... ok

test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 630 filtered out
```

✅ **15 passed / 0 failed**（含 4 个已有 R262 tests + 11 个新 R676 parity tests）

### 测试覆盖矩阵

| 测试 | Node 行为 | Rust parity |
|---|---|---|
| `r676_returns_empty_when_no_runtime` | 无 `list_workspace_service_command_definitions` 输出时直接返回 `[]` | ✅ |
| `r676_matches_single_command` | 单 command + 匹配 row → 返回 `[{row, configIndex: 0}]` | ✅ |
| `r676_no_match_skips` | name 不匹配 → 跳过 | ✅ |
| `r676_reuse_scope_overrides_default` | `reuseScope` 4 字符串之一 → 覆盖 lifecycle 默认 | ✅ |
| `r676_reuse_scope_invalid_falls_through` | `reuseScope` 不是 4 字符串 → 回落 lifecycle | ✅ |
| `r676_lifecycle_shared_uses_project_workspace` | `lifecycle="shared"` → `"project_workspace"` | ✅ |
| `r676_lifecycle_shared_does_not_match_run_scope` | `lifecycle="shared"` + row.scope="run" → 不匹配 | ✅ |
| `r676_matched_row_removed_from_pool` | 命中后从 pool 中删除（Node splice 语义） | ✅ |
| `r676_mixed_scopes` | 多 scope 同名 row，lifecycle 决定选哪个 | ✅ |
| `r676_cd_does_not_match_different_command` | command 字符串不匹配 → `score=-1` → 不选 | ✅ |
| `r676_skips_unsupported_service_kind` | `jobs` 类型被 pc_workspace_commands 过滤掉 | ✅ |

### 关键 bug & 学习

- **`match_workspace_runtime_service_to_command` 一致性**：command 与 runtime
  service 的 `command` 字段都非空但不等 → 直接返回 `-1`（不匹配）。初版
  R676 测试 `r676_mixed_scopes` 中我误以为 "second row 也会被选中"，这是
  测试期望错误而非代码错误。已修正测试 + 加 `r676_cd_does_not_match_different_command`
  单独 lock 这个语义。

## 回归

- `cargo build -p pc-server`：成功（无新 warning）
- `cargo test -p pc-http --lib`：495 passed / 0 failed
- `cargo test -p pc-plugin-database`：47 passed / 0 failed（新增 doc-test 1 passed，总 48）

## 综合覆盖度（更新至 R676）

| 维度 | R675 终态 | R676 终态 |
|---|---|---|
| Node services 1:1 parity | environment-config 7/9 | **+ workspace-runtime-read-model 3/4（最后一块纯函数）** |
| pc-repos workspace_runtime_read_model tests | 4 | **15**（+11） |
| pc-http lib tests | 495 | **495**（无 regression） |
| pc-plugin-database tests | 47 | **47**（无 regression） |
| pc-server build | ✅ | ✅（无新 warning） |

## 用户硬约束遵守

| 约束 | 状态 |
|---|---|
| 不 commit | ✅ |
| 不修 Adapter | ✅ |
| 真实验证 | ✅（15 unit test 真实跑通，含 expectedScope 解析顺序、match 命中、pool 删除等关键路径） |
| 中文 evidence | ✅ |
| 不修预存在 unrelated bug | ✅ |
| 不调 `update_goal` 完成 | ✅ |
| 继续推进 | ✅ |

## 后续计划

| 轮次 | 内容 |
|---|---|
| **R677** | 找下一个完全未复刻的 Node service parity 缺口（候选：`<environment-custom-image-runtime.ts (286 行) / `environment-custom-image-terminal-sessions.ts (353 行) / `plugin-environment-driver.ts (570 行) / `plugin-job-scheduler.ts (752 行)`） |
| **R678** | pc-server prod-mode 真实启动 + 真实 OAUTH 模拟（authenticated 路径） |
