# paperclip-rs 文档索引

> 日期：2026-08-05 · 语言：中文 · 状态：持续更新

## 顶层入口

| 文件 | 内容 | 目标读者 |
|---|---|---|
| [`../README.md`](../README.md) | 项目简介、协议一致性、构建、运行 | 所有人 |
| [`DOCUMENTATION-ROADMAP.md`](DOCUMENTATION-ROADMAP.md) | **78 项文档缺口清单 + 优先级矩阵 + 完成判据** | 全体 / PM |
| `02-PAPERCLIP-ARCHITECTURE.md` | Paperclip Node/TS 端完整基线架构分析 | 全体 |
| `03-KAMEO-ACTOR-ANALYSIS.md` | kameo Actor 架构分析、抽象层、优化路由计划 | 后端 |
| `04-EXECUTION-PLAN.md` | 最新执行计划（完成/待实现/风险评估） | PM + 全体 |

## 文档子集

| 子目录（建议） | 内容 |
|---|---|
| `architecture/` | `02-…` / `03-…` / `08-…`、根目录 `ARCHITECTURE-DIAGRAMS.md` / `MODULE-MAPPING.md` |
| `internal/` | `01-…` / `06-…`（一次性根因 + gap matrix）、根目录 `PROJECT-PLAN.md`、`openspec/` |
| `audit/` | `05-PROGRESS-AUDIT.md`（445 KB，按月切片） |

> 具体迁移计划见 [`DOCUMENTATION-ROADMAP.md`](DOCUMENTATION-ROADMAP.md) §6。

## 相关文档（项目根目录）

| 文件 | 内容 |
|---|---|
| `PROJECT-PLAN.md` | 17 周 7 阶段执行蓝图（已更新状态） |
| `ARCHITECTURE-DIAGRAMS.md` | 10 张纯文本架构图（图 3 已更新） |
| `MODULE-MAPPING.md` | Node/TS → Rust crate 逐模块对照 |
| `openspec/changes/paperclip-rs-rewrite/proposal.md` | 重写提案（Why + What） |
| `openspec/changes/paperclip-rs-rewrite/design.md` | 高层设计决策（How） |
| `openspec/changes/paperclip-rs-rewrite/tasks.md` | 275 个 checkbox 任务清单 |
| `openspec/changes/paperclip-rs-rewrite/specs/` | 19 个机器可读契约 spec |

## 速查

- **还要写哪些文档才能达到顶级项目？** → [`DOCUMENTATION-ROADMAP.md`](DOCUMENTATION-ROADMAP.md)
- **为什么 Vite 启动失败？** → `01-VITE-ERROR-ROOT-CAUSE.md`
- **paperclip Node 端有什么模块？** → `02-PAPERCLIP-ARCHITECTURE.md`
- **Actor 怎么工作？怎么路由？** → `03-KAMEO-ACTOR-ANALYSIS.md`
- **现在做到哪了？下一步做什么？** → `04-EXECUTION-PLAN.md`
- **Node 与 Rust 的实现差距？** → `06-NODE-RUST-GAP-MATRIX.md` / `07-COMPREHENSIVE-GAP-ANALYSIS.md`
