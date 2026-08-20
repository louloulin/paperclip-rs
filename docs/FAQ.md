# Paperclip-rs FAQ（常见问题）

> 配套：`RUNBOOK.md` / `TROUBLESHOOTING.md` / `OPERATIONS.md` / `ARCHITECTURE.md`
> 范围：常见问题 Q&A，覆盖部署 / 开发 / 性能 / 安全 / 兼容性

---

## 部署与运维

### Q: 第一次部署 pc-server 需要哪些步骤？

A: 最小可行步骤：
1. 安装 PostgreSQL 14+ 并创建 `paperclip` 数据库 + 用户
2. 设置 `PAPERCLIP_DATABASE_URL` env var
3. 设置 `PAPERCLIP_DECISION_SIGNING_SECRET`（至少 32 字节 base64）
4. `cargo build --release -p pc-server`
5. `./target/release/paperclip-server`（自动跑 migrations）
6. `curl http://localhost:3100/health` 验证

完整步骤参见 `RUNBOOK.md §1`。

### Q: 能否用 SQLite 替代 PostgreSQL？

A: **不能**。paperclip-rs 依赖 PostgreSQL 特性（`SELECT FOR UPDATE SKIP LOCKED`、JSONB、partial index 等），SQLite 不支持。生产部署必须用 PostgreSQL 14+。

### Q: 能否用 Docker Compose 部署？

A: 可以。最小 compose：
```yaml
services:
  db:
    image: postgres:16
    environment:
      POSTGRES_DB: paperclip
      POSTGRES_USER: paperclip
      POSTGRES_PASSWORD: ${DB_PASSWORD}
    volumes:
      - pgdata:/var/lib/postgresql/data
  server:
    image: your-registry/paperclip-server:latest
    environment:
      PAPERCLIP_DATABASE_URL: postgres://paperclip:${DB_PASSWORD}@db:5432/paperclip
      PAPERCLIP_DECISION_SIGNING_SECRET: ${SIGNING_SECRET}
    ports:
      - "3100:3100"
    depends_on:
      - db
volumes:
  pgdata:
```

### Q: 内存 / CPU 推荐配置？

A: 见 `RUNBOOK.md §1.1` 系统要求表。粗略：
- < 100 用户 / < 1000 issues / 天：4 cores / 8 GB
- 100-1000 用户 / 1000-10000 issues / 天：8 cores / 16 GB
- > 1000 用户：水平扩展多个 pc-server 副本 + load balancer

---

## 开发

### Q: 如何在本地起一个 demo 环境？

A: 用 seed_demo 功能：
```bash
PAPERCLIP_SEED_DEMO=admin cargo run -p pc-server
```
会自动创建 demo company + admin + 5 agents + 10 issues + 2 pipelines + 2 projects。Idempotent（重复启动复用）。

### Q: 怎么加一个新的 pure helper？

A: 流程：
1. 找到对应 Node 上游函数（`server/src/services/<name>.ts` 或 `packages/shared/src/<name>.ts`）
2. 在 Rust crate 的 `pure.rs`（或新建）加 `pub fn`
3. 写单元测试（覆盖 happy + 边界 + 反例）
4. `cargo test -p <crate> --lib <name>::pure` 验证
5. service 层（`service.rs`）调用 pure helper + 错误处理
6. HTTP route handler 调用 service
7. 全 workspace 测试 `cargo test --workspace --lib`

### Q: 如何测试 pc-http route？

A: 三种方式：
1. **单元测试**（推荐）：`pc-http::routes::<route>::tests`
2. **集成测试**：`tests/` 目录，用 `axum::Router` + `tower::ServiceExt::oneshot`
3. **e2e**：启动 server + curl / Playwright（参考 `ui-workflow-validation` change）

### Q: pc-server 启动太慢，怎么办？

A: 检查 6 个启动阶段耗时（启动时 tracing 日志）：
```
phase=config_load    elapsed_ms
phase=migrations     elapsed_ms
phase=seed           elapsed_ms
phase=bind           elapsed_ms
phase=server ready   elapsed_ms   ← 总耗时
```
- `config_load > 50ms`：env var 太多 / TOML 解析慢
- `migrations > 100ms`：DB schema 漂移，需 `pc-migrate reset`
- `bind > 50ms`：检查 socket backlog（`net.core.somaxconn`）

目标：warm start < 200ms。

---

## 性能

### Q: P99 latency 高，怎么排查？

A: 见 `TROUBLESHOOTING.md §2.1`。简版：
1. 看慢查询日志 → 加索引
2. 看 DB 负载 → 扩容 / 调连接池
3. 看 application WARN → 检查 timeout 配置

### Q: 内存持续上涨，是 leak 吗？

A: 见 `TROUBLESHOOTING.md §2.2`。快速定位：
- `kill -USR2 <pid>` dump heap
- 对比多个时间点的 heap snapshot
- 用 jemalloc profiler 分析

### Q: pc-server 能水平扩展吗？

A: **可以**。架构是 stateless：
- DB：PostgreSQL 集中存储
- WS broadcast：`pc-realtime` 用 broadcast channel（单节点内），多节点需要 Redis pubsub（V13+）
- Heartbeat：多节点会重复调度，需要 leader election 或 sharding

目前推荐：单节点 + 垂直扩展。多节点需要等 Redis pubsub 实现。

### Q: 5 分钟长跑脚本在哪？

A: `scripts/long-run-5min.sh`（V13 phase）。需要 wrk + 真实环境。本地开发可省略。

---

## 安全

### Q: 如何轮换 decision signing secret？

A: 见 `RUNBOOK.md §6.1`。要点：
1. 生成新 secret
2. 支持双 secret 并存（验证旧 + 签发新）
3. 24-48 小时后下架旧 secret
4. 监控 verification 失败率

### Q: RBAC 怎么配？

A: 通过 `pc-authz` 配置 principal permissions：
- `admin`: 全部权限
- `board`: company-scoped admin
- `member`: read + 自己的资源 write
- `agent`: agent 自己的资源 + 受托操作

具体配置参考 `crates/pc-authz/src/lib.rs`。

### Q: API rate limit？

A: 在 nginx / load balancer 层配置。pc-server 默认无限流（信任前置 LB）。如需内置限流，配置 `crates/pc-http/src/middleware/rate_limit.rs`。

---

## 兼容性

### Q: Rust 版本要求？

A: `rust-toolchain.toml` 指定 stable 1.78+。推荐 1.95+。

### Q: 能否跟 Node 上游 paperclip 共存？

A: **可以**（HTTP/WS 契约冻结）：
- pc-rs 与 Node server 共享同一 PostgreSQL schema
- 同一时刻只能运行一个 server（防止 WS 重复广播）
- 蓝绿切换：先停 Node，启动 pc-rs（DB schema 兼容）

### Q: schema migration 是 additive-only 吗？

A: 大部分是。但偶有破坏性变更（如新增 NOT NULL 列）会跟随 PCNode-rs 一次性迁移。详见 `pc-migrate/CONTRIBUTING.md`。

### Q: 旧 client（paperclip-cn/ui）能用 pc-rs 后端吗？

A: **能**。前端契约冻结，HTTP/WS API 完全兼容。配置 paperclip-cn 的 API endpoint 指向 pc-rs server URL 即可。

---

## 错误信息

### Q: `{"error":"Authentication required"}`

A: session cookie 缺失 / 过期。重新登录或检查 session middleware。

### Q: `{"error":"Permission denied"}`

A: actor 没权限。检查 `pc-authz::PrincipalPermissionGrant`。

### Q: `{"error":"Conflict: resource already exists"}`

A: UNIQUE 约束冲突。例如创建同名 company。改名或用 update。

### Q: `{"error":"Migration failed: column X cannot be null"}`

A: DB schema 漂移。运行：
```bash
./target/release/paperclip-migrate verify
./target/release/paperclip-migrate repair
```

### Q: `WebSocket closed: code 1006`

A: 网络中断或 server 崩溃。检查 nginx / LB 是否 idle timeout 杀掉连接（设置 > 1h）。

---

## 故障排查总入口

见 `TROUBLESHOOTING.md` —— 按症状分类（启动失败 / 性能 / DB / adapter / WS / 内存 / 安全 / 备份 / 升级）。

---

## 反馈与贡献

- Bug report：GitHub Issues
- 文档改进：欢迎 PR
- 安全漏洞：security@yourdomain.com（不要公开提）
- 商业支持：sales@yourdomain.com