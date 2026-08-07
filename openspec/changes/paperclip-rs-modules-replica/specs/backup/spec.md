# backup (M5)

`pc-backup` pg_dump / pg_restore 一致性 + 备份清单 + retention。

## ADDED Requirements

### Requirement: REQ-M5-1 Backup 清单
备份文件必须含时间戳、SHA-256、size、schema_version。


The system SHALL satisfy this requirement.
#### Scenario: 清单齐全
- GIVEN 一次成功 dump
- WHEN 读取 manifest
- THEN 必含四个字段

### Requirement: REQ-M5-2 Dump
`pc-backup::dump(db_url, dir)` 必须真实调用 `pg_dump --format=custom`，不写入模拟数据。


The system SHALL satisfy this requirement.
#### Scenario: pg_dump 真实调用
- GIVEN 真实 PG 有数据
- WHEN dump
- THEN 生成 `*.dump` 文件，可用 `pg_restore --list` 解析

### Requirement: REQ-M5-3 Restore
`restore(backup, db_url)` 必须真实调用 `pg_restore --clean --if-exists`。


The system SHALL satisfy this requirement.
#### Scenario: row level 一致
- GIVEN 源库 1000 行
- WHEN dump → restore 到另一 DB
- THEN 目标库含 1000 行，checksum 全部匹配

### Requirement: REQ-M5-4 Retention
按 retention policy 自动清理过期备份。


The system SHALL satisfy this requirement.
#### Scenario: 过期清理
- GIVEN 30 天前备份 + 1 天前备份
- WHEN 跑 retention
- THEN 30 天前的被删
