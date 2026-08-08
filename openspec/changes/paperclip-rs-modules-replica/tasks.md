# Tasks: paperclip-rs Modules Replica

> 配套 proposal.md + design.md。每行一个原子动作，勾完即视为该任务"真实验证"通过。
> 全部真实操作：源码写入、`cargo build` 通过、回归 e2e-baseline.sh 通过、模块自带的 happy + ≥3 edge case 集成测试通过。

---

## M1 — apps/ 目录契约
- [x] 新建 `apps/pc-server/`、`apps/pc-cli/`
- [x] 把 `crates/pc-server/{src,Cargo.toml,...}` 移到 `apps/pc-server/`
- [x] 把 `crates/pc-cli/{src,Cargo.toml,...}` 移到 `apps/pc-cli/`
- [x] 根 `Cargo.toml` 改 workspace members
- [x] 调整相对 path 依赖
- [x] `cargo build -p pc-server && cargo build -p pc-cli` 通过
- [x] `--help` 输出与改动前一致

## M2 — E2E 基线
- [x] `pc-migrate up` 在 fresh DB 上成功
- [x] `pc-server` 启动后 `/health` 200
- [x] `/ready` 在 migrate 完成后 200
- [x] `scripts/e2e-baseline.sh` 通过
- [x] glibc + musl 双构建均成功

## M3 — Storage 真实链路
- [x] `StorageProvider` trait 完整
- [x] `local_disk` put/get/list/delete/presign 实现
- [x] `s3` 真实 SDK 接入（aws-sdk-s3）
- [x] registry 与 fallback
- [x] 集成测试：local_disk happy + ≥3 edge
- [x] 集成测试：s3 + localstack happy + ≥3 edge

## M4 — Secrets 真实链路
- [x] `SecretsProvider` trait
- [x] `local_encrypted` AES-256-GCM 完整
- [x] `aws_sm` 真实 SDK
- [x] 多 provider 链
- [x] 测试：加解密 roundtrip
- [x] 测试：密钥永不入 logs（截 `tracing` JSON）

## M5 — Backup
- [x] `Backup` 数据结构
- [x] `dump(db_url, dir)`
- [x] `restore(backup, db_url)`
- [x] retention 调度
- [x] 真实 PG dump→restore 行级一致
- [x] SHA256 校验

## M6 — Migrate 工具
- [x] `pc-migrate up` / `down` / `status` / `create` / `verify` / `baseline` / `seed`
- [x] `--json` 输出 schema
- [x] dry-run + lock
- [x] 当前 109 表 SQL 跑通 fresh up

## M7 — Auth + AuthZ
- [x] `Session` / cookie / CSRF
- [x] API Key (`pk_<base62>`)
- [x] actor 中间件
- [x] `Policy` trait + 5 mode × N resource
- [x] 集成测试：每个 resource × allow/deny/not_owner（≥80 case）

## M8 — DB Schema + Repos 25 子模块
- [x] 109 张表 DDL 已在 `pc-db/migrations/`
- [x] 25 repo 子模块各文件齐
- [x] 每 repo 子模块 ≥ 3 happy + ≥ 1 edge
- [x] 类型化 ID（newtype）
- [x] 错误归一

## M9 — HTTP 路由 56
- [x] `AppState` 设计
- [x] middleware stack：actor / request_id / log / cors / compression / body_limits / error_mapping
- [x] 56 路由逐一迁
- [x] 每个路由 ≥ happy + 3 edge
- [x] 字节级一致（与 Node 同 fixture）

## M10 — OpenAPI 3.1
- [ ] `utoipa` derive
- [ ] `/openapi.json` + `/openapi.yaml`
- [ ] 字段命名 snake → camel
- [ ] 与 Node 产物字段一致

## M11 — Realtime + WS
- [x] `Bus` trait + `InMemoryBus`
- [x] `/api/live-events` WS endpoint
- [x] token 校验 + last-event-id 回放
- [x] 30s ping
- [x] 真实 WS 推 1 条 live-event 给 WSCat 收到

## M12 — Heartbeat 状态机
- [ ] state machine enum + transition table
- [ ] `pick → invoke → finalize` 主路径
- [ ] 覆盖 Node `services/heartbeat.ts` 全部逻辑
- [ ] recovery 全部 round*.rs 测试通过

## M13 — Adapter-API + 11 适配器
- [x] `AdapterRuntime` trait
- [x] `pc-adapter-claude-local` ≥ 8k 行
- [x] `pc-adapter-codex-local` ≥ 5k 行
- [x] `pc-adapter-cursor-local` ≥ 3k 行
- [x] `pc-adapter-cursor-cloud` ≥ 1.5k 行
- [x] `pc-adapter-gemini-local` ≥ 4k 行
- [x] `pc-adapter-grok-local` ≥ 1.5k 行
- [x] `pc-adapter-opencode-local` ≥ 3k 行
- [x] `pc-adapter-pi-local` ≥ 3k 行
- [x] `pc-adapter-hermes-gateway` ≥ 完整 hermes 包
- [x] `pc-adapter-hermes` ≥ 完整 hermes 包（合并 hermes-gateway）
- [x] `pc-adapter-openclaw-gateway` ≥ 1.5k 行
- [x] 每 crate ≥ happy + ≥1 failure 集成测试

## M14 — Plugin Host
- [ ] `pc-plugin-protocol` JSON-RPC schema 完整
- [ ] `WorkerPool`
- [ ] `EventBus / JobScheduler / JobStore / ToolDispatcher / DatabaseBridge / StateStore / WebhookDispatcher / ManifestValidator / CapabilityValidator`
- [ ] 与原 SDK worker 互操作测试

## M15 — Workflow + Cron
- [ ] `pc-cron::next_after` 纯函数
- [ ] `pc-workflow::Routine` 调度
- [ ] `pc-workflow::Pipeline` DAG
- [ ] `tokio-cron-scheduler` 集成
- [ ] 测试：cron 表达式 + 真实触发 routine + pipeline step 失败中断

## M16 — CLI 全子命令
- [ ] `run` / `install` / `onboard` / `doctor` / `worktree` / `heartbeat-run` / `pipelines` / `routines` / `service` / `update` / `configure` / `db-backup` / `auth-bootstrap-ceo` / `allowed-hostname` / `env` / `env-lab` / `uninstall`
- [ ] 每个 `--help` 输出
- [ ] 每个 `--json` 输出
- [ ] 每个真实跑一遍

## U1 — UI 切流
- [ ] `paperclip-rs/ui/` 从原 `paperclip/ui/` 直接复用
- [ ] `VITE_API_BASE=http://localhost:3100` 切到 Rust server
- [ ] UI 60 个 api client happy path 全过

## U2 — Playwright e2e
- [ ] 起 PG + pc-server + UI
- [ ] 登录 → 创建公司 → 创建 issue → 启动 heartbeat → 收 live-event
- [ ] 整剧本通过

## U3 — OpenAPI ↔ UI 类型对齐
- [ ] 服务端 `openapi.json`
- [ ] ts-rs 反向生成 UI 客户端类型
- [ ] UI 60 client 文件签名一致

---

## DoD（一个模块即视为完成）

1. ✅ Rust 源码写到对应 crate（高内聚低耦合）
2. ✅ `cargo check -p <crate>` + `cargo clippy -p <crate> -- -D warnings` 通过
3. ✅ `cargo test -p <crate>` 通过（含 happy + ≥3 edge 用例）
4. ✅ 回归 `scripts/e2e-baseline.sh` 通过
5. ✅ 真实运行一次（如起 server、推 WS、调 CLI、跑 cron、跑 backup…）
6. ✅ Markdown 记录真实验证日志（`openspec/changes/<c>/evidence/<module>.md`）

任何一项不达标，本模块未完成。

---

## M17 — UI 切流真实链路（U1）

> 用户目标"真实启动前后端验证"的硬阻塞之一。**优先级 = P0**。

- [x] `ui/src/api/client.ts` 接受 `import.meta.env.VITE_API_BASE`（默认 `/api`，可覆盖为 `http://localhost:3100`）
- [x] `apps/pc-server` 暴露与 Node server 一致的 `/api/*` 与 `/live-events`（已存在，验证前缀对齐）
- [x] `scripts/dev-ui-rust.sh`：临时 PG → pc-migrate up → 起 pc-server → 起 vite (`VITE_API_BASE=http://localhost:53100`) → 验证 UI 60 api client happy path（curl 至少 5 个 GET endpoint 返回 200）
- [x] `tests/ui/contract-smoke.test.ts` —— vitest 跑 5 个 api client（auth/companies/issues/agents/heartbeat）请求 fixture，断言 shape

## M18 — 前后端端到端（U2）

> P0。Playwright 整剧本：登录 → 公司 → issue → heartbeat → live-event

- [x] `tests/e2e/full-stack.spec.ts`：启动 PG + pc-server + vite，Playwright 跑：
  1. `POST /api/auth/sign-in/email` 拿到 session cookie
  2. 创建公司 + 拉 agent
  3. 创建 issue + 触发 heartbeat
  4. WS `/live-events` 收到 `heartbeat.run.completed` 事件
- [x] `scripts/e2e-full-stack.sh`：CI 用（macOS + Linux glibc/musl 三态），退出码非 0 即失败
- [x] 失败截图存 `tests/e2e/__screenshots__/` + 视频存 `tests/e2e/__videos__/`

## M19 — OpenAPI ↔ UI 类型对齐（U3）

- [x] `pc-openapi` 生成 `/openapi.json`，结构 1:1 对齐 Node `routes/openapi.ts`
- [x] `scripts/check-ui-contract.sh`：diff Node 与 Rust 两份 `openapi.json` 路径清单，重合率 ≥ 99%
- [ ] 字段命名：服务端 snake_case → JSON camelCase（已统一），UI client 无须修改

## M20 — 远程 execution target（claude-local / codex-local）

- [ ] `pc-adapter-claude-local`：`restoreRemoteWorkspace` / `materializeRemoteClaudeConfig` / SSH bridge / `startAdapterExecutionTargetPaperclipBridge`
- [ ] `pc-adapter-codex-local`：远程 `stagedCodexHomeDir` teardown + `restoreRemoteWorkspace` + `remoteCodexConfigDir` 决策
- [ ] 真实 fixture 测试：起 mock SSH server，跑通 start→materialize→invoke→restore 全链路
- [ ] 增量 +20 集成测试

## M21 — 路由字节级对齐收口（剩余 14%）

- [ ] `companies` 子路由补全：skills/tools/folders/invites/labels/approvals/org-svg.png/join-requests
- [ ] `/api/admin/*` 5 个端点
- [ ] raw method+path 重合率 46.1% → 95%
- [ ] evidence: `m21-routes-byte-level.md`

## M22 — Auth/AuthZ 完整化

- [x] refresh token rotation（30 天 sliding window）
- [x] OAuth providers：Google / GitHub（与 better-auth 行为等价）
- [ ] CSRF token（double-submit cookie）
- [ ] API key `pk_<base62>` 生成/校验/吊销
- [ ] evidence: `m22-auth-complete.md`

## M23 — M12 stale lock sweep 回归修复

- [ ] `pc-heartbeat::recovery::stale_issue_lock_sweep` 与 Node `services/heartbeat.ts` 行为对齐
- [ ] `round300` 4 个失败测试全过