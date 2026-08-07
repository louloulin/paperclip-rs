# ui-integration (U1–U3)

UI 切流 + Playwright e2e + OpenAPI ↔ UI 类型对齐。

## ADDED Requirements

### Requirement: REQ-UI-1 VITE_API_BASE 切换
`paperclip-rs/ui/` 直接复用 `paperclip/ui/`，通过 `VITE_API_BASE` 指向 Rust server。


The system SHALL satisfy this requirement.
#### Scenario: 切流
- GIVEN Rust server 跑在 3100
- WHEN UI 启动并设 `VITE_API_BASE=http://localhost:3100`
- THEN UI 60 个 api client 请求落到 Rust server

### Requirement: REQ-UI-2 Playwright e2e
`tests/e2e/` 整剧本：起 PG + pc-server + UI → 登录 → 创建公司 → 创建 issue → 启动 heartbeat → 收 live-event


The system SHALL satisfy this requirement.
#### Scenario: 端到端
- GIVEN 整脚本
- WHEN 跑
- THEN 全过

### Requirement: REQ-UI-3 OpenAPI ↔ UI 类型对齐
- 服务端 `openapi.json` 与 UI 60 client 文件字段 1:1 对齐。


The system SHALL satisfy this requirement.
#### Scenario: 类型对齐
- GIVEN OpenAPI
- WHEN 解析每个 client 文件签名
- THEN 字段名 + 类型一致
