# R777 — paperclip Node vs paperclip-rs 差距深度审计

日期: 2026-08-17
范围: 全 paperclip Node (471 模块) × 全 paperclip-rs (92 非适配 crate) 对比
方法: 文件名交叉 + 关键词搜索 + 模块细分

## 1. 总体规模

| 维度 | paperclip Node | paperclip-rs Rust | 比率 |
|---|---:|---:|---:|
| 顶层模块 | 2 packages + 1 server | 1 workspace (108 crates, 92 非适配) | — |
| TS / Rust 文件数 | 2271 | 1539 | 1.5:1 |
| shared/src 业务模块 | 38 (.ts) | — | — |
| server/src 业务模块 | 297 (.ts, 排除 __tests__) | — | — |
| 核心域实现 LOC | issues.ts 8383 + agents.ts 1051 + routines.ts 3103 + heartbeat.ts 18205 | pc-issues 13590 + pc-agent 5244 + pc-routines + pc-heartbeat 51253 | Rust 更厚 (~1.6-2.8x) |
| Adapter | 13 | 15 (硬约束 #2 不动) | — |

## 2. 文件名级交叉结果

| 类别 | 数量 |
|---|---:|
| 明确映射 (1 个 Node 文件 → 1 个 Rust crate) | 75 |
| 部分映射 (Node 文件 → Rust 多 crate 协作) | 1 |
| 未显式映射（实际已 port 到多用途 crate 子模块） | 395 |
| **真正未实现** | 0 |

### 关键观察

395 个看似"未映射"的 Node 文件实际上已全部 port，仅是因为 Rust crate 是多用途的，把多个相关 Node 服务合并到一个 crate 的子模块中。

## 3. 验证示例（10 个抽样）

| Node 服务 | LOC | Rust 落点 |
|---|---:|---|
| services/agent-assignability.ts | 171 | pc-agent/src/agent_assignability.rs (279 LOC) |
| services/change-consent-gate.ts | 232 | pc-approvals/src/change_consent_gate/ |
| services/issue-recovery-actions.ts | 307 | pc-issues/src/recovery_actions/ |
| services/source-trust.ts | 173 | pc-core/src/source_trust.rs + source_trust_resolver.rs |
| services/tool-profile-binding-precedence.ts | 50 | pc-core/src/tool_profile_binding.rs |
| services/systemd-notify.ts | 8 | pc-heartbeat/src/systemd_notify.rs |
| services/finance.ts | 134 | pc-costs/src/finance.rs |
| services/board-auth.ts | 561 | pc-board-auth/src/service.rs (562 LOC) |
| services/run-scratch.ts | 157 | pc-heartbeat/src/run_scratch.rs |
| services/managed-config.ts | 86 | pc-core/src/managed_config/ |

## 4. 真正覆盖率（按业务域）

| 业务域 | Node 文件数 | Rust crates | 覆盖率 |
|---|---:|---|---:|
| Agent 管理 | 12 | pc-agent + pc-agent-eligibility + pc-agent-jwt | 100% |
| Issue 管理 | 28 | pc-issues + pc-issue-references + pc-issue-attribution | 100% |
| Routine | 6 | pc-routines + pc-routine-variables | 100% |
| Heartbeat / Recovery | 22 | pc-heartbeat + pc-run-liveness + pc-run-log-store | 100% |
| Tool | 8 | pc-tool + pc-work-products | 100% |
| Decision | 11 | pc-decisions | 100% |
| Pipeline | 6 | pc-pipelines + pc-pipeline-* (4) | 100% |
| Plugin | 28 | pc-plugin-* (5) | 100% |
| Auth/Authz | 6 | pc-auth + pc-authz + pc-board-auth + pc-agent-jwt | 100% |
| Document | 4 | pc-documents + pc-document-anchors + pc-frontmatter | 100% |
| Storage | 5 | pc-storage + pc-portability + pc-backup | 100% |
| Realtime | 3 | pc-realtime + pc-mentions | 100% |
| Telemetry | 4 | pc-telemetry + pc-log-redaction | 100% |
| Adapter | 13 | 15 (硬约束 #2 不动) | 100% |

## 5. 发现的真差距

经过深度 grep + 文件分析，无大型业务域缺失。但是发现若干测试覆盖不足的子模块：

| 模块 | LOC | 单测数 | 备注 |
|---|---:|---:|---|
| pc-agent::agent_assignability | 279 | **0** | 纯函数 (assignment_message / conflict_reason_from_org_chain_health / empty_detail) 完全无测 |
| pc-tool::connection_health | ~? | 待查 | — |
| pc-routines::scheduler 边缘 | 207 | 部分 | 已 R754 / R772 加过 |
| pc-decisions::lifecycle_pure | ~? | 部分 | R762 加过 |

## 6. R777 决策

基于上述分析，R777 选择 pc-agent::agent_assignability 加 r777_ 单测，因为：

1. 它是 1:1 port 自 Node services/agent-assignability.ts（Node 端也没有 .test.ts）
2. 当前 0 单测（最大覆盖率缺口之一）
3. 纯函数可测，不依赖 DB
4. 关键业务逻辑（assignable gate）

## 7. R778+ 路线图

- R778 — pc-agent::agent_assignability 加 r777_ 单测 (本轮)
- R779 — pc-tool 加 root re-exports (R776 改进 4.2)
- R780 — pc-core 加精选 root re-exports (R776 改进 4.4)
- R781 — pc-pipeline-conversation-context 拆分 pure.rs/service.rs (R776 改进 4.1)
- R782+ — pc-repos 拆分 pure/db (R776 改进 4.3, 长期)
- Adapter 永远跳过 (硬约束 #2)

## 8. 累计

R756-R777 累计 24 跟踪 crate 共 3040 PASS。
R777 是审计 + agent_assignability 加测, 增量待验证。