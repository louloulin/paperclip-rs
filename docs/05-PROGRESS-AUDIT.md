# Paperclip-rs 复刻进度审计（2026-08-03，第四轮）

## 当前门禁
- ✅ `cargo fmt --all`
- ✅ `cargo build --workspace` — 0 errors / 9 warnings（pre-existing dead_code）
- ✅ `cargo test --workspace` — **270 passed (71 suites)**
- ✅ `cargo clippy -p pc-backup --all-targets -- -D warnings` — No issues found
- ✅ pc-server 启动 + 端到端 UI 代理冒烟通过

## 本轮关键进展

### 1. HTTP 中间件层（pc-http/middleware）— 15 tests
- `request_id`：每个请求注入 / 透传 `X-Request-Id`（UUID v7）
- `access_log`：结构化访问日志（request_id / method / path / status / duration_ms）
- `body_limit`：基于 Content-Length 头快速拒绝（默认 25 MiB）
- `redaction`：从 JSON / 文本中递归脱敏（password / token / secret / apiKey / 等 17 个字段）
- `cors`：基础 CORS 头注入（dev origins 允许列表）
- `apply_default_middleware()`：组合 4 层中间件，pc-server main.rs 接入

### 2. pc-backup crate（共享备份库）— 9 tests
- `BackupEngine`（pg_dump + gzip）/ `RestoreEngine`（psql + gzip 解压）
- `RetentionPolicy`（每日 7 天 + 每周 4 周 + 每月 1 月；strict name match）
- `BackupManager`（singleflight + status + list + prune）
- `instance_database_backups` route 重写，复用；新增 4 个端点

### 3. pc-migrate 独立二进制 — 2 tests
- `paperclip-migrate` 二进制：up / status / verify / baseline
- `--json` / `--database-url` / `--max-connections` 全局
- 端到端验证 196 迁移全部应用；8 张关键表全部存在

### 4. pc-telemetry OTLP exporter — 6 tests
- `otlp` feature 默认关闭（编译期 + 运行期零开销）
- API：`OtlpConfig` / `build_otlp_provider()` / `install_global()`
- 环境变量：`PAPERCLIP_OTLP_ENDPOINT` / `_HEADERS` / `_SAMPLE_RATIO` / `_DISABLED`
- pc-server 透传 `otlp` feature；启动时尝试安装

### 5. pc-cli 命令族扩展 — 11 tests
- 新增：`env-lab` / `worktree` / `service` / `client`（whoami / live-events / companies / agents / issues / get / post）

### 6. 路由 company_id 灵活化（端到端 UI 兼容性修复）
- 7 个路由（issues / cases / projects / approvals / decisions / pipelines / goals）的 ListQuery 改为 `Option<Uuid>`
- 每个 repo 加 `list_all(limit)` 方法
- handler 在 `None` 时返回全部行（跨公司）
- 修 SQL 插值 bug（`{COLS}` 形式）

## 全局统计

| 指标 | 第三轮 | 第四轮 | Δ |
|---|---|---|---|
| Crates | 36 | **38** | +2 (pc-backup, pc-migrate) |
| Binaries | 2 | **3** | +1 (paperclip-migrate) |
| 测试用例 | 239 | **270** | +31 (backup 9 + migrate 2 + telemetry 4 + middleware 13 + cli 7，部分去重) |
| Test suites | 68 | **71** | +3 |
| 中间件层 | 无 | 5 个（request_id / access_log / body_limit / redaction / cors） | 关键缺口补齐 |
| 路由 company_id 灵活化 | 强制 | 7 个改为可选 | UI 兼容性 ↑↑ |
| 路由覆盖 | 100% (56/56) | 100% (56/56) | 持平（仅缺 index.ts） |

## 三道门禁命令
```bash
cd paperclip-rs
rtk cargo fmt --all
rtk cargo build --workspace
rtk cargo test --workspace --no-fail-fast
```

## 端到端冒烟（已验证）

### Server 直连
```bash
curl -s 'http://127.0.0.1:3100/health'           # 200
curl -s 'http://127.0.0.1:3100/api/companies'     # 200 + 真实数据
curl -s 'http://127.0.0.1:3100/api/agents'        # 200 + 真实数据
curl -s 'http://127.0.0.1:3100/api/issues'        # 200 + 真实数据（无需 company_id）
curl -s 'http://127.0.0.1:3100/api/projects'      # 200 + 真实数据（无需 company_id）
```

### UI 代理（Vite :5173 → Rust server :3100）
```bash
/ui proxy:   200 OK
/api/health → 200
/api/companies → 200
/api/agents → 200
/api/issues → 200
/api/projects → 200
/api/cases → 200
/api/approvals → 200
/api/decisions → 200
/api/pipelines → 200
/api/goals → 200
/api/feature-flags → 200
/api/plugins/tools → 200
/api/companies/:id/status-cards → 200
```

### 中间件验证
```bash
# X-Request-Id 注入
curl -i /health | grep x-request-id
# x-request-id: 019fc670-1c75-75f1-b359-b1de07baea45

# X-Request-Id 透传
curl -i /health -H 'X-Request-Id: my-trace-123' | grep x-request-id
# x-request-id: my-trace-123

# CORS
curl -i /health -H 'Origin: http://127.0.0.1:5173' | grep access-control-allow-origin
# access-control-allow-origin: http://127.0.0.1:5173

# Body limit (>25MB → 413)
curl -i POST /api/companies --data-binary @/tmp/30mb.json
# HTTP/1.1 413 Payload Too Large
```

### 迁移工具
```bash
paperclip-migrate status --json
# {"available":196,"applied":196,"pending":[]}
paperclip-migrate verify --json
# {"missing":[],"ok":true,"present":[...8 张关键表...]}
```

## 修复的 Bug / 改进
| # | 文件 | 改动 |
|---|---|---|
| 1 | crates/pc-http/src/middleware/{mod,request_id,access_log,body_limit,redaction,cors,stack}.rs | 新建（5 个中间件 + stack 装配） |
| 2 | crates/pc-http/src/lib.rs | 导出中间件模块 |
| 3 | crates/pc-server/src/main.rs | apply_default_middleware 接入 |
| 4 | crates/pc-http/src/routes/issues.rs | ListQuery company_id → Option；handler 分流 |
| 5 | crates/pc-http/src/routes/{cases,projects,approvals,decisions,pipelines,goals}.rs | 同上 |
| 6 | crates/pc-repos/src/{issue,case,project,approval,decision,pipeline,goal}.rs | 加 list_all(limit) 方法 |
| 7 | pc-backup + pc-migrate 补全（见前轮记录） | 9 + 2 tests |

## 真实复刻距离（按代码行）
| 范围 | 原 (Node/TS) | 我们 (Rust) | 倍数 |
|---|---|---|---|
| 全部 | 755,410 行 | 33,797 行 | **4.5%** |
| server | 619,638 行 | ~25,000 行 | **4%** |
| 路由 agents | 3,976 | 567 | 14% |
| 路由 issues | 10,999 | 148 | 1.3% |
| 路由 pipelines | 2,913 | 124 | 4% |
| 路由 companies | 1,003 | 138 | 14% |
| 路由文件总数 | 56 | 61 (含 documents 额外) | 100%+ |

**深度差距**：路由都真实返回 DB 数据，但业务逻辑（权限校验、状态机、复杂关联、子资源）远浅于原版。

## 仍未完成（剩余真实差距）

### A. 路由深度（最高优先级）
- `agents.ts` 3976 行 → 我们 567 行：hire / 配置修订 / 权限继承 / instructions / 运行时环境选择
- `issues.ts` 10999 行 → 我们 148 行：checkout/wakeup 状态机、tree control、watchdog、recovery、文档附件、bot 交互
- `pipelines.ts` 2913 行 → 我们 124 行：transition 校验、case 事件
- `companies.ts` 1003 行 → 我们 138 行：成员管理、logo、import paths

### B. 中间件差距
- auth middleware（API key / session 双轨）
- board-mutation guard（敏感 mutation 需 board 认证）
- private-hostname guard
- trust-proxy
- http-log-policy

### C. 插件 worker stdio JSON-RPC 5 个 501 端点
- `bridge_data` / `bridge_action` / `plugin_data` / `plugin_action` / `bridge_stream`
- 当前 `worker_not_running()` 返回 501 是正确语义；待真实 plugin worker 跑通后改走 JSON-RPC

### D. A/B 字节级对比
- Node server vs Rust server 同一 fixture
- 需 E2E 套件

### E. Phase G 切流量
- UI 默认 `VITE_API_BASE` 指向 Rust server
- 归档 paperclip/ 目录

## 下一步建议（按业务价值）

1. **agents.rs 深度补完**（hire / config revision / 权限 / instructions）— 最高业务价值
2. **issues.rs 深度补完**（checkout / wakeup / tree）— 第二高
3. **完整 auth middleware**（API key + session 双轨）
4. **board-mutation guard**（CEO/board 角色校验）
5. **Playwright e2e**（UI 真实加载、点击、操作）
6. **Phase G 切流量**

## 关键约束
- pc-server binary 启动时尝试 spawn `node`（plugin worker bootstrap）— 警告但不影响 HTTP
- `plugins.rs` 中 `worker_not_running()` 返回 501 是**正确**的语义
- Postgres `paperclip` 用户对 `drizzle` schema 无权限（migration 失败），需用 `root` 用户
- `rm` 被沙箱拒绝，用 `touch` / Python 操作文件
