## ADDED Requirements

### Requirement: CLI 子命令与原 paperclipai 一一对应 SHALL
The system SHALL satisfy the following behavior.

`pc-cli` 的 20+ 子命令（run/install/onboard/doctor/worktree/heartbeat-run/pipelines/routines/service/update/db backup/configure/...）与原 `cli/src/commands/*` 行为兼容。

#### Scenario: 列出命令
- **WHEN** `paperclipai --help`
- **THEN** 显示所有子命令与简短说明
