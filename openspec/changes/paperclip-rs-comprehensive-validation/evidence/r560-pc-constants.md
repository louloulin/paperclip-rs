# R560 — pc-constants 新 crate（2026-08-11）

> paperclip Node `packages/shared/src/constants.ts`（1647 LOC, 199 个 export const）的精选 port。
> 按业务域分模块，每个常量是 `pub const FOO: &[&str]` 与 Node TS `as const` 数组 1:1 对齐。

## 1. 设计决策

### 1.1 不整体 port 199 个常量
Node `constants.ts` 包含大量：
- 已在域 crate 里 port 的常量（DEPLOYMENT_MODES → pc-network-bind ✅、AGENT_ADAPTER_TYPES → pc-adapter-type ✅、AGENT_STATUSES → pc-agent 部分）
- 标签映射（`AGENT_ROLE_LABELS: Record<AgentRole, string>`）—— 应放 UI/i18n crate，不在 constants
- 计算后的常量（`INBOX_MINE_ISSUE_STATUS_FILTER = ...join(",")`）—— 本 crate 也保留，因为是上游 1:1 API contract

### 1.2 按域分模块
| 模块 | 内容 | LOC | 测试 |
|---|---|---|---|
| `company.rs` | COMPANY_STATUSES / 附件限制 / PRINCIPAL_TYPES / MEMBERSHIP_STATUSES / INVITE_TYPES / JOIN_REQUEST_* | ~80 | 6 |
| `agent.rs` | AGENT_DEFAULT_MAX_CONCURRENT_RUNS / WORKSPACE_BRANCH_ROUTINE_VARIABLE / AGENT_ICON_NAMES / PROJECT_ICON_NAMES / ADAPTER_AGNOSTIC_KEYS / MODEL_PROFILE_KEYS | ~85 | 5 |
| `issue.rs` | ISSUE_STATUSES / INBOX_* / ISSUE_PRIORITIES / ISSUE_WORK_MODES / ISSUE_RELATION_TYPES / ISSUE_TREE_* / ISSUE_ORIGIN_KINDS / ISSUE_RECOVERY_* / ISSUE_THREAD_INTERACTION_* / 文档 keys | ~110 | 9 |
| `heartbeat.rs` | HEARTBEAT_INVOCATION_SOURCES / WAKEUP_TRIGGER_DETAILS / WAKEUP_REQUEST_STATUSES / HEARTBEAT_RUN_STATUSES / RUN_LIVENESS_STATES / LIVE_EVENT_TYPES | ~50 | 4 |
| `budget.rs` | BUDGET_SCOPE_TYPES / BUDGET_METRICS / BUDGET_WINDOW_KINDS / BUDGET_THRESHOLD_TYPES / BUDGET_INCIDENT_* / BILLING_TYPES / COST_STATUSES / FINANCE_* / STORAGE_PROVIDERS | ~50 | 6 |
| `workflow.rs` | PIPELINE_CASE_STATUSES / PIPELINE_STAGE_KINDS / PIPELINE_TRIGGER_KINDS / ROUTINE_TRIGGER_KINDS / ROUTINE_STATUSES / DECISION_EFFECT_TYPES / APPROVAL_* / DOCUMENT_ANNOTATION_* / EXTERNAL_OBJECT_* | ~85 | 7 |

**总计 6 模块 / ~460 LOC / 37 unit tests + 12 integration tests = 49 tests**

### 1.3 关键常量值（与 Node 上游 1:1）

#### Company
```rust
pub const COMPANY_STATUSES: &[&str] = &["active", "paused", "archived"];
pub const DEFAULT_COMPANY_ATTACHMENT_MAX_BYTES: usize = 10 * 1024 * 1024;  // 10 MiB
pub const MAX_COMPANY_ATTACHMENT_MAX_BYTES: usize = 1024 * 1024 * 1024;    // 1 GiB
pub const PRINCIPAL_TYPES: &[&str] = &["user", "agent"];
pub const MEMBERSHIP_STATUSES: &[&str] = &["pending", "active", "suspended", "archived"];
pub const INVITE_TYPES: &[&str] = &["company_join", "bootstrap_ceo"];
pub const INVITE_JOIN_TYPES: &[&str] = &["human", "agent", "both"];
pub const JOIN_REQUEST_STATUSES: &[&str] = &["pending_approval", "approved", "rejected"];
```

#### Issue
```rust
pub const ISSUE_STATUSES: &[&str] = &[
    "backlog", "todo", "in_progress", "in_review",
    "done", "cancelled", "blocked",
];
pub const INBOX_MINE_ISSUE_STATUSES: &[&str] = &["todo", "in_progress", "in_review", "blocked"];
pub const ISSUE_PRIORITIES: &[&str] = &["critical", "high", "medium", "low"];
pub const ISSUE_WORK_MODES: &[&str] = &["standard", "ask", "planning", "skill_test"];
pub const ISSUE_RELATION_TYPES: &[&str] = &["blocks"];
pub const ISSUE_TREE_CONTROL_MODES: &[&str] = &["pause", "resume", "cancel", "restore"];
pub const MAX_ISSUE_REQUEST_DEPTH: u32 = 1024;
```

#### Heartbeat
```rust
pub const HEARTBEAT_RUN_STATUSES: &[&str] = &[
    "queued", "running", "scheduled_retry",
    "succeeded", "failed", "cancelled", "timed_out",
];
pub const RUN_LIVENESS_STATES: &[&str] = &["alive", "silent", "stalled", "stuck", "done"];
pub const LIVE_EVENT_TYPES: &[&str] = &[/* 21 个事件类型 */];
```

## 2. 公开 API（crate 顶层 re-export）

```rust
use pc_constants::{
    // company
    COMPANY_STATUSES, DEFAULT_COMPANY_ATTACHMENT_MAX_BYTES,
    MAX_COMPANY_ATTACHMENT_MAX_BYTES, PRINCIPAL_TYPES,
    MEMBERSHIP_STATUSES, COMPANY_MEMBERSHIP_ROLES,
    HUMAN_COMPANY_MEMBERSHIP_ROLES, INSTANCE_USER_ROLES,
    INVITE_TYPES, INVITE_JOIN_TYPES, JOIN_REQUEST_TYPES,
    JOIN_REQUEST_STATUSES,
    // agent
    AGENT_DEFAULT_MAX_CONCURRENT_RUNS, WORKSPACE_BRANCH_ROUTINE_VARIABLE,
    AGENT_ICON_NAMES, PROJECT_ICON_NAMES, ADAPTER_AGNOSTIC_KEYS, MODEL_PROFILE_KEYS,
    // issue
    ISSUE_STATUSES, INBOX_MINE_ISSUE_STATUSES, INBOX_MINE_ISSUE_STATUS_FILTER,
    ISSUE_PRIORITIES, ISSUE_WORK_MODES, ISSUE_HARNESS_KINDS,
    MAX_ISSUE_REQUEST_DEPTH, ISSUE_RELATION_TYPES, ISSUE_TREE_CONTROL_MODES,
    ISSUE_TREE_HOLD_STATUSES, ISSUE_ORIGIN_KINDS, ...,
    // heartbeat
    HEARTBEAT_INVOCATION_SOURCES, WAKEUP_TRIGGER_DETAILS,
    WAKEUP_REQUEST_STATUSES, HEARTBEAT_RUN_STATUSES,
    RUN_LIVENESS_STATES, LIVE_EVENT_TYPES,
    // budget
    BUDGET_SCOPE_TYPES, BUDGET_METRICS, BUDGET_WINDOW_KINDS, ...,
    // workflow
    PIPELINE_CASE_STATUSES, PIPELINE_STAGE_KINDS, PIPELINE_TRIGGER_KINDS,
    ROUTINE_TRIGGER_KINDS, ROUTINE_STATUSES, DECISION_EFFECT_TYPES, ...,
};
```

## 3. 验证结果

### 3.1 单元测试（lib）
```
running 37 tests
test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 3.2 集成测试
```
running 12 tests
test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 3.3 clippy + fmt
```
cargo clippy -p pc-constants --all-targets -- -D warnings
  → 0 warnings ✅
cargo fmt -p pc-constants --check
  → no diff ✅
```

### 3.4 关键 invariant 测试覆盖
- `attachments < max_attachments`（编译期 const block assert）
- Human roles ⊆ Company roles
- Inbox filter status ⊆ Issue statuses
- Inbox filter string = joined statuses
- Task watchdog kind ∈ ISSUE_ORIGIN_KINDS
- HEARTBEAT_RUN_STATUSES 包含全部 terminal 状态
- PIPELINE_TRIGGER_KINDS ⊆ ROUTINE_TRIGGER_KINDS（routine 是 superset）
- 每个常量数组内无重复项
- SYSTEM_ISSUE_DOCUMENT_KEYS 包含 continuation + pipeline case body

## 4. 与现有 crate 解耦

- **不重复 port** 已在域 crate 的常量：
  - `DEPLOYMENT_MODES` / `BIND_MODES` / `DEPLOYMENT_EXPOSURES` → `pc-network-bind` ✅
  - `AGENT_ADAPTER_TYPES` → `pc-adapter-type` ✅（enum，更强类型）
  - `AGENT_STATUSES` → `pc-agent`（部分，作为 enum variants）
  - `PLUGIN_*` / `TOOL_*` / `PERMISSION_KEYS` → 留在后续轮次 port（pc-plugin-host / pc-tool）
- **不 port** 上游的 label maps（`AGENT_ROLE_LABELS: Record<AgentRole, string>`）—— UI/i18n 范畴
- **跨域引用**：其他 crate 可以 `use pc_constants::issue::ISSUE_STATUSES` 复用

## 5. workspace 累计成果（R560 末）

- **workspace crates**：**100 → 101**（+1）
- **新增 crate**：`pc-constants`（460 LOC + 49 tests）
- **覆盖 Node `shared/src/constants.ts` 第一批 ~30%**（60 / 199 个 export const）
- **后续轮次 port 计划**：
  - R561：plugin / tool domain（PLUGIN_*, TOOL_*, PERMISSION_KEYS）—— ~80 个常量
  - R562：labels / i18n（AGENT_ROLE_LABELS 等）—— 单独 crate 或 ui crate
  - R563：storage / backup domain（已在 pc-storage / pc-backup 里）
- **clippy 0 warnings**：✅
- **fmt clean**：✅

## 6. 模块层覆盖率（R560 末）

| 口径 | R558 | R560 |
|---|---|---|
| Node `shared/src/` 顶层 32 个 .ts | 30 ported | 30 + constants.ts 第1批 |
| workspace crates | 100 | **101** |
| 模块层覆盖率（顶层） | 93.75% | **~95%**（constants.ts 第一批） |

