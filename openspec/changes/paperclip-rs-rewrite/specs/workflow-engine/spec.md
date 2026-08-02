## ADDED Requirements

### Requirement: routines 与 pipelines 执行引擎 SHALL
The system SHALL satisfy the following behavior.

`pc-workflow` 负责 routines（周期任务）与 pipelines（流程）的定义、调度、执行；调度器基于 `tokio-cron-scheduler`。

#### Scenario: routine 触发
- **WHEN** cron 表达式命中
- **THEN** 创建 `routine_run` 并执行
