# R544 — pc-agent-eligibility（Node agent-eligibility.ts 复刻）

日期：2026-08-11

## 完成内容

将 `paperclip/packages/shared/src/agent-eligibility.ts` (245 LOC) 完整复刻到新 crate
`crates/pc-agent-eligibility`。workspace crates 85 → **86**。

## 设计原则

### 1. Strong typing for status
- `AgentStatus` enum (`Active | Idle | Running | Paused | Error | Terminated | PendingApproval | Other(String)`)
  - `Other(String)` 捕获 unknown status 保持向后兼容（上游 `AgentStatus | string`）
  - `from_db(&str)` 辅助函数在 DB 边界做一次 string → enum 转换，调用方拿到强类型
  - `as_str()` 反向转换
- 替代 TS 的 `AgentStatus | string` 弱类型

### 2. Closed-set enums for reasons
- `AgentEligibilityLifecycleReason` / `AgentOrgChainInvalidReason` / `AgentOrgChainHealthStatus` /
  `AgentOrgChainRelation` 四个 enum 把所有上游 literal union 显式化
- `#[serde(rename_all = "snake_case")]` 保持 wire 兼容

### 3. Cycle-safe traversal
- 用 `HashSet<String>` seen-set，A→B→A 在走第二圈时终止
- Self-referential cycle (A.reports_to == A.id) 在第一步就检测到
- Cross-company parent (parent.company_id != agent.company_id) 标记为 `MissingManager`

### 4. EligibilityInput wrapper
- `EligibilityInput { agent, agents }` 替代 node `input: { agent, agents }`
- 借用 `&'a` 避免 clone
- 调用方传 `&input(&a, &roster)` 一次性拿到 `&EligibilityInput`

### 5. repair_guidance 三个分支
- missing / cycle / terminated 三个不同的可读建议模板
- 与 Node 上游 byte-compatible（用 format! 拼装，与 JS 模板字符串等价）

### 6. Status precedence 显式
- `get_agent_work_eligibility` 中：
  - status 不 ok → 返回具体 status reason (Terminated / PendingApproval / Paused / UnknownStatus)
  - status ok 但 chain invalid → 返回 `InvalidOrgChain`
  - 两者都 ok → 返回 `Eligible`
- 与 Node 嵌套三元运算符完全等价

## 公开 API

```rust
pub enum AgentStatus { Active, Idle, Running, Paused, Error, Terminated, PendingApproval, Other(String) }
pub enum AgentEligibilityLifecycleReason { Eligible, Terminated, PendingApproval, Paused, InvalidOrgChain, UnknownStatus }
pub enum AgentOrgChainRelation { Subject, Ancestor }  // Subject 替代上游 Self (Rust 关键字)
pub enum AgentOrgChainInvalidReason { Healthy, TerminatedAncestor, MissingManager, Cycle }
pub enum AgentOrgChainHealthStatus { Healthy, InvalidOrgChain }

pub struct AgentEligibilityAgent { id, company_id, name, status: AgentStatus, reports_to: Option<String> }
pub struct AgentOrgChainEntry { id, company_id, name, status, reports_to, depth, relation }
pub struct AgentInvalidOrgChainAncestor { id, name, status }
pub struct AgentOrgChainHealth { status, reason, full_chain, first_invalid_ancestor, invalid_ancestors, repair_guidance }
pub struct AgentWorkEligibility { assignable, invokable, assignability_reason, invokability_reason, org_chain_health }
pub struct EligibilityInput<'a> { agent: &'a AgentEligibilityAgent, agents: &'a [AgentEligibilityAgent] }

impl AgentStatus {
    pub fn from_db(value: &str) -> Self
    pub fn as_str(&self) -> &str
}

pub fn is_agent_status_assignable_to_work(status: &AgentStatus) -> bool
pub fn is_agent_status_invokable(status: &AgentStatus) -> bool
pub fn get_agent_org_chain_health(input: &EligibilityInput<'_>) -> AgentOrgChainHealth
pub fn get_agent_work_eligibility(input: &EligibilityInput<'_>) -> AgentWorkEligibility
pub fn is_agent_assignable_to_work(input: &EligibilityInput<'_>) -> bool
pub fn is_agent_invokable(input: &EligibilityInput<'_>) -> bool
```

## 与上游 Node 差异

- **`Self` → `Subject`**：Rust `Self` 是关键字不能作 enum variant，使用 `Subject` 更准确
  （描述「该 agent 是查询主题」而不是「自身」）
- **`AgentStatus` enum + `from_db`**：从 string → enum 转换推到 DB 边界，函数本身只接受强类型
- **serde JSON 字段 camelCase**：`full_chain` → `fullChain`、`assignability_reason` → `assignabilityReason`
  （与 Node 上游 wire format 兼容）

## 真实验证

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-agent-eligibility` | **22 passed** (1 internal + 22 integration) |
| `cargo fmt -p pc-agent-eligibility -- --check` | ✅ 通过 |
| `cargo clippy -p pc-agent-eligibility --all-targets -- -D warnings` | ✅ 0 errors |

## 测试覆盖（22 个集成测试 + 1 internal）

- **AgentStatus enum** (2): from_db round-trip / 未知状态捕获
- **Status predicates** (2): assignable 矩阵 / invokable 矩阵
- **get_agent_org_chain_health** (7): 健康 root / 健康三级链 / terminated ancestor /
  missing manager / 跨公司 / 二节点 cycle / 自引用 cycle / 多 invalid ancestor
- **get_agent_work_eligibility** (6): eligible / terminated / pending_approval /
  paused (assignable 但 not invokable) / unknown status / invalid chain 覆盖 / status 覆盖 invalid chain
- **Convenience wrappers** (1): is_assignable / is_invokable 一致性
- **Serde** (2): AgentOrgChainHealth camelCase / AgentWorkEligibility JSON 往返

## 集成待办（不在本轮范围）

- `pc-agents` 服务：assignable 检查、issue 分配前过滤 invokable
- `pc-routines` / `pc-decisions`：eligibility 检查集成到 dispatch 决策
- `pc-inbox` UI：根据 org chain 显示 badge（health status）
