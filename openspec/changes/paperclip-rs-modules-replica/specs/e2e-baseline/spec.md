# e2e-baseline (M2)

提供一个可重复运行的端到端基线脚本，后续每个模块完成后用这个脚本做回归验证。

## ADDED Requirements

### Requirement: REQ-M2-1 Health Check
`pc-server` 启动后 `GET /health` 必须在 5 秒内返回 HTTP 200。


The system SHALL satisfy this requirement.
#### Scenario: 启动后 health 通过
- GIVEN 外部 PostgreSQL 已就绪
- WHEN `pc-server` 监听 3100
- THEN `curl -fsS :3100/health` 在 5s 内返回 200

### Requirement: REQ-M2-2 Auto Migrate
`pc-server` 启动时应自动调用 `pc-migrate up`，使 109 张表 schema 在 fresh DB 上对齐到当前 `pc-db::migrations/`。


The system SHALL satisfy this requirement.
#### Scenario: 启动即迁移
- GIVEN 干净 PG（无 schema）
- WHEN `pc-server` 启动
- THEN 自动完成 migrate，DB 中含 109 张期望表

### Requirement: REQ-M2-3 Baseline Script
`scripts/e2e-baseline.sh` 必须可在干净外部 PG 上 0 错误跑通：起 PG → migrate → server → curl → shutdown。


The system SHALL satisfy this requirement.
#### Scenario: 双平台脚本通过
- GIVEN 干净 macOS + Linux（glibc 与 musl）
- WHEN `bash scripts/e2e-baseline.sh`
- THEN 两个平台均 exit code 0
