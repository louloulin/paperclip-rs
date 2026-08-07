# migrate (M6)

`pc-migrate` 独立迁移 CLI。

## ADDED Requirements

### Requirement: REQ-M6-1 Sub-commands
必须存在 `up` / `down` / `status` / `create` / `verify` / `baseline` / `seed` 七个 sub-command。


The system SHALL satisfy this requirement.
#### Scenario: sub-command 列举
- GIVEN `pc-migrate --help`
- WHEN 解析输出
- THEN 含全部七个

### Requirement: REQ-M6-2 JSON 输出
任意 sub-command + `--json` 必须输出可解析 JSON。


The system SHALL satisfy this requirement.
#### Scenario: status --json
- GIVEN DB schema 应用一半
- WHEN `pc-migrate status --json`
- THEN 输出 applied/pending 数组

### Requirement: REQ-M6-3 Safety
`up` 必须有 lock file；并发跑第二个进程应等待或拒绝。


The system SHALL satisfy this requirement.
#### Scenario: 并发跑
- GIVEN 第一个 up 在运行
- WHEN 第二个 up 启动
- THEN 等待或拒绝（明确错误）

### Requirement: REQ-M6-4 Fresh DB up
现有 109 表 SQL 在 fresh PG 上 `pc-migrate up` 0 错误。


The system SHALL satisfy this requirement.
#### Scenario: fresh up
- GIVEN fresh DB
- WHEN `pc-migrate up`
- THEN 109 张表齐
