# Paperclip-rs 全面差距分析与后续计划（2026-08-06）

更新时间：2026-08-06（Round 257 完成后）

## 一、当前完成度快照

| 维度 | 完成度 | 依据 |
|---|---|---|
| **整体进度** | **~81%** | 路由端点 88%（审计）/ 53%（raw）/ 100%（形状） |
| **路由形状** | **100%** | 56/56 Node 路由文件全部有 Rust 对应 |
| **数据持久化** | **~90%** | pc-repos ~20K 行；invite/join_request/company_member/decision_bundle/permission_grant 全部仓储化 |
| **Adapter 协议** | **100% (13/13)** | 11 个 adapter 实现 CLI 协议（4 个 stub 但 args + JSONL parser 完整） |
| **Plugin runtime** | **~80%** | supervisor + 指数 backoff + Crashed 状态机 + event bus + stream bus + config validator |
| **Auth/Authz** | **~55%** | session/email/password/refresh rotation 简化 |
| **Realtime 链路** | **~95%** | R252-R257 完成 subscriber/channel filter/rate limit/since-until/replay/stats |
| **Heartbeat 核心** | **~75%** | scheduler + retry cap + watchdog 决策；**缺少依赖 readiness / staleness recovery / 幂等合并 / 抑制 DB override** |
| **Case/Issue** | **~85%** | cases 表迁移 + 6 类 case + issue monitor + 续接 summary |
| **Decision / Bundle** | **~95%** | signing + canonical + tamper 拒绝 + bundle 仓储 |
| **Companies 主路由** | **~90%** | members/permissions/invites/join_requests/decisions/activity/user-directory/org-chart 全部仓储化 |

## 二、已完成核心模块（最近 50 轮）

### Realtime 链路（R252-R257，全完成）
- R252: Subscriber trait + ChannelFilter + SSE endpoint
- R253: task-watchdog capability classifier
- R254: per-resource channel filter (issue_id/watchdog_id/agent_id/run_id)
- R255: per-IP token bucket + per-company connection limit + IP extraction
- R256: since/until 时间窗口过滤（WS + SSE 双覆盖）
- R257: replay 阶段 since/until 过滤 + `/api/realtime/stats` 端点

### Companies 仓储化（R88-R93）
- R88: invite + join_request 模块化
- R89: company_member 模块化（修复 4 个隐藏 bug：表名/列名错误）
- R90-R91: principal_permission_grant 模块化（修复 100% 命中 500 bug）
- R92: decision_bundle 仓储化（11 个集成测试）
- R93: audit/org/search/agents 子块仓储化（修复 4 个隐藏 bug）

### Cases / Issues / Routines
- 全部仓储化 + 6 类 case 状态机迁移
- 14 个 issue stub 化端点（R96）
- 11 个 tool-gateway/adapter/workspace authz stub 化（R97）

### 关键基础设施
- catalog_provenance（16 项白名单 + canonical + sourceRef/originHash）
- decision_signing（HMAC-SHA256 + canonical JSON + atomic hard-link + tamper 拒绝）
- home_paths（11 项路径规则）
- tool_profile_binding（6 target precedence + 3 键稳定排序）
- portability_fidelity（10 count 字段 + warning 构造器）
- tool_content_guards（4 个 prompt injection regex + sign/verify）
- plugin_stream_bus / plugin_event_bus / plugin_config_validator

## 三、主要剩余差距（按优先级）

### P0 — 系统核心可靠性
| 模块 | 缺口 | 影响 |
|---|---|---|
| heartbeat 依赖 readiness | 心跳执行前未检查前置条件（adapter 可用、worktree 干净、issue lock 等） | 减少 flaky run |
| heartbeat staleness recovery | 长时间无心跳的 run 未自动恢复/标记 | 后台调度可靠性 |
| heartbeat 幂等/wakeup 合并 | 多次 wake 同一 run 未去重 | 资源浪费 |
| heartbeat 抑制 DB override | 仅 env var 抑制，缺 DB 表行级 override | 多租户场景失效 |
| 其他 retry reason | 已实现 transient_failure / max_turns_continuation；缺 `dependency_unavailable` / `workspace_locked` / `quota_exceeded` 等 | 业务场景覆盖不全 |

### P1 — 用户面核心
| 模块 | 缺口 | 影响 |
|---|---|---|
| company-skills 深度 | routes 100% / 仓储 70% | 文件版本管理、test-run、fork 流程不完整 |
| tools/tool-connections | routes 100% / OAuth + 真实调用 60% | agent 调用外部工具受限 |
| plugin worker→host 回调 | supervisor 已迁移，worker→host 回调 + 生命周期恢复未完整 | 插件双向通信 |
| decisions/decision-training | decision_bundles 已迁移，decision-training 80% | 决策训练数据流不完整 |
| secrets 真实解密 | provider descriptor 已完整，AWS/GCP/Vault 真实解密未完整 | 远端密钥不可用 |

### P2 — 辅助功能
| 模块 | 缺口 |
|---|---|
| folders / labels 完整迁移 | 已部分迁移 |
| approvals / recovery-actions | routes 100%，仓储 60% |
| routines / pipelines 深度 | 已迁移主体 |
| cli auth bridge | 简化实现 |
| UI e2e 冒烟 | 未启动 |

## 四、下一阶段计划（10 轮内推到 90%）

### 轮次 258 — heartbeat 依赖 readiness 与 staleness recovery（**下一个核心模块**）

**目标**：复刻 Node `services/heartbeat.ts` 中 scheduler 在 claim 之前的 readiness 评估逻辑，确保心跳执行前所有前置条件都被验证。

**范围**：
1. **`crates/pc-heartbeat/src/readiness.rs`**（新模块）：
   - `ReadinessCheck` 枚举：`AdapterAvailable | WorktreeClean | IssueLockAvailable | DependenciesResolved | BudgetAvailable | SuppressionCleared`
   - `ReadinessReport` 结构：列出通过/失败/阻塞原因
   - `evaluate_readiness(agent, run, environment)` —— 串行检查所有前置条件
   - `is_stale(last_heartbeat_at, now, threshold)` —— staleness 判定
   - `recover_stale_run(run_id)` —— 恢复/标记策略

2. **`crates/pc-heartbeat/src/lib.rs`**：
   - `spawn_heartbeat_supervisor` 在 tick 循环中调用 `evaluate_readiness` + `is_stale`
   - readiness 失败的 run 不被 claim，进入 `waiting_for_readiness` 状态
   - stale run 自动恢复或标记 `stale_abandoned`

3. **仓储层扩展**：`HeartbeatRepo::mark_waiting_for_readiness` / `HeartbeatRepo::recover_stale_runs`

4. **测试**：
   - `readiness::*` 5 个单测
   - 集成测试 `pc-heartbeat readiness_contract`

### 轮次 259 — heartbeat 幂等/wakeup 合并与抑制 DB override

**目标**：防止重复 wake、引入 DB 级抑制覆盖。

### 轮次 260 — 其他 retry reason（dependency_unavailable / workspace_locked / quota_exceeded）

### 轮次 261-263 — company-skills 深度（version 管理 / fork 流程 / test-run 状态机）

### 轮次 264-266 — tools/tool-connections 真实 OAuth 流程

### 轮次 267 — decisions/decision-training 仓储化

### 轮次 268-270 — secrets 真实解密（AWS/GCP/Vault）

### 轮次 271-272 — plugin worker→host 回调 + 生命周期恢复

### 轮次 273-275 — UI e2e 冒烟 + Phase G 切流量

**预期**：10 轮内推到 **≥ 90%**，再 2-3 轮推到 e2e 冒烟通过。

## 五、本轮（Round 258）执行目标

聚焦 heartbeat **依赖 readiness** 与 **staleness recovery** 两大 P0 缺口：
- 复刻 Node `services/heartbeat.ts` 中的 readiness pipeline
- 在 scheduler 中集成 readiness 评估
- 新增 stale run 恢复策略
- 完整单测 + 集成测试覆盖

