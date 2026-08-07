# Tasks: paperclip-rs Modules Replica

> 配套 proposal.md + design.md。每行一个原子动作，勾完即视为该任务"真实验证"通过。
> 全部真实操作：源码写入、`cargo build` 通过、回归 e2e-baseline.sh 通过、模块自带的 happy + ≥3 edge case 集成测试通过。

---

## M1 — apps/ 目录契约
- [ ] 新建 `apps/pc-server/`、`apps/pc-cli/`
- [ ] 把 `crates/pc-server/{src,Cargo.toml,...}` 移到 `apps/pc-server/`
- [ ] 把 `crates/pc-cli/{src,Cargo.toml,...}` 移到 `apps/pc-cli/`
- [ ] 根 `Cargo.toml` 改 workspace members
- [ ] 调整相对 path 依赖
- [ ] `cargo build -p pc-server && cargo build -p pc-cli` 通过
- [ ] `--help` 输出与改动前一致

## M2 — E2E 基线
- [ ] `pc-migrate up` 在 fresh DB 上成功
- [ ] `pc-server` 启动后 `/health` 200
- [ ] `/ready` 在 migrate 完成后 200
- [ ] `scripts/e2e-baseline.sh` 通过
- [ ] glibc + musl 双构建均成功

## M3 — Storage 真实链路
- [ ] `StorageProvider` trait 完整
- [ ] `local_disk` put/get/list/delete/presign 实现
- [ ] `s3` 真实 SDK 接入（aws-sdk-s3）
- [ ] registry 与 fallback
- [ ] 集成测试：local_disk happy + ≥3 edge
- [ ] 集成测试：s3 + localstack happy + ≥3 edge

## M4 — Secrets 真实链路
- [ ] `SecretsProvider` trait
- [ ] `local_encrypted` AES-256-GCM 完整
- [ ] `aws_sm` 真实 SDK
- [ ] 多 provider 链
- [ ] 测试：加解密 roundtrip
- [ ] 测试：密钥永不入 logs（截 `tracing` JSON）

## M5 — Backup
- [ ] `Backup` 数据结构
- [ ] `dump(db_url, dir)`
- [ ] `restore(backup, db_url)`
- [ ] retention 调度
- [ ] 真实 PG dump→restore 行级一致
- [ ] SHA256 校验

## M6 — Migrate 工具
- [ ] `pc-migrate up` / `down` / `status` / `create` / `verify` / `baseline` / `seed`
- [ ] `--json` 输出 schema
- [ ] dry-run + lock
- [ ] 当前 109 表 SQL 跑通 fresh up

## M7 — Auth + AuthZ
- [ ] `Session` / cookie / CSRF
- [ ] API Key (`pk_<base62>`)
- [ ] actor 中间件
- [ ] `Policy` trait + 5 mode × N resource
- [ ] 集成测试：每个 resource × allow/deny/not_owner（≥80 case）

## M8 — DB Schema + Repos 25 子模块
- [ ] 109 张表 DDL 已在 `pc-db/migrations/`
- [ ] 25 repo 子模块各文件齐
- [ ] 每 repo 子模块 ≥ 3 happy + ≥ 1 edge
- [ ] 类型化 ID（newtype）
- [ ] 错误归一

## M9 — HTTP 路由 56
- [ ] `AppState` 设计
- [ ] middleware stack：actor / request_id / log / cors / compression / body_limits / error_mapping
- [ ] 56 路由逐一迁
- [ ] 每个路由 ≥ happy + 3 edge
- [ ] 字节级一致（与 Node 同 fixture）

## M10 — OpenAPI 3.1
- [ ] `utoipa` derive
- [ ] `/openapi.json` + `/openapi.yaml`
- [ ] 字段命名 snake → camel
- [ ] 与 Node 产物字段一致

## M11 — Realtime + WS
- [ ] `Bus` trait + `InMemoryBus`
- [ ] `/api/live-events` WS endpoint
- [ ] token 校验 + last-event-id 回放
- [ ] 30s ping
- [ ] 真实 WS 推 1 条 live-event 给 WSCat 收到

## M12 — Heartbeat 状态机
- [ ] state machine enum + transition table
- [ ] `pick → invoke → finalize` 主路径
- [ ] 覆盖 Node `services/heartbeat.ts` 全部逻辑
- [ ] recovery 全部 round*.rs 测试通过

## M13 — Adapter-API + 11 适配器
- [ ] `AdapterRuntime` trait
- [ ] `pc-adapter-claude-local` ≥ 8k 行
- [ ] `pc-adapter-codex-local` ≥ 5k 行
- [ ] `pc-adapter-cursor-local` ≥ 3k 行
- [ ] `pc-adapter-cursor-cloud` ≥ 1.5k 行
- [ ] `pc-adapter-gemini-local` ≥ 4k 行
- [ ] `pc-adapter-grok-local` ≥ 1.5k 行
- [ ] `pc-adapter-opencode-local` ≥ 3k 行
- [ ] `pc-adapter-pi-local` ≥ 3k 行
- [ ] `pc-adapter-hermes-gateway` ≥ 完整 hermes 包
- [ ] `pc-adapter-hermes` ≥ 完整 hermes 包（合并 hermes-gateway）
- [ ] `pc-adapter-openclaw-gateway` ≥ 1.5k 行
- [ ] 每 crate ≥ happy + ≥1 failure 集成测试

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
