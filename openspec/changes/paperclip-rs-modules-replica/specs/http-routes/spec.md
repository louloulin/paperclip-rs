# http-routes (M9)

`pc-http` axum Router + middleware + 56 路由。

## ADDED Requirements

### Requirement: REQ-M9-1 AppState
`AppState { db, bus, providers, config }` 必须可被所有 route 共享。


The system SHALL satisfy this requirement.
#### Scenario: AppState 注入
- GIVEN `axum::extract::State<AppState>`
- WHEN handler 启动
- THEN 各 route 拿到同一份依赖

### Requirement: REQ-M9-2 Middleware Stack
actor / request_id / log / cors / compression / body_limits / error_mapping 七件齐。


The system SHALL satisfy this requirement.
#### Scenario: 中间件顺序生效
- GIVEN request 包含恶意 header
- WHEN 经过 stack
- THEN 各 middleware 逐层生效（顺序以 design 为准）

### Requirement: REQ-M9-3 56 Routes 字节级一致
56 路由与原 server 同 fixture 下字节级一致（happy + 3 edge × 56）。


The system SHALL satisfy this requirement.
#### Scenario: route happy
- GIVEN 每条路由
- WHEN 与 Node server 并行跑同一 fixture
- THEN status code + JSON body 字节级一致

#### Scenario: route edge
- GIVEN 每个 route 3 edge case（missing arg / unauth / invalid input）
- WHEN 与 Node 对比
- THEN 字节级一致
