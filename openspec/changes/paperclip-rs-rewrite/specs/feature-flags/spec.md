## ADDED Requirements

### Requirement: 特性开关 catalog SHALL
The system SHALL satisfy the following behavior.

`pc-feature-flags` 提供与 `shared/feature-catalog.ts` 等价的特性表；通过 `GET /feature-flags` 暴露给前端。

#### Scenario: 获取特性列表
- **WHEN** `GET /feature-flags` 带 session
- **THEN** 返回 200 + `[{key, defaultEnabled, ...}]`
