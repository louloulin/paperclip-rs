# plugin-host (M14)

`pc-plugin-protocol` + `pc-plugin-host`。

## ADDED Requirements

### Requirement: REQ-M14-1 Protocol Schema
JSON-RPC 2.0 schema 完全覆盖 `define-plugin.ts` 的所有 method。


The system SHALL satisfy this requirement.
#### Scenario: method 覆盖
- GIVEN 原 SDK 全部 method
- WHEN 解析 Rust 协议
- THEN 1:1 对齐

### Requirement: REQ-M14-2 Host 9 Service
EventBus / JobScheduler / JobStore / ToolDispatcher / DatabaseBridge / StateStore / WebhookDispatcher / ManifestValidator / CapabilityValidator 九件齐，对应原 9 个 service。


The system SHALL satisfy this requirement.
#### Scenario: 9 件齐
- GIVEN `pc-plugin-host/src/`
- WHEN `grep -E "pub (struct|trait) (EventBus|JobScheduler|JobStore|ToolDispatcher|DatabaseBridge|StateStore|WebhookDispatcher|ManifestValidator|CapabilityValidator)"`
- THEN 全部命中

### Requirement: REQ-M14-3 与原 SDK worker 互操作
同一 plugin npm 包，原 host 与 Rust host 启动能跑到 invoke 一次。


The system SHALL satisfy this requirement.
#### Scenario: 互操作
- GIVEN 同一 plugin manifest
- WHEN 原 host 与 Rust host 各启一次
- THEN invoke 行为一致
