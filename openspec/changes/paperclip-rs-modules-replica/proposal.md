# Proposal: paperclip-rs Modules Replica

## Why

`paperclip/` 仓库（760 server files / 44.4 万行 TS + 1168 UI files / 34.4 万行 + 10 个内置适配器 + 109 张表）当前所有后端职责都跑在 Node + Express + better-auth + Drizzle 单体上。痛点（PROJECT-PLAN.md 已明列）：V8 启动开销大、冷启动 3–5s、常驻 400MB+、类型边界模糊、跨平台部署薄弱。

`paperclip-rs/` 已经有 Cargo workspace 与 ~244k 行 Rust 实现，骨架完整，但与 plan 对照仍有具体缺口（MOD-MAPPING + 盘点：apps/ 目录契约未独立、UI 切流验证缺失、各模块"真实最佳方式"实现程度不均）。需要按模块逐一复刻、复刻一个真实验证一个，按 Rust 的高内聚低耦合与零成本抽象原则落地。

## What

按 16 个差距维度（G1–G10 + 子项）拆成可独立交付的模块，每个模块满足：

- **复刻**：`paperclip-{node}/<dir>` → `paperclip-rs/<target>`，行为等价、接口形态按 Rust 习惯表达。
- **真实验证**：每个模块完成后跑一次真实运行（启动 / curl / DB roundtrip / e2e / 集成测试），不是只跑 `cargo test`。
- **高内聚低耦合**：通过 trait 抽象隔离边界（已存在的 `StorageProvider`、`SecretsProvider`、`AdapterRuntime` 等保留并扩充）。
- **Rust 最佳方式**：使用类型状态、newtype、错误约定、零拷贝日志、强类型 ID、编译期 SQL 校验（sqlx）；不引入与 Rust 习惯冲撞的"类 Node"风格。

## Module Order（16 个可独立交付的模块，全部从 phase=build 顺序推进）

| # | 模块 | 来源 (paperclip) | 目标 | 验收口径 |
|---|---|---|---|---|
| M1 | apps/ 目录契约 | `crates/pc-server`, `crates/pc-cli` | `apps/pc-server`, `apps/pc-cli` | `cargo check` + `cargo build -p pc-server` 仍成功 |
| M2 | E2E 基线 | 整栈 | 启动 PC server，跑 migrate，curl /health，UI happy path | `pc-server` 起来、`/health` 200、migrate 109 表通过、UI 可登录到一个空公司 |
| M3 | Storage 真实链路 | `server/src/storage/*` | `pc-storage` 完整 trait + S3 SDK 接入 | 真实 put/get + multipart + presign 集成测试 |
| M4 | Secrets 真实链路 | `server/src/secrets/*` | `pc-secrets` AES-GCM + aws-sm + 多 provider 链 | 加解密 roundtrip + 自管密钥读取 |
| M5 | Backup 真实链路 | `packages/db/backup.ts` | `pc-backup` pg_dump / pg_restore 一致性 | 真实库 dump→restore 后数据字节级一致 |
| M6 | Migrate 工具 | `packages/db/migrate.ts` | `pc-migrate` 上 / 下 / status / create | 用现有 109 表 SQL 文件做一次 fresh up |
| M7 | Auth + AuthZ | `server/src/auth/better-auth.ts` + `services/authorization.ts` | `pc-auth` + `pc-authz` | session / cookie / CSRF / API key 完整；策略逐条迁 |
| M8 | DB schema + Repos 25 子模块 | `packages/db/schema/*` + `server/src/services/*` | `pc-db` (DDL) + `pc-repos` 全 25 | 与 Node 同 fixture 下行为字节级一致 |
| M9 | HTTP 路由 56 | `server/src/routes/*` | `pc-http` 56 路由 | 与原 server 字节级一致（happy + 3 edge） |
| M10 | OpenAPI 3.1 | `server/src/routes/openapi.ts` | `pc-openapi` utoipa | `/openapi.json` 与原产物字段对齐 |
| M11 | Realtime + WS | `server/src/realtime/*` | `pc-realtime` + `pc-ws` | 真实 WS：触发 live-event → UI 收到 |
| M12 | Heartbeat 状态机 | `services/heartbeat.ts` + recovery/* | `pc-heartbeat` | pick→invoke→finalize→live-event 端到端剧本 |
| M13 | Adapter-API + 11 适配器 | `packages/adapters/*` | `pc-adapter-api` + 11 个 crate | 每个适配器 ≥1 happy + ≥1 failure 集成测试 |
| M14 | Plugin host | `server/src/services/plugin-*` + `packages/plugins/sdk` | `pc-plugin-host` + `pc-plugin-protocol` | 与原 SDK worker 互操作通过 |
| M15 | Workflow + Cron | `services/routines.ts` + `pipelines.ts` + `cron.ts` | `pc-workflow` + `pc-cron` | 真实 cron 触发 routine + pipeline |
| M16 | CLI 全子命令 | `paperclip/cli/*` 19 commands | `pc-cli` 全部 + `--json` | 每个子命令 `--help` & 真实跑一遍 |

UI 部分（M0/U1–U3）单独列：

| # | 模块 | 来源 | 目标 | 验收 |
|---|---|---|---|---|
| U1 | UI 切流与契约冻结 | `paperclip/ui/*` | `paperclip-rs/ui` + `VITE_API_BASE` | Rust server 跑通，UI 60 个 api client 全部 happy |
| U2 | Playwright e2e 剧本 | 全栈 | `tests/e2e/` 真实跑 | happy path 脚本过 |
| U3 | OpenAPI ↔ UI 类型对齐 | ui/src/api/* | ts-rs 或类似 | 字段级一致 |

## Out of Scope（不属于本 change）

- 嵌入式 PG 跨平台完善（M-IPG，单独 change，因为涉及 binary 分发与 CI 矩阵）
- Feature flags / Doc anchors 等辅助 crate 的进一步丰富（合在 M8 / M9 末尾）
- Doc-anchors deep-link（合在 M9 文档）
- Node server 物理删除（合并切流完成后归档时做）

## Success Criteria

- ✅ M1–M16 全部"复刻→真实验证"两个动作二选一都完成
- ✅ 全部 56 路由与原 server 字节级一致（happy + ≥3 edge case × 路由）
- ✅ 11 个 adapter 全部 happy + failure 集成测试
- ✅ UI（`VITE_API_BASE` 切到 Rust server）完整 happy path 通过
- ✅ `cargo clippy -- -D warnings` 无警告
- ✅ `cargo fmt --check` 无 diff
- ✅ 性能：`wrk` 同样 fixture 下 P99 延迟较 Node server 下降 ≥ 30%、RSS 下降 ≥ 40%

## Sequencing Rationale

按"先基线、后模块；先骨架、后细节；先真实、后优化"：

```
M1 apps/契约 → M2 基线 → [M3-M6 数据基建并列] → [M7 认证] → [M8 仓储]
  → [M9 路由 → M10 OpenAPI] → [M11 WS → M12 Heartbeat]
  → [M13 适配器 → M14 插件] → [M15 工作流 → M16 CLI]
  → [U1-U3 UI 集成]
```

模块之间存在依赖：M9 路由依赖 M8 repos；M11 WS 依赖 M8；M12 heartbeat 依赖 M13 部分 adapter；M14 插件依赖 M8 + M13；M15 cron 依赖 M8；M16 CLI 依赖前面所有。M1 + M2 必须先做，因为它们是所有后续真实验证的地基。

## Open Questions for Design Phase

- M2 基线选择哪种数据库：嵌入式 PG 还是外部 PG？建议优先外部 PG（更稳定、更快），用 testcontainers-rs 起容器。
- M3 Storage：是否在本阶段把 s3-sdk 完整接入（依赖 ↑ 约 8MB 二进制大小），先用 `aws-sdk-s3` 完整版。
- M7 Auth：复刻 better-auth 还是仅 cookie/session/CSRF？建议仅行为复刻，避免强绑。
- M12 Heartbeat：现有 52k Rust 是否覆盖 Node 的全部逻辑？以现有为底，按缺失补全。
- M13 Adapter 深度：claude-local 7114 行 TS，Rust 版本可能 ≥ 8k 行；其他适配器按需。
