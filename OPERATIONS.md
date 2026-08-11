# Paperclip-rs 运维手册（OPERATIONS.md）

> R583 / 2026-08-12
> 范围：生产部署 / 备份恢复 / 监控告警 / 故障排除 / 性能基线
> 配套：`ARCHITECTURE.md`（架构）/ `MODULE-MAPPING.md`（Node→Rust 映射）/ `README.md`（快速上手）

## 1. 生产部署

### 1.1 系统要求

| 项 | 最低 | 推荐 |
|---|---|---|
| OS | Linux (glibc 2.31+) / macOS 12+ | Ubuntu 22.04 LTS / macOS 14 |
| CPU | 4 cores | 8 cores |
| RAM | 8 GB | 16 GB |
| 磁盘 | 50 GB SSD | 200 GB NVMe |
| PostgreSQL | 14 | 16 |
| Rust 工具链 | 1.78+ | 1.95+ (stable) |

### 1.2 编译部署

```bash
# 1. 克隆
git clone https://github.com/your-org/paperclip-rs.git
cd paperclip-rs

# 2. 编译发布版本
cargo build --release -p pc-server -p pc-cli -p pc-migrate

# 3. 安装二进制
install -m 0755 target/release/paperclip-server /usr/local/bin/
install -m 0755 target/release/paperclipai   /usr/local/bin/
install -m 0755 target/release/paperclip-migrate /usr/local/bin/
```

### 1.3 systemd 单元

```ini
# /etc/systemd/system/paperclip-server.service
[Unit]
Description=Paperclip Rust Server
After=network.target postgresql.service
Requires=postgresql.service

[Service]
Type=notify
User=paperclip
Group=paperclip
EnvironmentFile=/etc/paperclip/server.env
ExecStart=/usr/local/bin/paperclip-server
Restart=on-failure
RestartSec=5s

# 资源限制
MemoryMax=4G
TasksMax=4096

# 安全加固
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/paperclip

[Install]
WantedBy=multi-user.target
```

```bash
systemctl daemon-reload
systemctl enable --now paperclip-server
systemctl status paperclip-server
```

### 1.4 环境变量

| 变量 | 必需 | 默认 | 说明 |
|---|---|---|---|
| `PAPERCLIP_DATABASE_URL` | ✅ | — | PostgreSQL 连接字符串 |
| `PAPERCLIP_PORT` | ❌ | `8080` | HTTP 监听端口 |
| `PAPERCLIP_BIND_ADDR` | ❌ | `0.0.0.0` | HTTP 监听地址 |
| `PAPERCLIP_RUN_MODE` | ❌ | `production` | `development` / `production` |
| `PAPERCLIP_LOG_LEVEL` | ❌ | `info` | `trace` / `debug` / `info` / `warn` / `error` |
| `PAPERCLIP_LOG_FORMAT` | ❌ | `json` | `json` (prod) / `pretty` (dev) |
| `PAPERCLIP_HOME` | ❌ | `~/.paperclip` | 数据根目录（备份、配置） |
| `PAPERCLIP_BOOTSTRAP_TOKEN` | ❌ | — | 首次启动管理员 token |
| `PAPERCLIP_DECISION_SIGNING_SECRET` | ❌ | auto | 决策签名密钥（自动生成） |
| `PAPERCLIP_OTLP_ENDPOINT` | ❌ | — | OpenTelemetry OTLP exporter URL |
| `RUST_LOG` | ❌ | — | 覆盖 tracing filter（高级） |

### 1.5 反向代理（Nginx）

```nginx
upstream paperclip {
    server 127.0.0.1:8080;
    keepalive 32;
}

server {
    listen 443 ssl http2;
    server_name paperclip.example.com;

    ssl_certificate     /etc/letsencrypt/live/paperclip.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/paperclip.example.com/privkey.pem;

    client_max_body_size 50M;

    location / {
        proxy_pass http://paperclip;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;

        # WebSocket
        proxy_read_timeout 86400s;
        proxy_send_timeout 86400s;
    }

    location /live-events {
        proxy_pass http://paperclip/live-events;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
    }
}
```

## 2. 数据库运维

### 2.1 Schema 迁移

```bash
# 查看迁移状态
paperclip-migrate status

# 应用迁移
paperclip-migrate up

# 回滚一个迁移
paperclip-migrate down

# 验证迁移一致性
paperclip-migrate verify

# 生成新的迁移（开发）
paperclip-migrate create --name add-foo-table
```

### 2.2 备份

```bash
# 完整备份（默认 pg_dump 格式）
paperclipai db-backup --output /var/backups/paperclip/$(date +%Y%m%d).dump

# 仅 schema
paperclipai db-backup --schema-only --output /var/backups/paperclip/schema.sql

# 压缩备份
paperclipai db-backup --compress gzip --output /var/backups/paperclip/daily.dump.gz
```

### 2.3 恢复

```bash
# 停止服务
systemctl stop paperclip-server

# 恢复（会覆盖现有数据）
pg_restore -h localhost -U paperclip -d paperclip /var/backups/paperclip/20260812.dump

# 启动服务
systemctl start paperclip-server
```

### 2.4 备份保留策略

| 类型 | 保留期 | 频率 |
|---|---|---|
| 每日完整备份 | 7 天 | 每日 02:00 |
| 每周完整备份 | 4 周 | 每周日 02:00 |
| 每月归档 | 12 月 | 每月 1 日 02:00 |
| WAL 归档 | 7 天 | 持续 |

### 2.5 真空与重建索引

```sql
-- 每月一次
VACUUM (ANALYZE, VERBOSE);
REINDEX DATABASE paperclip;
```

## 3. 监控告警

### 3.1 健康检查端点

| 端点 | 用途 |
|---|---|
| `GET /health` | 总体健康（200 = OK） |
| `GET /api/health` | 同上，namespace 化 |
| `GET /openapi.json` | OpenAPI schema（验证 server alive） |
| `GET /api/access` | 鉴权可达性 |

### 3.2 Prometheus 指标（OTLP）

通过 `PAPERCLIP_OTLP_ENDPOINT=http://otel-collector:4317` 启用。导出：

- `paperclip_http_requests_total{method,path,status}`
- `paperclip_http_request_duration_seconds{path}`
- `paperclip_db_pool_size`
- `paperclip_db_pool_available`
- `paperclip_heartbeat_runs_total{status}`
- `paperclip_active_runs{adapter}`
- `paperclip_live_events_subscribers`

### 3.3 关键告警

| 告警 | 阈值 | 严重度 |
|---|---|---|
| 服务不可达 | `/health` 连续 3 次失败 | P0 |
| 数据库连接耗尽 | pool available < 5% | P0 |
| DB 延迟 | p99 > 500ms 持续 5min | P1 |
| HTTP 错误率 | 5xx > 1% 持续 5min | P1 |
| 心跳积压 | active runs > 100 | P2 |
| 磁盘使用 | > 85% | P2 |
| 备份失败 | 连续 2 天失败 | P0 |

### 3.4 故障排查清单

```bash
# 1. 服务状态
systemctl status paperclip-server
journalctl -u paperclip-server --since "1 hour ago"

# 2. 健康检查
curl -v http://localhost:8080/health

# 3. 数据库连接
psql "$PAPERCLIP_DATABASE_URL" -c "SELECT 1;"

# 4. 当前活跃连接
psql "$PAPERCLIP_DATABASE_URL" -c \
  "SELECT count(*), state FROM pg_stat_activity GROUP BY state;"

# 5. 长事务
psql "$PAPERCLIP_DATABASE_URL" -c \
  "SELECT pid, now() - xact_start AS duration, query FROM pg_stat_activity \
   WHERE xact_start IS NOT NULL ORDER BY duration DESC LIMIT 10;"

# 6. 锁等待
psql "$PAPERCLIP_DATABASE_URL" -c \
  "SELECT blocked_locks.pid AS blocked_pid, blocking_locks.pid AS blocking_pid \
   FROM pg_catalog.pg_locks blocked_locks \
   JOIN pg_catalog.pg_locks blocking_locks ON blocking_locks.locktype = blocked_locks.locktype \
   WHERE NOT blocked_locks.granted;"

# 7. 表膨胀
psql "$PAPERCLIP_DATABASE_URL" -c \
  "SELECT schemaname, tablename, pg_size_pretty(pg_total_relation_size(schemaname || '.' || tablename)) \
   FROM pg_tables WHERE schemaname='public' ORDER BY pg_total_relation_size(schemaname || '.' || tablename) DESC LIMIT 10;"
```

## 4. 启动性能基线（R579 实测）

| 阶段 | cold | warm |
|---|---|---|
| db_connect | 7ms | 7ms |
| migrations | 868ms | 9ms |
| adapter_registration | 0ms | 0ms |
| heartbeat_recovery | 3ms | 3ms |
| bind | < 1ms | < 1ms |
| **总计** | **~880ms** | **~20ms** |

冷启动慢是 cargo 编译（30-60s），不是 server 本身慢。

## 5. 水平扩展

### 5.1 无状态 server

`paperclip-server` 是无状态的（除 PG 数据库外）。可水平扩展：

```bash
# 在多台机器上同时启动 server（共享同一 PG）
machine-a: paperclip-server  # bind :8080
machine-b: paperclip-server  # bind :8080
machine-c: paperclip-server  # bind :8080

# Nginx upstream load balancing
upstream paperclip {
    server 10.0.0.1:8080;
    server 10.0.0.2:8080;
    server 10.0.0.3:8080;
}
```

### 5.2 WebSocket 限制

WebSocket 订阅（`/live-events`）是有状态的。订阅者应使用 **sticky session**（基于 IP 哈希或 cookie）。

### 5.3 心跳调度

多副本部署时，每个 server 独立运行心跳 supervisor。需要：

- `paperclip-server --heartbeat-shard=X/N` 显式分片（推荐）
- 或接受少量重复唤醒（DB 唯一约束兜底）

## 6. 安全

### 6.1 关键配置

- `PAPERCLIP_BOOTSTRAP_TOKEN` 必须在首次启动后立即清除
- `PAPERCLIP_DECISION_SIGNING_SECRET` 必须持久化到 secret store（重启后保留）
- DB 用户不应是 superuser；最小权限：`CREATE/SELECT/INSERT/UPDATE/DELETE` + `USAGE` on sequences

### 6.2 防火墙

入站：

| 端口 | 来源 | 用途 |
|---|---|---|
| 443 / 80 | 外部 | HTTPS 入口 |
| 8080 | 仅 LB / 本机 | Rust server（不建议直接暴露） |

出站：

| 端口 | 目的地 | 用途 |
|---|---|---|
| 5432 | DB | PostgreSQL |
| 4317 (可选) | OTEL collector | OpenTelemetry |
| 443 | AI provider APIs | 适配器（claude / codex / openai） |

### 6.3 审计

所有写操作（issues, decisions, agent config 等）写入 `activity_log` 表。

```sql
SELECT actor_id, actor_kind, action, target_kind, target_id, created_at
FROM activity_log
WHERE created_at > now() - interval '1 hour'
ORDER BY created_at DESC;
```

## 7. 升级流程

```bash
# 1. 备份
paperclipai db-backup --output /var/backups/pre-upgrade-$(date +%Y%m%d).dump

# 2. 拉取新代码
cd /opt/paperclip-rs
git fetch && git checkout v0.2.0

# 3. 编译
cargo build --release -p pc-server -p pc-cli -p pc-migrate

# 4. 应用 schema 迁移
paperclip-migrate up

# 5. 重启服务（蓝绿或滚动）
systemctl restart paperclip-server

# 6. 验证
curl http://localhost:8080/health
```

### 7.1 回滚

```bash
# 1. 停止服务
systemctl stop paperclip-server

# 2. 恢复数据库
pg_restore --clean --if-exists -d paperclip /var/backups/pre-upgrade-XXX.dump

# 3. 切回旧代码
git checkout v0.1.5
cargo build --release -p pc-server

# 4. 启动
systemctl start paperclip-server
```

## 8. 常见问题

### Q1: `/health` 返回 503

通常是 DB 不可达。检查：
- `psql $PAPERCLIP_DATABASE_URL -c "SELECT 1;"`
- 网络：防火墙、PG `pg_hba.conf`
- 资源：DB server OOM、磁盘满

### Q2: 心跳不工作

- 检查 `heartbeat_recovery` 日志（`tracing::info!`）
- 检查 agent `status`（应是 `active`）
- 检查 adapter 注册（`adapter_registration` 阶段）

### Q3: WebSocket 频繁断连

- 反向代理超时设置（nginx `proxy_read_timeout 86400s;`）
- 客户端实现：用 `subscribe_with_resume` 补偿断连

### Q4: 启动慢

- 冷启动 30-60s = cargo 编译（首次或大改后）
- 实际 server 启动 < 100ms（R579 实测）
- 已编译后启动 = 立即

### Q5: 内存泄漏

- `MemoryMax=4G` 限制，触 OOM 即重启
- 排查：`/usr/local/bin/paperclip-server` 的 RSS 监控
- 长期：cargo flamegraph profiling

