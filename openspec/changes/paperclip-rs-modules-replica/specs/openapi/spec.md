# openapi (M10)

`pc-openapi` utoipa 集成 + `/openapi.json` + `/openapi.yaml`。

## ADDED Requirements

### Requirement: REQ-M10-1 /openapi.json 与 /openapi.yaml 路由存在
- `GET /openapi.json` → 200 + OpenAPI 3.1 JSON
- `GET /openapi.yaml` → 200 + YAML


The system SHALL satisfy this requirement.
#### Scenario: 端点存在
- GIVEN `pc-server` 运行中
- WHEN `curl /openapi.json` 与 `/openapi.yaml`
- THEN 两端点返回 200

### Requirement: REQ-M10-2 字段命名
JSON 字段与 Node OpenAPI 字段命名 snake → camel 一致。


The system SHALL satisfy this requirement.
#### Scenario: 字段一致
- GIVEN 同一 spec
- WHEN 解析 Rust 与 Node 两份 openapi.json
- THEN keys 集合相等

### Requirement: REQ-M10-3 覆盖 56 路由
- OpenAPI 中 paths 必须覆盖 56 路由


The system SHALL satisfy this requirement.
#### Scenario: 路径数量
- GIVEN openapi.json
- WHEN 解析 paths
- THEN 数量 == 56
