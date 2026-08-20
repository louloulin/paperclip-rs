# Paperclip-rs TROUBLESHOOTING（故障排查手册）

> 配套：`RUNBOOK.md`（运维脚本）/ `FAQ.md`（常见问题）/ `OPERATIONS.md`（运维总览）
> 范围：按症状分类的故障排查指南（启动失败 / 性能下降 / DB 错误 / adapter 失败 / WS 断连等）

---

## 1. 启动失败

### 1.1 `pc-server: database connection refused`

**症状**：`tracing::error! "db connect failed"` + 进程退出码 1

**根因排查**：
```bash
# 1. 检查 DB 是否可达
pg_isready -h $DB_HOST -p 5432

# 2. 检查 DATABASE_URL 格式
echo "$PAPERCLIP_DATABASE_URL"
# 期望：postgres://user:pass@host:5432/dbname

# 3. 检查 DB 凭证
psql "$PAPERCLIP_DATABASE_URL" -c "SELECT 1"

# 4. 检查 server 端能否解析 DB 域名
nslookup $DB_HOST
```

**修复**：
- DB 未启动 → `systemctl start postgresql`
- 防火墙阻止 5432 → 开放 security group
- 凭证错误 → 更新 env，重启 server

### 1.2 `migrations failed: relation already exists`

**症状**：migration 应用时冲突

**根因**：DB schema 不一致（部分 migrations 之前手动跑过）

**修复**：
```bash
# 查看具体冲突的 migration
./target/release/paperclip-migrate list

# 选项 1：标记已应用（如果表已存在）
./target/release/paperclip-migrate mark-applied <migration_id>

# 选项 2：重置 DB（生产前必备份）
pg_dump ... --file=/backup/$(date +%s).dump
./target/release/paperclip-migrate reset
```

### 1.3 `port 3100 already in use`

**症状**：bind 阶段 EADDRINUSE

**修复**：
```bash
# 找出占用进程
lsof -i :3100

# 选项 1：kill 旧进程
kill -TERM <pid>

# 选项 2：换端口（紧急）
./paperclip-server --bind :3200
# 然后更新 nginx upstream
```

### 1.4 `adapter xxx failed to register`

**症状**：adapter 注册阶段 panic

**根因**：adapter binary 不存在 / 权限不够 / PATH 错

**修复**：
```bash
# 检查 adapter binary
ls -la /usr/local/bin/claude-local
which claude-local  # 期望非空

# 检查权限
chmod +x /usr/local/bin/claude-local

# 临时禁用
export PAPERCLIP_DISABLED_ADAPTERS=claude-local,codex-local
./paperclip-server
```

---

## 2. 性能问题

### 2.1 HTTP 请求 P99 > 1s

**排查步骤**：
```bash
# 1. 看慢查询日志
tail -f /var/log/paperclip/slow-queries.log

# 2. 看 DB 负载
psql -c "SELECT pid, query, state, age(now(), query_start) FROM pg_stat_activity ORDER BY age DESC LIMIT 20;"

# 3. 看 ws 连接数 / heartbeats
curl -s http://localhost:3100/metrics | grep -E 'ws_conn|heartbeat'

# 4. 看 application log 的 WARN 关键字
grep -E "WARN|timeout|slow" /var/log/paperclip/app.log | tail -50
```

**常见修复**：
- 加 DB 索引：`CREATE INDEX CONCURRENTLY idx_xxx ON ...;`
- 调高 connection pool：`max_connections = 64`
- 启用 query cache：`pc-http::cache::enable()`
- 水平扩展：增加 pc-server 副本

### 2.2 内存持续上涨（疑似 leak）

**排查**：
```bash
# 1. 启用 jemalloc profiling（如果用 jemalloc）
export MALLOC_CONF="prof:true,prof_prefix:jeprof"

# 2. dump heap
kill -USR2 <pid>  # paperclip-rs 自定义 heap dump 信号

# 3. 对比 baseline
jeprof --base=/tmp/jeprof.base.<pid>.<n1>.heap /tmp/jeprof.<pid>.<n2>.heap
```

**常见根因**：
- DB 连接泄漏（pool 未释放）→ 检查所有 `db.query_*` 后的 `?` 传播
- HashMap 不限增长（无 TTL）→ 加 LRU eviction
- Stream buffer 泄漏 → 检查所有 `mpsc::Sender` drop 路径

### 2.3 心跳调度器堆积

**症状**：`paperclip_heartbeat_runs_pending` 持续增长

**排查**：
```sql
-- 看 runs 状态分布
SELECT status, COUNT(*) FROM runs GROUP BY status;

-- 看最老的 pending run
SELECT id, scheduled_at, age(now(), scheduled_at) FROM runs
WHERE status = 'pending'
ORDER BY scheduled_at LIMIT 10;
```

**修复**：
```toml
# 调高 batch size
[heartbeat]
batch_size = 200
tick_interval_seconds = 1
```

---

## 3. DB 错误

### 3.1 `connection pool timeout`

**症状**：`sqlx::Error::PoolTimedOut`

**根因**：DB 连接用尽

**排查**：
```sql
SELECT count(*) FROM pg_stat_activity WHERE datname = 'paperclip';
SELECT pid, state, query FROM pg_stat_activity WHERE state = 'idle in transaction';
```

**修复**：
- 调高 `max_connections`
- 杀掉 idle in transaction 长事务
- 应用层加 query timeout

### 3.2 `deadlock detected`

**症状**：偶发 deadlock error

**排查**：
```bash
# 看 deadlock 日志（PG 默认输出到 stderr / log）
grep "deadlock detected" /var/log/postgresql/*.log
```

**修复**：
- 短事务：避免长持有 row lock
- 一致顺序：跨多个 update 时按固定顺序加锁
- 用 `SELECT ... FOR UPDATE SKIP LOCKED` 替代顺序锁

### 3.3 慢查询

**排查**：
```sql
-- 启用 pg_stat_statements（需要 superuser）
CREATE EXTENSION pg_stat_statements;

-- 看 top 10 慢查询
SELECT query, calls, mean_exec_time, total_exec_time
FROM pg_stat_statements
ORDER BY mean_exec_time DESC LIMIT 10;
```

---

## 4. Adapter 失败

### 4.1 `adapter xxx timeout`

**症状**：runs 状态 = failed，error message 含 timeout

**排查**：
```bash
# 1. 看 adapter 子进程日志
journalctl -u paperclip-adapter-claude-local

# 2. 直接测试 adapter
echo '{"action":"ping"}' | nc -U /tmp/paperclip-claude.sock
```

**修复**：
- 调高 adapter timeout：`PAPERCLIP_ADAPTER_TIMEOUT_SECONDS=300`
- 重启 adapter 进程：`systemctl restart paperclip-adapter-claude-local`
- 升级 adapter 版本

### 4.2 `adapter rate limit`

**症状**：HTTP 429 from adapter upstream

**修复**：
- 调低 pc-server 派发频率
- 实现 exponential backoff
- 申请更高 rate limit

---

## 5. WebSocket 断连

### 5.1 客户端频繁断连

**排查**：
```bash
# 1. 看 WS metrics
curl -s http://localhost:3100/metrics | grep ws_

# 2. 看 nginx 配置（如果有 reverse proxy）
grep -E "proxy_read_timeout|proxy_send_timeout" /etc/nginx/sites-enabled/paperclip
```

**修复**：
- nginx `proxy_read_timeout 3600s;`
- nginx `proxy_send_timeout 3600s;`
- 检查 LB（AWS ALB 默认 60s idle timeout）

### 5.2 Live events 不推送

**症状**：UI 操作后其他用户看不到实时更新

**排查**：
```bash
# 1. 检查 broadcast channel
curl -s http://localhost:3100/metrics | grep broadcast

# 2. 手动订阅 WS
websocat ws://localhost:3100/api/companies/xxx/events/ws

# 3. 在另一终端操作，看是否收到事件
```

**修复**：
- 检查 `pc-realtime` 是否在跑
- 检查 event listener 是否注册
- 重启 pc-server

---

## 6. 内存 / CPU 异常

### 6.1 CPU 100%

**排查**：
```bash
# 1. 看进程状态
top -p <pid>
# 注意 %CPU / RES / SHR 列

# 2. 看哪个线程 busy
perf top -p <pid>

# 3. 看 tokio runtime 状态
kill -USR1 <pid>  # paperclip-rs dump runtime
```

**常见根因**：
- 死循环 bug
- tokio::spawn_blocking 过多
- tight loop in cron / heartbeat

### 6.2 磁盘满

**排查**：
```bash
df -h
du -sh /var/lib/paperclip/*  # 找最大占用
du -sh /tmp/* | sort -h
```

**修复**：
- 清理过期 telemetry state：`rm /var/lib/paperclip/telemetry/*.jsonl`
- 清理 log：`journalctl --vacuum-time=7d`
- 扩容

---

## 7. 安全事件

### 7.1 检测到异常登录

**排查**：
```sql
-- 查最近失败的登录
SELECT * FROM auth_events WHERE event = 'login_failed' ORDER BY created_at DESC LIMIT 50;
```

**响应**：
- 封禁 IP（nginx deny）
- 强制用户重置密码
- 通知安全团队

### 7.2 怀疑泄漏 signing secret

**响应**（参考 RUNBOOK §6.1）：
- 立即轮换 secret
- 检查所有历史 decision 是否被伪造
- 通知安全团队

---

## 8. 备份恢复失败

### 8.1 `pg_restore: could not execute`

**症状**：backup dump 损坏

**排查**：
```bash
# 验证 dump 完整性
pg_restore --list /backup/*.dump 2>&1 | head

# 看具体错误
pg_restore -d postgres --clean --if-exists /backup/*.dump 2>&1 | grep -i error
```

**修复**：
- 用昨天的备份
- 启用 WAL archival（point-in-time recovery）

---

## 9. 升级失败回滚

参考 RUNBOOK §3.3 标准流程。

---

## 10. 通用排查清单

任何问题，先回答以下问题：

1. **何时发生**：刚启动 / 运行中 / 部署后 / 高峰期？
2. **影响范围**：所有用户 / 部分用户 / 某个 endpoint？
3. **最近变更**：是否刚升级 / 改配置 / 部署代码？
4. **资源状态**：CPU / 内存 / 磁盘 / DB 连接？
5. **日志关键字**：ERROR / WARN / timeout / panic / OOM？
6. **可重现**：必现 / 偶发 / 特定条件下？

按这6 个维度系统性排查，避免漏掉关键信息。