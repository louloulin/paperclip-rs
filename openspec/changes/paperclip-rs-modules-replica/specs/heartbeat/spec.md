# heartbeat (M12)

`pc-heartbeat` 状态机 + 端到端剧本。

## ADDED Requirements

### Requirement: REQ-M12-1 状态机
`pick_runnable → invoke_adapter → finalize` 三态清晰可观察。


The system SHALL satisfy this requirement.
#### Scenario: 主路径
- GIVEN 一个 runnable issue
- WHEN heartbeat tick
- THEN 依次 pick → invoke → finalize，并写 activity_log

### Requirement: REQ-M12-2 Node semantics 对齐
对照 `services/heartbeat.ts` + `recovery/*` 全部行为（stranded issue / stale run / continuation / escalation / quota monitor / review handoff ...）逐项实现。


The system SHALL satisfy this requirement.
#### Scenario: 全部 round*.rs 测试通过
- GIVEN 现有 round320–357 集成测试
- WHEN `cargo test -p pc-heartbeat`
- THEN 全绿

### Requirement: REQ-M12-3 端到端剧本
从 POST /heartbeat 到 live-event 推送的完整链路在 5s 内闭环。


The system SHALL satisfy this requirement.
#### Scenario: e2e 剧本
- GIVEN 配置好的 issue
- WHEN `POST /heartbeat`
- THEN 5s 内 ActivityLog 与 WebSocket 推送同时出现
