# workflow (M15)

`pc-cron` + `pc-workflow` routines + pipelines + cron 调度。

## ADDED Requirements

### Requirement: REQ-M15-1 Cron 表达式
`pc-cron::next_after(cron, from)` 纯函数，与 croner 行为一致。


The system SHALL satisfy this requirement.
#### Scenario: 5 个标准 case
- GIVEN 5 个 croner-style cron
- WHEN 调 next_after
- THEN 与 Node `croner` 库同等输出

### Requirement: REQ-M15-2 Routine 调度
按 cron + tick 真实触发。


The system SHALL satisfy this requirement.
#### Scenario: 真实触发
- GIVEN routine "*/1 * * * *"
- WHEN 等 90s
- THEN activity_log 多出一条 routine run

### Requirement: REQ-M15-3 Pipeline DAG
pipeline 各步骤按依赖顺序执行，step 失败中断后续 step。


The system SHALL satisfy this requirement.
#### Scenario: pipeline 中断
- GIVEN 3-step pipeline，step-2 故意失败
- WHEN 触发
- THEN step-3 不被执行，run 标 failed
