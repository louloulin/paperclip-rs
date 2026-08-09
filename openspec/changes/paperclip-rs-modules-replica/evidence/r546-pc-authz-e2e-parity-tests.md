# R546 / M46 — pc-authz e2e parity 测试

## 本轮完成

新增 22 个 e2e parity 测试 + 6 个 DB-backed 测试，对齐 Node `authorization-service.test.ts` 的核心场景。

### 测试文件

| 文件 | 类型 | 测试数 | 说明 |
|---|---|---|---|
| `tests/parity_node.rs` | 纯函数（无 DB） | 22 | 对齐 Node service 测试的关键决策分支 |
| `tests/builder_db_e2e.rs` | DB-backed（自动 skip） | 6 | ContextBuilder 真实 PG e2e |

### Parity 测试清单（22）

| 测试 | 对齐 Node |
|---|---|
| `parity_user_role_grant_allows_tasks_assign` | "allows active user role grants" |
| `parity_agent_suggest_grant_allows_agent_config_read` | "allows suggest grants to read peer agent configuration" |
| `parity_instance_admin_short_circuits_all_actions` | admin 短路（多 action 验证） |
| `parity_anonymous_is_always_denied` | anonymous deny |
| `parity_system_is_universal_allow` | system 全局 allow |
| `parity_cross_company_is_denied_for_user` | 跨公司 deny |
| `parity_admin_role_unlocks_admin_actions` | admin 4 类 action |
| `parity_operator_role_lacks_admin_only_keys` | operator deny admin-only |
| `parity_issue_assignee_can_mutate` | issue assignee mutate |
| `parity_issue_mention_grant_for_user` | mention grant |
| `parity_responsible_user_can_mutate_issue` | responsible_user |
| `parity_agent_self_via_assignee_can_mutate` | agent self (assignee) |
| `parity_agent_self_run_allows_comment` | agent self (self_run) |
| `parity_agent_mention_grant_allows_comment` | agent mention grant |
| `parity_agent_parent_report_allows_comment` | parent-report |
| `parity_agent_consent_grant_allows_mutate` | consent gate |
| `parity_agent_without_grant_cannot_write` | agent write deny (4 类 key) |
| `parity_agent_can_read_company_resources` | agent read 默认 allow |
| `parity_company_member_can_read_by_default` | member read 默认 allow |
| `parity_viewer_cannot_mutate_without_assignment` | viewer mutate deny |
| `parity_pending_membership_is_denied` | pending membership deny |
| `parity_grant_overrides_insufficient_role` | grant 覆盖 role 不足 |

### DB-backed 测试清单（6）

| 测试 | 验证 |
|---|---|
| `builder_loads_user_membership_and_role` | 从 DB 加载 membership + role |
| `builder_loads_user_grants` | 从 DB 加载 grants 并验证决策 |
| `builder_loads_agent_membership` | Agent membership 加载 |
| `builder_instance_admin_short_circuits` | Instance admin 短路 |
| `builder_anonymous_returns_empty_context` | Anonymous 空 context |
| `builder_system_returns_empty_context` | System 空 context |

### 验证

- `cargo test -p pc-authz`：**61 passed, 2 ignored**（4 suites）
  - lib: 33
  - parity_node: 22
  - builder_db_e2e: 6 (无 DB 时 vacuous pass)
- `cargo test --workspace --lib -- --test-threads=1`：**4962 passed**（无回归）

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| HTTP 便捷 API（enforce_*） | ✅ M42 |
| Mention / consent / parent-report / responsible_user | ✅ M43 |
| 路由接入（12 个） | ✅ M42-M45 |
| **e2e parity 测试 vs Node（22 + 6）** | ✅ M46 |
| Low-trust preset 解析 / DB-backed boundary | ⏳ M47 |
| 全量接入所有受保护路由 | ⏳ 渐进 |
