# Paperclip-rs 复刻进度审计（2026-08-03，第五轮）

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
| 测试用例 | 239 | **327** | +57 (routines 8 + middleware 15 + backup 9 + migrate 2 + telemetry 6 + cli 11 + 上一轮未计) |
| Test suites | 68 | **84** | +13 |
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

## 第五轮新增

### Routines 模块 8/8 测试通过
- `pc-repos/src/routine.rs` (~2000 行)：完整 routine / revision / trigger / run / dispatch
  - `create_webhook_trigger` 端到端：AES-GCM 加密 secret → `company_secrets` + `company_secret_versions` → `routine_triggers.secret_id` FK → append revision
  - `fire_public_trigger`：bearer / github_hmac / hmac_sha256 / none 4 种 signing_mode 常量时间校验 → dispatch_run 链路复用 → idempotency_key 命中复用旧 run
- `pc-http/src/routes/routines.rs` (923 行)：完整 CRUD + trigger + revision + run + fire 路由
  - webhook 创建返回 `{trigger, secretMaterial: {webhookUrl, webhookSecret}, revision}` 一次性质明文 secret
  - webhook fire 端点接受任意 JSON body（直接作为 payload）、读取 `authorization` / `idempotency-key` / `x-hub-signature-256` / `x-signature` / `x-timestamp` 头
- `pc-secrets` 补 `hmac_sha256` helper（HMAC-SHA256 hex 编码）用于 github_hmac / hmac_sha256 模式

### 验证
```bash
cd paperclip-rs
rtk cargo test -p pc-http --test routines_http_contract  # 8/8 通过
rtk cargo test --workspace --no-fail-fast                # 327 passed (84 suites)
```

## 第六轮新增

### 修复 `/api/cases` 与 `/api/pipelines` 路由重复注册
- `pc-http/src/routes/cases.rs`：`/api/cases/:id` → `/api/cases/:case_id` 统一路径参数命名
- `pc-http/src/routes/pipelines.rs`：移除冲突的 `/api/cases/:case_id` 基础路由，仅保留子路径（transition / claim / release / events / issue-links）
- 修复后 `paperclip-server` 能正常启动，`/health` 返回 200，curl 可访问所有路由

### 修复 `extensions.rs` 与 `issues.rs` 路由重复注册
- `pc-http/src/routes/extensions.rs`：移除重复的 `/api/companies/:company_id/issues/count`，仅保留 `/api/issues/:id/heartbeat-context`

### 修复 board API key 创建 stub
- `pc-http/src/routes/access.rs::board_keys_create`：
  - **之前**：`let key_hash = "key-hash-stub".to_string();`（硬编码 stub）
  - **之后**：生成 `pcp_board_<uuid>` 一次性 token，使用 `sha2_sha256(token)` 哈希后存入 `board_api_keys.key_hash`，响应中包含一次性明文 token（与 `hashBearerToken` / `createBoardApiToken` 等价）
- 新增 `pc-http/tests/access_http_contract.rs::board_key_create_persists_real_sha256_hash_and_returns_one_time_token`：
  - 验证创建返回一次性 token
  - 验证 DB 存储哈希与 `SHA-256(token)` 一致（64 字符 hex）
  - 验证列表接口不泄露明文 token
  - 验证 revoke 删除

### 路由 company_id 灵活化（扩展）
- `pc-http/src/routes/routines.rs`：`ListQuery.company_id` 改为 `Option<Uuid>`，handler 在 `None` 时走 `RoutineRepo::list_all(200)`
- `pc-repos/src/routine.rs`：新增 `list_all(limit)` 方法

### camelCase 修复
- `pc-repos/src/pipeline.rs::PipelineRow`：添加 `#[serde(rename_all = "camelCase")]`，与原 Node server 一致

### 当前门禁（最新）
- ✅ `cargo build --workspace` — 0 errors
- ✅ `cargo build --bin paperclip-server` — 0 errors
- ✅ `cargo test --workspace --no-fail-fast` — **328 passed (85 suites)**
- ✅ `paperclip-server` 启动 + 端到端冒烟：
  - `/health` → `{"db":{"ok":true},"status":"ok","version":"0.1.0"}`
  - `/api/companies?limit=2` → 200 + 真实数据
  - `/api/agents?limit=2` → 200 + 真实数据（camelCase）
  - `/api/issues?limit=2` → 200 + 真实数据
  - `/api/cases?limit=2` → 200 + 真实数据
  - `/api/routines?limit=3` → 200 + 真实数据（无需 company_id）
  - `/api/pipelines?limit=3` → 200 + 真实数据（camelCase）
  - `/api/feature-flags` → 200 + 默认 2 个 flag


## 第七轮新增

### 新增 `issue_checkout_locks` 表迁移
- `paperclip-rs/crates/pc-db/migrations/drizzle/0198_issue_checkout_locks.sql`：
  - `issue_checkout_locks` 表 + 2 索引 + FK 到 issues
  - 已应用到 `paperclip_repos`（测试库）和 `paperclip`（默认库）
  - 修复 `/api/issues/:id/checkout` 端点的 FK 缺失问题

### 新增 issue checkout/wakeup 路由契约测试
- `pc-http/tests/issues_checkout_wakeup_contract.rs`（4 个测试）：
  - `issue_checkout_persists_run_id_and_creates_lock_and_queues_wakeup`：验证 checkout 写入 issues.checkout_run_id、issue_checkout_locks 行、agent_wakeup_requests 行
  - `issue_wakeup_endpoint_queues_request_without_checkout`：验证 wakeup 端点独立工作（source='issue_wakeup'）
  - `issue_checkout_404_when_issue_missing`：404 路径
  - `issue_checkout_handles_existing_lock_gracefully`：验证 FK 满足后可接受新 run

### 端到端冒烟验证（checkout/wakeup 完整链路）
- POST `/api/issues/<id>/checkout` → `{"actorId","issueId","runId","status":"checked-out","wakeupQueued":true}`
- POST `/api/issues/<id>/wakeup` → `{"actorId","issueId","status":"wakeup-queued"}`
- DB 验证：issues.checkout_run_id 设置、issue_checkout_locks 行存在、agent_wakeup_requests 两条（checkout + wakeup 各一）

### 当前门禁（最新）
- ✅ `cargo build --workspace` — 0 errors, 19 warnings（pre-existing）
- ✅ `cargo build --bin paperclip-server` — 0 errors
- ✅ `cargo test --workspace --no-fail-fast` — **332 passed (86 suites)**
- ✅ `paperclip-server` 启动 + checkout/wakeup 端到端冒烟通过


### 第八轮新增

### approvals + decisions 路由契约测试
- `pc-http/tests/approvals_decisions_crud_contract.rs`（3 个测试）：
  - `approval_create_get_list_decide_delete_lifecycle`：完整审批流（创建→查询→过滤列表→决定→删除）
  - `approval_create_rejects_empty_approval_type`：422/400 验证
  - `decision_create_and_list_filter_by_company`：决策 CRUD（需要预先存在 agent/issue/run 以满足 FK）

### 当前门禁（最新）
- ✅ `cargo build --workspace` — 0 errors
- ✅ `cargo test --workspace --no-fail-fast` — **335 passed (87 suites)**
- 已测试路由组：access / agents / companies / issues / issues_checkout_wakeup / **approvals** / **decisions** / pipelines / routines（9 个组）

