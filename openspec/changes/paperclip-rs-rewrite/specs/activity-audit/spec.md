## ADDED Requirements

### Requirement: 活动日志、成本事件、决策训练样本的统一写入 SHALL
The system SHALL satisfy the following behavior.

`pc-activity` 把 activity_log / cost_events / budget_incidents / decision_training_examples 等写入路径收敛到统一 trait，事务边界由仓储层保证。

#### Scenario: 记录 cost event
- **WHEN** 心跳完成一次 run 并产生 usage
- **THEN** 同一事务内写入 `cost_events` 与 `heartbeat_run_events`
