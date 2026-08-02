## ADDED Requirements

### Requirement: 自动生成 OpenAPI 3.1 文档 SHALL
The system SHALL satisfy the following behavior.

`pc-openapi` 通过 utoipa 在编译期生成 OpenAPI 3.1；与 `server/src/routes/openapi.ts` 输出字段对齐。

#### Scenario: 拉取文档
- **WHEN** `GET /openapi.json`
- **THEN** 返回 200 + 合法 JSON
