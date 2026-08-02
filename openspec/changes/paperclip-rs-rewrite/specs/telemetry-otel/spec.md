## ADDED Requirements

### Requirement: tracing + JSON 日志 + OpenTelemetry（默认关闭） SHALL
The system SHALL satisfy the following behavior.

`pc-telemetry` 默认输出结构化 JSON 日志；通过 `OTEL_EXPORTER_OTLP_ENDPOINT` 启用 OTLP exporter。

#### Scenario: 默认行为
- **WHEN** 未设置 `OTEL_EXPORTER_OTLP_ENDPOINT`
- **THEN** 日志以 JSON 输出到 stdout，不发起 OTLP 连接
