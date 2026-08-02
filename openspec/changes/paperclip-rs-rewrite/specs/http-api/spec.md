## ADDED Requirements

### Requirement: HTTP 路由路径与方法与原 server 完全一致 SHALL
The system SHALL satisfy the following behavior.

`pc-http` 通过 axum 暴露的路由必须与原 Node server 的 56 个路由模块的路径、方法、请求/响应 schema 一致，前端 UI（`paperclip/ui/`）无需任何修改即可对接。

#### Scenario: 公司列表端点
- **WHEN** `GET /api/companies` 带有效 session cookie
- **THEN** 返回 200 + `[{id, name, ...}, ...]`，与原 server 字节级一致

#### Scenario: 创建 agent 端点
- **WHEN** `POST /api/companies/{companyId}/agents` 带合法 zod 等价校验
- **THEN** 返回 201 + 新 agent 实体

### Requirement: 请求体验证失败返回 400 + 结构化错误 SHALL
The system SHALL satisfy the following behavior.

校验失败时统一返回 `400 Bad Request` + `{error: {code: "validation_error", details: [{path, message}]}}`。

#### Scenario: 缺字段
- **WHEN** `POST /api/issues` 缺 `title`
- **THEN** 返回 400 + `details[0].path == "title"`

### Requirement: OpenAPI 文档自动生成 SHALL
The system SHALL satisfy the following behavior.

`pc-openapi` 在编译期从 axum 路由 derive 生成 OpenAPI 3.1 文档，暴露在 `GET /openapi.json` 与 `GET /openapi.yaml`。

#### Scenario: 拉取 OpenAPI
- **WHEN** `GET /openapi.json`
- **THEN** 返回 200 + 合法 OpenAPI 3.1 JSON，包含所有路由与 schema
