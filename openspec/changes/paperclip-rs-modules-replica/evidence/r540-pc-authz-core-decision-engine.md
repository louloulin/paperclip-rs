# R540 / M40 — pc-authz 核心决策引擎

## 本轮完成

`crates/pc-authz/src/` 从 128 行 stub 扩展为结构化授权决策引擎，对齐 Node `services/authorization.ts` 的核心分支：

### 模块拆分

| 文件 | 行数 | 职责 |
|---|---|---|
| `lib.rs` | 123 | 公共 API 导出 + 兼容旧 `DefaultPolicy` stub |
| `types.rs` | 418 | `PrincipalType` / `CompanyRole` / `PermissionKey` (21 个) / `Action` / `Resource` / `Reason` / `Decision` |
| `policy.rs` | 800+ | `Context` 注入 + `evaluate` / `check` 决策函数 |

### 核心 API

```rust
use pc_authz::{evaluate, check, Action, CompanyRole, Context, PermissionKey, Resource};

// 注入上下文
let ctx = Context::for_user(memberships, grants, Some(CompanyRole::Admin), false);

// 决策
let decision = evaluate(&actor, &ctx, &resource, Action::Permission(PermissionKey::JoinsApprove));
if !decision.allowed {
    return Err(ApiError::Forbidden(decision.explanation));
}

// 或 Result 形式
check(&actor, &ctx, &resource, action)?
```

### 决策逻辑（与 Node `evaluateAuthorization` 核心分支对齐）

1. **System 全局 allow** (`Reason::AllowInstanceAdmin`)
2. **Anonymous 直接 deny** (`Reason::DenyUnauthenticated`)；开发模式 `is_local_board` 例外
3. **User instance_admin 短路** (`Reason::AllowInstanceAdmin`)
4. **本地 board** (`Reason::AllowLocalBoard`)
5. **公司成员资格**：`has_membership` 检查 active status
6. **Issue 维度短路**：
   - assignee 直接 mutate (`AllowDirectChange`)
   - mentioned 用户 comment / mutate (`AllowIssueMentionGrant`)
   - agent self (`AllowSelf`)
7. **Permission key 解析**：先看 grants (`AllowExplicitGrant`)，再按 role (`AllowSimpleCompanyMember`)
8. **特殊 Action 默认规则**：IssueRead/ProjectRead/AgentRead/CompanyScopeRead → allow member；IssueMutate → 看 role；RuntimeManage/AgentConfigUpdate → 要 admin

### 与 Node 对齐的 Reason 标签

23 个 `Reason` 枚举值：
- 14 个 `Allow*`（low_trust_boundary / local_board / instance_admin / explicit_grant / direct_change / consented_change / legacy_agent_creator / issue_mention_grant / direct_parent_report / self / company_agent / company_member / simple_company_member / manager_chain）
- 9 个 `Deny*`（unauthenticated / company_boundary / missing_membership / missing_grant / missing_consent / no_grant / policy_restricted / forbidden / unknown_action）

### 验证

- `cargo check -p pc-authz`：0 errors
- `cargo test -p pc-authz --lib`：**24 passed**（13 个 policy 决策路径 + 6 个类型 + 1 个 lib API + 4 个 lib 内）
- `cargo check --workspace`：0 errors（17 crates）
- `cargo test --workspace --lib -- --test-threads=1`：**4953 passed**（40 suites；比 M39 增加 19 个测试）

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey（21 个常量对齐） | ✅ |
| Action / Resource / Decision / Reason 类型 | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| grants 表 / DB 集成 | ⏳ M41 |
| Issue mention / consent / parent 报告 / low-trust boundary | ⏳ M42 |
| 路由集成 / middleware | ⏳ M43-M44 |
