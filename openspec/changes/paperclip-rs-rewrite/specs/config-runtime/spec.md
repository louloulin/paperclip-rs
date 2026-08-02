## ADDED Requirements

### Requirement: 配置加载 + 嵌入式 PG 启动 SHALL
The system SHALL satisfy the following behavior.

`pc-config` 解析 `.env` 与环境变量；`pc-server` 启动时优先尝试嵌入式 PG，失败则回退外部 PG。

#### Scenario: 嵌入式 PG 不可用
- **WHEN** `pg-embedded` 在当前平台无预构建二进制
- **THEN** 启动时打印警告并使用 `PAPERCLIP_DATABASE_URL` 连接外部 PG
