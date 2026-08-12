# Proposal: paperclip-rs 核心模块与 UI 整体功能复刻（差距清零）

## 背景问题

- paperclip（Node + TS monorepo，server 760 源文件 / 44.4 万行）与 paperclip-rs（Rust workspace，101 crates / 2 binaries）并行存在。
- paperclip-rs 已复刻大部分路由（56/56 路由模块 100% 有 Rust 对应）、仓储层与认证授权，累计 ~3,400+ 单测全绿（R634 末）。
- 仍存在三类差距：(1) middleware 缺口（compression / trust-proxy / private-hostname-guard / validate / http-log-policy / board-mutation-guard）；(2) 部分 server services 未实现或仅桩（plugin host 内部、run-continuations、issue-liveness、invite-grants、hot-restart、tool-access-policy、summary-slot-finalization、pipeline-case-outputs/aggregation、run-log-store、environment-custom-image-runtime/terminal-sessions 等）；(3) UI 接入验证不完整（M19 UI↔OpenAPI 覆盖 86.7%，复杂流程未做端到端）。
- 适配器域（11 个内置适配器 + gateway）已基本完成（R596-R624，~1,400+ adapter 测试），本轮按用户指示**不纳入范围**。

## 目标

1. 以 Node server 为行为契约，逐模块补齐核心（非适配器）差距，高内聚低耦合、一个模块一个模块复刻。
2. 每个模块复刻后真实验证：单测 + 契约测试 + 全栈（临时 PG → pc-migrate up → pc-server → UI/curl）。
3. UI 接入完成整体功能复刻：M19 UI↔OpenAPI 对齐到 100%，V11 60 client happy path 保持全绿，Playwright v12 全流程通过。
4. 输出最新进度百分比与逐模块差距清单，更新文档（MODULE-MAPPING / PROJECT-PLAN / progress-snapshot）。

## 范围

- **核心域**：server middleware、server services（非适配器）、pc-repos、pc-http 路由行为、pc-auth/authz、pc-heartbeat/realtime、CLI 相关核心命令。
- **UI 接入**：UI client × Rust OpenAPI 对齐、UI→Rust server 全栈冒烟、复杂流程（terminal WS、settings、plugins、heartbeat 状态）端到端。
- **验证**：workspace 单测全绿、契约测试、e2e baseline、long-run、perf-baseline、OpenAPI 生成链路修复（rust-openapi.json 目前 paths 为空）。

## 非目标（Non-goals）

- 适配器域（11 个 adapter crate 的内部实现与适配器 e2e）——按用户指示暂不处理。
- UI 源码改造（1168 文件完全复用，仅验证与配置切换）。
- 数据库 schema 变更（109 表 SQL DDL 已冻结）。
- 性能压测对比优化（仅重跑 perf-baseline 作为回归保护，不做优化专项）。
*** End Patch
