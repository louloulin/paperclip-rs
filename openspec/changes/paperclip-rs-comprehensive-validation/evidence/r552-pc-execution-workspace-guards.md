# R552 — pc-execution-workspace-guards（Node execution-workspace-guards.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/execution-workspace-guards.ts` (19 LOC) 完整复刻到新 crate
`crates/pc-execution-workspace-guards`。workspace crates 93 → **94**。

## 设计原则

### 1. enum 强类型 status / mode
- Node 用字符串联合 `"active" | "idle" | "in_review" | "archived" | "cleanup_failed"`
- Rust 用 `enum ExecutionWorkspaceStatus` + 5 个变体 + `as_str()` / `parse()` round-trip
- 同样 enum 化 `ExecutionWorkspaceMode` 6 种 mode

### 2. `HashSet<&'static str>` 零拷贝 closed statuses
- Node 用 `new Set(["archived", "cleanup_failed"])`
- Rust 用 `closed_execution_workspace_statuses()` 返回 `HashSet<&'static str>`
- 0 分配，0 拷贝

### 3. `Option<&T>` 入参自动处理 None
- Node 用 `null | undefined` + truthy 判断
- Rust 用 `Option<&ExecutionWorkspaceGuardTarget>` 直接 pattern match
- 调用方语义清晰：`is_closed_isolated_execution_workspace(None)` → false

### 4. 自定义 `ExecutionWorkspaceGuardTarget` struct
- 只承载 4 个需要的字段（closed_at / mode / name / status）
- 不绑定完整的 `ExecutionWorkspace` 类型（解耦）
- 业务侧可从任意 ExecutionWorkspace 投影出 4 字段后传入

## 公开 API

```rust
pub enum ExecutionWorkspaceStatus { Active, Idle, InReview, Archived, CleanupFailed }
impl ExecutionWorkspaceStatus { pub fn as_str / pub fn parse }

pub enum ExecutionWorkspaceMode { SharedWorkspace, IsolatedWorkspace, OperatorBranch, ReuseExisting, Inherit, AgentDefault }
impl ExecutionWorkspaceMode { pub fn as_str / pub fn parse }

pub fn closed_execution_workspace_statuses() -> HashSet<&'static str>

pub struct ExecutionWorkspaceGuardTarget {
    pub closed_at: Option<String>,
    pub mode: ExecutionWorkspaceMode,
    pub name: String,
    pub status: ExecutionWorkspaceStatus,
}

pub fn is_closed_isolated_execution_workspace(workspace: Option<&ExecutionWorkspaceGuardTarget>) -> bool
pub fn get_closed_isolated_execution_workspace_message(workspace: &ExecutionWorkspaceGuardTarget) -> String
```

## 与上游 Node 差异

- **enum + as_str/parse**：替代字符串字面量 union
- **Option<&T>**：替代 `null | undefined`
- **Minimal struct**：不绑定完整 `ExecutionWorkspace`

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-execution-workspace-guards` | **17 passed** (7 internal + 10 integration) |
| `cargo fmt -p pc-execution-workspace-guards` | ✅ 通过 |
| `cargo clippy -p pc-execution-workspace-guards --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（17 个）

- **closed status set** (1): 包含 archived / cleanup_failed，不含 active
- **enum round-trip** (2): status (5) / mode (6) 都覆盖
- **is_closed 行为** (6): None / 非 isolated / isolated open / isolated archived / isolated cleanup_failed / closed_at 优先
- **message** (1): 包含 workspace.name
