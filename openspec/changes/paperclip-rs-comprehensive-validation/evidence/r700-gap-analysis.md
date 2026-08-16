# R700 — Paperclip-rs 全量差距分析 (2026-08-16)

## 扫描方法

1. 对比 `paperclip/server/src/services/*.ts`（Node 端业务服务） vs
   `paperclip-rs/crates/pc-*` （Rust 端业务 crate）
2. 用 `find -name "*.rs"` 递归统计 Rust 真实代码量
3. 用 `wc -l` 统计 Node 端 .ts 行数
4. 用 semantic mapping 把 Node service 映射到 Rust crate

## 物理事实（2026-08-16 实测）

| 维度 | Node | Rust |
|---|---:|---:|
| Services 文件 | 211 .ts | 106 pc-* crates |
| src 行数（全部） | ~444K | 418,815 |
| 单测文件 | — | 486 |
| 单测数 | — | 6,830+ |
| 单测 fail | — | 0 |

## 真实差距矩阵（Node → Rust semantic mapping）

| Node Service | Node 行数 | Rust Crate | Rust 行数 | Gap 比例 | 评估 |
|---|---:|---|---:|---:|---|
| heartbeat.ts | 18,205 | pc-heartbeat | 10,485 | 58% | 进行中 |
| issues.ts | 8,383 | pc-issues | 12,028 | **超过** | ✅ |
| tool-access.ts | 7,028 | pc-tool | 1,381 | 80% | ⚠️ 大缺口 |
| tool-gateway.ts | 6,316 | pc-tool | (合并) | — | ⚠️ |
| company-skills.ts | 6,845 | pc-company-member | ? | ? | ⚠️ |
| company-portability.ts | 6,151 | pc-portability | ? | ? | ⚠️ |
| recovery/service.ts | 5,580 | pc-backup | 1,580 | 72% | ⚠️ 大缺口 |
| workspace-runtime.ts | 5,178 | pc-execution-workspace-guards | 205 | **96%** | 🔴 巨大缺口 |
| pipelines.ts | 5,175 | pc-pipelines | 4,870 | 6% | ✅ 接近 |
| secrets.ts | 4,893 | pc-secrets | 4,638 | 5% | ✅ 接近 |
| plugin-host-services.ts | 3,193 | pc-plugin-host | 9,067 | **超过** | ✅ |
| routines.ts | 3,103 | pc-routines | 2,887 | 7% | ✅ |
| plugin-loader.ts | 2,540 | pc-plugin-host | (合并) | — | ✅ |
| issue-thread-interactions.ts | 2,226 | pc-issues | (合并) | — | ✅ |
| authorization.ts | 2,187 | pc-authz | 3,786 | **超过** | ✅ |
| feedback.ts | 2,117 | pc-feedback (聚合) | 6 子目录 | 进行中 | ✅ |
| built-in-agents.ts | 2,054 | pc-built-in-agents? | ? | ? | ⚠️ |
| execution-workspaces.ts | 2,048 | pc-execution-workspace-guards | (合并) | ⚠️ |
| environment-runtime.ts | 2,029 | pc-environment | 7,163 | **超过** | ✅ |
| tool-access-policy.ts | 1,834 | pc-tool | (合并) | ⚠️ |
| task-watchdogs.ts | 1,815 | pc-task-watchdog? | ? | ⚠️ |
| plugin-worker-manager.ts | 1,633 | pc-plugin-host | (合并) | ✅ |
| workspace-file-resources.ts | 1,451 | pc-documents | (合并) | ⚠️ |
| attention.ts | 1,437 | pc-inbox | 828 | 42% | ⚠️ |
| company-search.ts | 1,340 | pc-companies | (合并) | ⚠️ |
| smoke-lab.ts | 1,235 | pc-approvals | (合并) | ⚠️ |
| issue-execution-policy.ts | 1,226 | pc-issues | (合并) | ✅ |
| projects.ts | 1,224 | pc-project | 1,108 | 9% | ✅ |
| issue-tree-control.ts | 1,212 | pc-issues | (合并) | ✅ |
| document-annotations.ts | 1,184 | pc-documents | (合并) | ⚠️ |

## 巨大缺口清单（Gap > 70%）

1. **workspace-runtime.ts (5,178 → 205 行)** — git worktree 管理 + runtime services + dirty quarantine + branch reconciliation
2. **tool-access.ts (7,028 → 1,381 行)** — tool access policy + execution
3. **tool-gateway.ts (6,316 → 合并)** — tool execution gateway + metrics
4. **recovery/service.ts (5,580 → 1,580 行)** — recovery + observability
5. **company-skills.ts (6,845)** — company skill policy + catalog
6. **company-portability.ts (6,151)** — company export/import
7. **heartbeat.ts (18,205 → 10,485 行)** — 进行中

## 评估口径修正

- 之前的 99.99% 是基于 routes/handlers 覆盖率，不是 service 业务逻辑覆盖率
- 真实业务逻辑覆盖率估计在 70-85%
- 主要缺口在 workspace runtime + tool gateway + recovery

## 后续推进计划

### 阶段 M：真实业务逻辑补足（取代之前的 J 阶段）

按"用户真实使用频率 × 缺口大小"排序：

1. **R701** — pc-tool 业务逻辑补足（tool-access + tool-gateway）
2. **R702** — pc-execution-workspace-guards 业务逻辑补足（workspace-runtime 核心）
3. **R703** — pc-backup 业务逻辑补足（recovery）
4. **R704** — pc-company-member 业务逻辑补足（company-skills）
5. **R705** — pc-portability 业务逻辑补足（company-portability）
6. **R706** — pc-heartbeat 收尾

### 阶段 N：UI 类型迁移 + 真实 mutation 流通

- 把 R694 生成的 49,871 行 d.ts 替换 `@paperclipai/shared`
- 修复 `/api/companies` 权限过滤 bug（companies.rs:80）
- 修复 `/Rd13b0/agents/all` Layout hooks 类型不匹配
- 完成 POST/PATCH/DELETE 真实流通验证

### 阶段 O：Adapter 收尾（待硬约束 #2 解除）

- 13 个 adapter 真实驱动接通
- Hermes gateway / openclaw / cursor-cloud 真正接入

## R700 关键交付

- [x] 全量 Node service vs Rust crate 差距扫描
- [x] 真实行数对比（不只是顶层文件）
- [x] 7 个巨大缺口定位
- [x] 重新评估：核心域真实覆盖率 70-85%（之前 99.99% 基于 routes）
- [x] 阶段 M/N/O 三阶段后续计划
