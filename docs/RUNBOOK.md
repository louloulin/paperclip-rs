# Paperclip-rs RUNBOOK（运维脚本手册）

> 配套：`OPERATIONS.md`（运维总览）/ `ARCHITECTURE.md`（架构）/ `TROUBLESHOOTING.md`（故障排查）/ `FAQ.md`（常见问题）
> 适用范围：日常运维 / 值班响应 / 部署 / 备份恢复 / 升级

---

## 1. 启动 / 停止 / 重启

### 1.1 启动 pc-server

```bash
# 标准启动（开发模式）
cargo run --release -p pc-server

# 生产模式（systemd / supervisor）
./target/release/paperclip-server \
    --config /etc/paperclip/server.toml \
    --bind 0.0.0.0:3100

# 验证启动成功
curl -sf http://localhost:3100/health
# 期望：200 OK

# 启动 + 种子数据（e2e 测试用）
PAPERCLIP_SEED_DEMO=admin ./target/release/paperclip-server
```

启动序列日志检查点（按时间顺序）：
```
phase=config_load     elapsed_ms=5      ← 配置加载
phase=migrations      elapsed_ms=9      ← DB migrations
phase=seed            elapsed_ms=12     ← 仅 PAPERCLIP_SEED_DEMO 时出现
phase=bind            elapsed_ms=4      ← TCP bind
phase=server ready    elapsed_ms=80     ← 总耗时 < 200ms 目标
```

### 1.2 停止 pc-server

```bash
# 优雅停止（SIGTERM，等当前请求完成）
kill -TERM <pid>

# 强制停止（SIGKILL，立即终止）
kill -KILL <pid>
```

优雅停止会触发：
1. 拒绝新 HTTP 请求
2. 等所有 in-flight 请求 ≤ 30s
3. 心跳 supervisor 关闭
4. WebSocket close 帧
5. DB connection pool close
6. 进程退出码 0

### 1.3 重启 pc-server

```bash
# 滚动重启（zero-downtime，需 2 副本 + load balancer）
1. 从 load balancer 摘除 instance A
2. SIGTERM to instance A
3. 启动新版本 instance A
4. 等 health check OK
5. 加回 load balancer

# 普通重启（短 downtime）
systemctl restart paperclip-server  # systemd
supervisorctl restart paperclip    # supervisor
```

---

## 2. 备份与恢复

### 2.1 数据库备份

```bash
# 全量备份（推荐每日 1 次）
pg_dump -h $DB_HOST -U paperclip -d paperclip \
    --format=custom --compress=9 \
    --file=/backup/paperclip-$(date +%Y%m%d-%H%M%S).dump

# 验证备份完整性
pg_restore --list /backup/paperclip-20260820-120000.dump | head

# 仅 schema（不含数据）
pg_dump --schema-only -d paperclip > schema.sql
```

### 2.2 自动备份（systemd timer）

```ini
# /etc/systemd/system/paperclip-backup.timer
[Unit]
Description=Daily paperclip backup

[Timer]
OnCalendar=daily
OnCalendar=02:00
Persistent=true

[Install]
WantedBy=timers.target
```

### 2.3 恢复数据库

```bash
# 停止 server（避免写入冲突）
systemctl stop paperclip-server

# 恢复（覆盖现有 DB）
pg_restore -h $DB_HOST -U paperclip -d paperclip \
    --clean --if-exists \
    /backup/paperclip-20260820-120000.dump

# 启动 server（自动跑 migrations 验证 schema）
systemctl start paperclip-server

# 验证
curl -sf http://localhost:3100/health
psql -c "SELECT COUNT(*) FROM companies;"
```

### 2.4 文件资源备份

```bash
# paperclip home 目录（包含 adapters 配置、telemetry state）
tar czf /backup/paperclip-home-$(date +%Y%m%d).tgz \
    /var/lib/paperclip/ \
    --exclude='*.log' \
    --exclude='cache/*'

# 备份保留策略
# - 全量 DB: 30 天
# - 增量 WAL: 7 天
# - paperclip-home: 30 天
```

---

## 3. 升级

### 3.1 升级前检查

```bash
# 1. 查看 migrations 数量（升级会增加 migrations）
git pull origin main
cargo build --release -p pc-migrate
./target/release/paperclip-migrate list --pending

# 2. 备份 DB（升级前必做）
pg_dump ... --file=/backup/pre-upgrade-$(date +%Y%m%d).dump

# 3. 检查 breaking changes
cat CHANGELOG.md | grep -A 5 'BREAKING'
```

### 3.2 升级步骤（蓝绿部署）

```bash
# 1. 部署新版本到"绿"实例
./target/release/paperclip-server --bind :3200 &
GREEN_PID=$!

# 2. 等 green 健康
for i in {1..30}; do
    curl -sf http://localhost:3200/health && break
    sleep 1
done

# 3. 切流量
# - 更新 nginx upstream 把 :3100 替换为 :3200
# - nginx -s reload

# 4. 观察 5 分钟（看 error rate / latency）

# 5. 关闭旧实例
kill -TERM $GREEN_PID  # 实际是切完流量后的旧 :3100 pid
```

### 3.3 升级失败回滚

```bash
# 1. 切回旧实例（反向 nginx reload）
# 2. 回滚 DB（用 pre-upgrade backup）
pg_restore -d paperclip --clean --if-exists /backup/pre-upgrade-*.dump
# 3. 启动旧版本 binary
```

---

## 4. 监控关键指标

### 4.1 健康检查

```bash
# Liveness（容器是否活着）
curl -fsS http://localhost:3100/health

# Readiness（能服务请求）
curl -fsS http://localhost:3100/health/ready

# Startup probe
curl -fsS http://localhost:3100/health/startup
```

### 4.2 关键 Prometheus metrics

| Metric | 说明 | 告警阈值 |
|---|---|---|
| `paperclip_http_request_duration_seconds` | HTTP 请求 P99 latency | > 1s |
| `paperclip_db_pool_size` | DB 连接池使用 | > 80% |
| `paperclip_heartbeat_runs_pending` | 待处理 heartbeat run | > 100 |
| `paperclip_ws_connections_active` | 活跃 WS 连接 | > 10000 |
| `paperclip_adapter_failures_total` | Adapter 失败累计 | > 10/min |
| `paperclip_db_migrations_pending` | 待应用 migrations | > 0 |

### 4.3 日志关键字告警（loki / ELK）

```
ERROR   → 立刻告警
WARN    → 1 分钟内 ≥10 次告警
adapter timeout  → adapter 失败
migration failed → 启动失败
out of memory → 立刻告警 + dump heap
```

---

## 5. 性能调优

### 5.1 DB 连接池

```toml
# server.toml
[database]
max_connections = 32        # 默认 16，按 (CPU*2 + disk_count) 调整
min_connections = 4
acquire_timeout_seconds = 10
```

### 5.2 心跳调度

```toml
[heartbeat]
tick_interval_seconds = 2   # 默认 2s，按 CPU 调整
batch_size = 50             # 默认 50，按 issue 数量调整
```

### 5.3 WebSocket

```toml
[realtime]
broadcast_buffer_size = 1024
client_lag_tolerance = 50
heartbeat_interval_seconds = 30
```

---

## 6. 安全

### 6.1 密钥轮换

```bash
# 1. 生成新 decision signing secret
cargo rand -p pc-secrets

# 2. 更新 config
echo "PAPERCLIP_DECISION_SIGNING_SECRET=<new>" >> /etc/paperclip/server.env

# 3. 重启 server（graceful）
systemctl reload paperclip-server

# 4. 验证：旧 secret 签名的 decision 仍可 verify（向后兼容窗口）
```

### 6.2 Rate Limiting

```toml
[auth]
login_attempts_per_minute = 10
session_max_concurrent_per_user = 5
```

### 6.3 审计日志

```bash
# 查看用户操作
psql -c "SELECT * FROM audit_log WHERE user_id = '$UID' ORDER BY created_at DESC LIMIT 50;"

# 查看所有 admin 操作
psql -c "SELECT * FROM audit_log WHERE action LIKE 'admin.%' ORDER BY created_at DESC LIMIT 50;"
```

---

## 7. 紧急操作

### 7.1 紧急关闭写操作（read-only 模式）

```bash
# 紧急：server 收到信号 SIGUSR1 时切到只读
kill -USR1 <pid>
# 所有 mutation endpoint 返回 503
# DB connection pool 切换到 read replica
```

### 7.2 紧急清理孤儿 runs

```bash
# 找出超过 1 小时还在 running 的 runs
psql -c "SELECT id, started_at FROM runs WHERE status = 'running' AND started_at < now() - interval '1 hour';"

# 强制标 failed
psql -c "UPDATE runs SET status = 'failed', completed_at = now(), failure_reason = 'manual cleanup' WHERE id IN (...);"
```

### 7.3 紧急禁用 adapter

```bash
# 临时禁用某个 adapter（不改 DB）
echo '{"disabled_adapters":["codex-local"]}' > /etc/paperclip/adapter-override.json
systemctl reload paperclip-server
```