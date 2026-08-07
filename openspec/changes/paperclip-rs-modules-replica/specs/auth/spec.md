# auth (M7)

`pc-auth` + `pc-authz` 行为复刻。

## ADDED Requirements

### Requirement: REQ-M7-1 Session/Cookie/CSRF
完整 session 模型，cookie 名 `pc_session` + `pc_csrf`，CSRF token 用于 state-changing 请求。


The system SHALL satisfy this requirement.
#### Scenario: session 校验通过
- GIVEN session cookie 有效
- WHEN 请求
- THEN actor 被解析，无 401

### Requirement: REQ-M7-2 API Key
格式 `pk_<base62>`，Authorization Bearer。


The system SHALL satisfy this requirement.
#### Scenario: api key 通过
- GIVEN `Authorization: Bearer pk_xxxx`
- WHEN 请求
- THEN actor 解析正确

### Requirement: REQ-M7-3 Policy Trait + Authorization
5 mode × N resource 全部 1:1 映射 `services/authorization.ts` 行为。


The system SHALL satisfy this requirement.
#### Scenario: 80+ case
- GIVEN resource 集 {company, agent, issue, project, case, approval, decision, routine, pipeline, environment, plugin, secret, ...}
- WHEN 每个 resource × allow/deny/not_owner/anon/elevated 各跑一次
- THEN 全部行为与 Node 同 fixture 一致
