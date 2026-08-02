## ADDED Requirements

### Requirement: 资源/动作/主体三元组策略 SHALL
The system SHALL satisfy the following behavior.

`pc-authz` 提供 `Policy<S, A, R>` trait；每个策略以 `(Subject, Action, Resource)` 三元组评估；策略表来自原 `services/authorization.ts`。

#### Scenario: 议题分配
- **WHEN** agent 试图 `issue.assign(issue_id)` 且自己不是 `issue.assignee` 也不是 board 成员
- **THEN** 返回 `AuthzError::Forbidden`
