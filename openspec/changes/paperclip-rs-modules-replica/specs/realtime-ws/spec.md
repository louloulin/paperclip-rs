# realtime-ws (M11)

`pc-realtime` + `pc-ws` 真实 WS 推送。

## ADDED Requirements

### Requirement: REQ-M11-1 Bus Trait
`pub trait Bus: Send + Sync` 抽象 publish + subscribe，与 Node live-events 行为对齐。


The system SHALL satisfy this requirement.
#### Scenario: 进程内广播
- GIVEN `InMemoryBus`
- WHEN publish 一个 `LiveEvent::IssueCreated` 到两个订阅者
- THEN 两个订阅者均收到，且时序正确

### Requirement: REQ-M11-2 WS Endpoint
`GET /api/live-events` 升级 WebSocket，握手同 cookie/api_key 鉴权，30s ping。


The system SHALL satisfy this requirement.
#### Scenario: WS 鉴权
- GIVEN 无 cookie
- WHEN 升级 WS
- THEN 401 close

### Requirement: REQ-M11-3 Last-Event Replay
订阅时带 `Last-Event-Id` header，bus 必须回放该 id 之后的事件。


The system SHALL satisfy this requirement.
#### Scenario: 断线重连
- GIVEN 上一次断开时 last_event_id=10
- WHEN 重连带该 id
- THEN 收到 id>10 的事件，未重复
