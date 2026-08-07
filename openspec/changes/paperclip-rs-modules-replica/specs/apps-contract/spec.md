# apps-contract (M1)

物理上把 `pc-server` 与 `pc-cli` 二进制从 `crates/` 下独立到 `apps/` 下，与 PROJECT-PLAN.md 的工作区布局对齐。零行为改动，仅目录结构与 workspace 成员调整。

## ADDED Requirements

### Requirement: REQ-M1-1 Workspace Layout
`pc-server` 与 `pc-cli` 必须以独立 workspace 成员存在于 `apps/pc-server/` 与 `apps/pc-cli/` 目录下；不应继续出现在 `crates/` 下。


The system SHALL satisfy this requirement.
#### Scenario: 目录独立
- GIVEN 当前在 `crates/` 下存在 `pc-server` 与 `pc-cli`
- WHEN 完成 M1
- THEN `apps/pc-server/` 与 `apps/pc-cli/` 存在，源文件齐全；`crates/pc-server/` 与 `crates/pc-cli/` 不复存在

#### Scenario: workspace 编译通过
- GIVEN M1 完成
- WHEN `cargo build --workspace`
- THEN 编译成功，无 warning

### Requirement: REQ-M1-2 Help 输出兼容
`pc-server --help` 与 `pc-cli --help` 输出与改动前必须逐字一致。


The system SHALL satisfy this requirement.
#### Scenario: 帮助文本不变
- GIVEN 在改动前后分别捕获帮助输出
- WHEN 用 `diff` 对比
- THEN 0 diff
