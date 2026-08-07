# repos (M8)

DB schema + 25 个仓储子模块。

## ADDED Requirements

### Requirement: REQ-M8-1 Schema Migration
109 张表 DDL 全部存在于 `pc-db/migrations/`，可 `pc-migrate up` 0 错。


The system SHALL satisfy this requirement.
#### Scenario: fresh 迁移
- GIVEN fresh DB
- WHEN `pc-migrate up`
- THEN 109 张表齐

### Requirement: REQ-M8-2 25 Sub-Modules
`pc-repos` 必须有 25 个子模块，每个子模块一个源文件，按主题（company / agent / issue / case / project / approval / decision / routine / pipeline / environment / execution / heartbeat / plugin / auth / activity / document / goal / folder / sidebar / inbox / summary / tool / smoke / settings / skill）命名。


The system SHALL satisfy this requirement.
#### Scenario: 文件齐全
- GIVEN `crates/pc-repos/src/`
- WHEN `ls -1 *.rs`
- THEN 25+ 个分主题文件

### Requirement: REQ-M8-3 Typed IDs + Normalized Errors
每个子模块使用 newtype ID（`CompanyId(Uuid)`、`IssueId(i64)` 等）；错误归一 `RepoError → AppError::Repo`。


The system SHALL satisfy this requirement.
#### Scenario: ID 编译期安全
- GIVEN 用 `IssueId` 传给 `CompanyRepo::get`
- WHEN 编译
- THEN 类型不匹配错误

### Requirement: REQ-M8-4 Coverage Per Sub-module
每个子模块 ≥ 3 happy + ≥ 1 edge case 集成测试。


The system SHALL satisfy this requirement.
#### Scenario: 全覆盖
- GIVEN 25 子模块
- WHEN `cargo test -p pc-repos`
- THEN 所有 happy/edge 全过
