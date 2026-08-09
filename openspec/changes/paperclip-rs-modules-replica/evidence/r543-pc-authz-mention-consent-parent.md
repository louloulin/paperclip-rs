# R543 / M43 — pc-authz 补充策略（mention / consent / parent-report）

## 本轮完成

扩展 `Context` 与决策函数，覆盖 Node `authorization.ts` 中的 4 个剩余策略分支：

### Context 新增字段

- `issue_mentioned_agent_ids: Vec<Uuid>` — issue body 中提及的 agent
- `issue_parent_id: Option<Uuid>` — issue 的 parent issue id
- `actor_is_assignee_on_parent: bool` — 当前 run 的 actor agent 在 parent 上是 assignee
- `has_consented_change_grant: bool` — grant scope 包含 `consentedChange`
- `is_low_trust_create_or_comment: bool` — low-trust 内允许的 create / comment

新增 `Context::with_extended_issue(...)` 构造方法。

### 决策新增分支

1. **User responsible_user 短路**（`AllowDirectChange`）：issue 的 `responsible_user_id == user_id` 时直接 mutate/comment
2. **Agent mention grant**（`AllowIssueMentionGrant`）：当前 actor agent 在 `issue_mentioned_agent_ids` 内时可 comment/mutate
3. **Agent parent-report**（`AllowDirectParentReport`）：actor 在 parent 上是 assignee + 当前 issue 有 parent_id + comment 动作时允许
4. **Consent gate**（`AllowConsentedChange`）：grant scope 含 `consentedChange` 时 mutate 允许（user + agent 均生效）

### 验证

- `cargo check -p pc-authz`：0 errors
- `cargo test -p pc-authz --lib`：**33 passed**（+5 个新测试）
  - `user_responsible_user_can_mutate`（responsible_user 短路）
  - `user_viewer_cannot_mutate_without_assignment`（viewer 默认 deny）
  - `agent_mention_grant_allows_comment`（mention grant）
  - `agent_parent_report_allows_comment`（parent-report）
  - `agent_consent_grant_allows_mutate`（consent gate）
- `cargo test --workspace --lib -- --test-threads=1`：**4962 passed**（+5）

### 复刻进度（pc-authz）

| 子系统 | 状态 |
|---|---|
| PrincipalType / CompanyMembershipRole | ✅ |
| PermissionKey (21) / Action / Resource / Decision / Reason | ✅ |
| 决策函数（核心分支对齐 Node） | ✅ |
| DB-backed ContextBuilder | ✅ M41 |
| HTTP 便捷 API（enforce_*） | ✅ M42 |
| Mention / consent / parent-report / responsible_user | ✅ M43 |
| Low-trust boundary 完整复刻 | ⏳ M44（部分 — `is_low_trust_create_or_comment` 已加） |
| 全局接入所有受保护路由 | ⏳ 渐进 |
| Low-trust preset 解析 / DB-backed boundary | ⏳ |
