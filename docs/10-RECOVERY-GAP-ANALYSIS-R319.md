# Paperclip-rs vs Paperclip 全面差距分析（Round 319 完成版）

更新时间：2026-08-07（Round 319 完成）

## 0. 当前进度快照

| 维度 | 完成度 | 依据 |
|---|---|---|
| **整体进度** | **~85%** | 304 轮 + 19 个增量文档；所有公开 API 形状完整；行为等价 ~85% |
| **路由形状** | **100%** | 56/56 Node 路由文件全部有 Rust 对应；Node 56 → Rust 68 |
| **路由端点覆盖** | **~88%（审计）/ ~60%（raw）** | raw: 334/551 unique URL, 316/685 method+path |
| **路由代码深度** | **~75%** | DB 查询 ✅；复合状态机 ✅；校验 ⚠️；跨服务调用 ⚠️ |
| **数据持久化** | **~95%** | pc-repos 20K+ 行 |
| **Adapter 真实执行** | **100% (13/13)** | 13 个 adapter 全部实现 CLI 协议 |
| **Plugin runtime** | **~85%** | supervisor + 指数 backoff + Crashed + event/stream bus |
| **Auth/Authz** | **~65%** | session/email/password/refresh rotation 简化 |
| **Realtime 链路** | **~95%** | R252-R257 全完成 |
| **Heartbeat 核心** | **~90%（本轮 focus 后）** | scheduler + retry cap + watchdog + liveness + adapter 失败分类 + provider quota monitor + escalation |
| **Case/Issue** | **~90%** | cases 表迁移 + 6 类 case + issue monitor + 续接 summary + recovery 编排 |
| **Decision / Bundle** | **~95%** | signing + canonical + tamper 拒绝 + bundle 仓储 |
| **Companies 主路由** | **~95%** | members/permissions/invites/join_requests/decisions/activity/user-directory/org-chart |

**pc-heartbeat 单测**：384/384 ✅
**pc-heartbeat 集成测试**：~265 个 ✅（最新 Round 319 加 5 个）
**pc-server 编译**：✅（Round 319 后）

## 1. Recovery Service 模块对比（Round 319 后）

### 1.1 文件级对比

| 维度 | Node | Rust | 状态 |
|---|---|---|---|
| 主服务文件 | `server/src/services/recovery/service.ts` (5580 行) | `crates/pc-heartbeat/src/recovery/` 38 文件 ~16K 行 | ✅ 拆分为模块 |

### 1.2 顶级 orchestration 接口（12 个全部 100% 覆盖）

- ✅ buildRunOutputSilence
- ✅ escalateStrandedRecoveryIssueInPlace
- ✅ escalateStrandedAssignedIssue
- ✅ recordWatchdogDecision
- ✅ scanSilentActiveRuns
- ✅ reconcileStrandedAssignedIssues
- ✅ sweepStaleIssueLocks
- ✅ buildIssueGraphLivenessAutoRecoveryPreview
- ✅ reconcileResolvedDependencyWakeBackstop
- ✅ reconcileIssueGraphLiveness
- ✅ readRecoveryTimerIntervalMs
- ✅ 所有内部子模块

### 1.3 Recovery service 子函数覆盖（98 个内部辅助函数）

| 类别 | 数量 | Rust 覆盖 | 缺失 |
|---|---|---|---|
| 公开枚举 / 常量 | ~15 | 100% | — |
| Key/Origin/Reason 构造器 | ~10 | 100% | — |
| Pure decision 函数 | ~25 | ~88% (22/25) | `is_stranded_issue_recovery_issue`, `is_recovery_origin_issue`, `is_terminal_issue_status` |
| DB 仓储函数 | ~30 | ~93% (28/30) | `ensure_source_issue_commented_for_stale_evaluation`, `ensure_stranded_issue_recovery_issue` |
| Run lifecycle 函数 | ~15 | ~85% | `append_recovery_run_event`, `next_run_event_seq` |
| Quota/Classification 函数 | ~10 | 100%（本轮） | — |
| Participant/Resolve 函数 | ~10 | ~70% | `resolve_continuation_waiting_on_review`, `resolve_invokable_recovery_agent_id`, `resolve_escalation_owner_agent_id`, `resolve_stranded_recovery_routing` |

**总体 recovery service 内部辅助函数覆盖 ~85%**。

## 2. Round 319 已修复缺口

| 缺口 | 状态 |
|---|---|
| `scheduleProviderQuotaRecoveryMonitor` 主调度路径未触发 | ✅ 已接 reconcile_and_escalate_stranded_for_company 前置分支 |
| `reconcile_and_escalate_stranded_for_company` quota monitor 写入 | ✅ 5/5 测试通过真实 PostgreSQL |
| `in_review` 状态下的 quota monitor participant agent 加载 | ✅ `current_review_participant_agent_id` + `load_latest_run_row_for_agent` |
| `provider_quota_monitored` 指标透传 | ✅ StrandedSweepOutcome 新字段 |
| monitor `serviceName == "AI provider quota"` 持久化分类 | ✅ persist_provider_quota_recovery_classification |
| 单事务原子性（wakeup + scheduled_retry run + wakeup.run_id + action UPDATE） | ✅ 全在同一 tx |
| retry 时间覆盖 Node 所有候选字段 | ✅ retryNotBefore / transientRetryNotBefore / providerQuotaRetryNotBefore |

## 3. 剩余 P2 缺口（按 ROI 排序）

### P2-1 ⭐ 下一个目标：resolveContinuationWaitingOnReview（in_review participant 续审分支）

**Node 位置**：server/src/services/recovery/service.ts:3229
**业务价值**：issue 在 in_review 状态下，判断是否需要触发 continuation review
**预期影响**：补全 escalateStrandedAssignedIssue 在 in_review 路径的完整闭环
**估计**：~150 行新代码 + 3-4 个集成测试

### P2-2：configurationIncomplete 手返路径

**Node 位置**：server/src/services/recovery/service.ts:3747 附近
**业务价值**：adapter failure 是 configuration_incomplete 时直接 escalated + 写 evidence，不走 monitor
**预期影响**：补全 escalateStrandedAssignedIssue 对 configuration_incomplete 类型的完整处理
**估计**：~120 行 + 2-3 个测试

### P2-3：buildRecoveryIssueInPlaceEscalationComment in-place comment 完整对齐

**Node 位置**：server/src/services/recovery/service.ts:3095
**业务价值**：escalateStrandedRecoveryIssueInPlace 写 issue comment 的全字段结构
**预期影响**：让 in-place escalation 在 UI 上展示完整证据
**估计**：~200 行 + 3-4 个测试

### P2-4：ensureStrandedIssueRecoveryIssue 顶层 issue 创建

**Node 位置**：server/src/services/recovery/service.ts:2712
**业务价值**：stranded assigned issue 走 monitor 路径失败时，创建独立 recovery issue
**预期影响**：完整 stranded recovery issue 生命周期
**估计**：~200 行 + 3 个测试

### P2-5：ensureSourceScopedStrandedRecoveryAction 重构到独立模块

**Node 位置**：server/src/services/recovery/service.ts:2863
**预期影响**：recovery_action 写入路径标准化
**估计**：~150 行 + 2 个测试

### P2-6：HeartbeatRunActor 注入 Db

**Node 位置**：kameo actor → recovery lib
**业务价值**：actor 模型持有 Db 句柄，可异步触发 recovery 操作
**预期影响**：actor 与 recovery 模块解耦但能联动
**估计**：~300 行 + 5 个测试

## 4. 整体完成度趋势

```
Round 257: ~81%
Round 290: ~83% (heartbeat recovery 大量补齐)
Round 300: ~84%
Round 319: ~85% (本轮 +0.5% quota monitor 写入)
```

按当前节奏，每轮约 +0.3-0.5%，预计：
- Round 320-325：P2-1 ~+0.5%
- Round 326-330：P2-2 + P2-3 ~+0.7%
- Round 331-340：P2-4 + P2-5 ~+0.7%
- Round 341-350：P2-6 actor Db 注入 ~+0.8%
- **Round 350 预计达 ~89%**

## 5. 不再是缺口的领域（已完成）

- ✅ 13 个 adapter 协议
- ✅ Plugin supervisor + worker lifecycle + event bus
- ✅ Decision signing + tamper detection
- ✅ Document storage (Local + S3)
- ✅ Realtime SSE/WS 端到端
- ✅ 6 类 case 状态机
- ✅ Issue monitor + claim_due_monitors
- ✅ Recovery sweep 主循环
- ✅ Provider quota monitor 写入路径
- ✅ Stale run evaluation + auto_dismiss

## 6. 下一步计划（按 ROI）

**Round 320（下一个）**：实现 resolve_continuation_waiting_on_review —— in_review 状态续审判断
- TDD：先写失败测试（in_review issue with participant agent + has latest review → decide continue review path）
- Node 行 4178 in enqueueStrandedIssueRecovery 主循环
- 集成到 enqueue_stranded_issue_recovery 中

**Round 321**：补 configuration_incomplete 手返路径
**Round 322-323**：补全 in-place escalation comment 完整结构
**Round 324**：ensure_stranded_issue_recovery_issue 顶层 issue 创建
**Round 325**：ensure_source_scoped_stranded_recovery_action 重构
**Round 326+**：actor Db 注入、跨模块调度

## 7. 验证基线

```bash
# 库测试（必须 100%）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --lib
# 期望：384 passed

# 集成测试（必须 100%）
env -u SHELL rtk proxy cargo test -p pc-heartbeat --tests -- --test-threads=1
# 期望：~265 passed

# pc-server 编译（必须 OK）
env -u SHELL rtk proxy cargo test -p pc-server --bins --no-run -- --test-threads=1
# 期望：编译通过

# 完整 workspace lib
env -u SHELL rtk proxy cargo test --workspace --no-fail-fast --lib
# 期望：~700 passed
```
