## ADDED Requirements

### Requirement: WebSocket live-events 通道 SHALL
The system SHALL satisfy the following behavior.

`pc-ws` 在 `GET /live-events` 升级 WebSocket；token 通过 query string `?token=` 或首包验证；消息协议：`subscribe` / `unsubscribe` / `ping` ↔ `event` / `pong` / `error`。

#### Scenario: 建立连接
- **WHEN** 客户端带有效 token 升级 WebSocket
- **THEN** 服务端返回 101 Switching Protocols，等待 `subscribe`

#### Scenario: 订阅公司事件
- **WHEN** 客户端发送 `{"op":"subscribe","companyId":"<uuid>"}`
- **THEN** 该 socket 收到 company 内所有 live-event，直到 `unsubscribe` 或断连

### Requirement: ping/pong 心跳与超时 SHALL
The system SHALL satisfy the following behavior.

服务端每 30s 发 ping，客户端 30s 内未 pong 则断开。

#### Scenario: 客户端不响应 ping
- **WHEN** 30s 内无 pong
- **THEN** 服务端关闭连接（code 1001）
