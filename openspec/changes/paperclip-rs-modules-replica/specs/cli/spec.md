# cli (M16)

`pc-cli` 全部子命令。

## ADDED Requirements

### Requirement: REQ-M16-1 19 sub-commands
`run / install / onboard / doctor / worktree / heartbeat-run / pipelines / routines / service / update / configure / db-backup / auth-bootstrap-ceo / allowed-hostname / env / env-lab / uninstall / worktree-lib` 等必须存在。


The system SHALL satisfy this requirement.
#### Scenario: sub-commands 列举
- GIVEN `pc-cli --help`
- WHEN 解析输出
- THEN 全部子命令展示

### Requirement: REQ-M16-2 --json 输出
任意 sub-command + `--json` 必须输出可解析 JSON。


The system SHALL satisfy this requirement.
#### Scenario: run --json
- GIVEN `pc-cli run --json`
- WHEN 解析输出
- THEN JSON 有效

### Requirement: REQ-M16-3 真跑
每个子命令在测试环境下真实跑一遍能正常退出。


The system SHALL satisfy this requirement.
#### Scenario: 全子命令跑通
- GIVEN 19 个 sub-commands
- WHEN 依次 `pc-cli <cmd> --help` + 真实 run
- THEN 全部 exit code 0
