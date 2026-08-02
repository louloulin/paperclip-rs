## ADDED Requirements

### Requirement: 109 张 PostgreSQL 表与原 schema 字节级一致 SHALL
The system SHALL satisfy the following behavior.

`pc-db` 的迁移文件以 SQL DDL 形式定义 109 张表，结构（列、类型、约束、索引、外键）与 `paperclip/packages/db/src/schema/*.ts` 推导出的 DDL 完全一致；可在原数据上直接 `pc-migrate up` 无 DDL 漂移。

#### Scenario: fresh DB 迁移至最新
- **WHEN** 在空库上跑 `paperclip-migrate up`
- **THEN** `pg_dump --schema-only` 与原 Drizzle 输出一致

### Requirement: 迁移版本管理与可回滚 SHALL
The system SHALL satisfy the following behavior.

迁移以 `NNNN_name.sql` 命名，记录在 `_pc_migrations` 表；支持 `up`/`down`/`status`。

#### Scenario: 查看迁移状态
- **WHEN** `paperclip-migrate status`
- **THEN** 输出每个迁移的 applied/pending 状态
