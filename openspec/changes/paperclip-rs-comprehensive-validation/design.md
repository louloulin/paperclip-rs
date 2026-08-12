# Design: 差距清零的实施设计

## 0. 进度基线（2026-08-12 独立核算）

| 域 | 权重 | 完成度 | 依据 |
|---|---|---|---|
| shared/ 契约 | 15% | 85% | 189 文件映射 pc-core + 跨 crate；M30 路由覆盖 100% |
| server/ 路由 | 25% | 92% | 56/56 模块全部有 Rust 对应；760 处 route()；G11 字节级差异未清零 |
| server/ middleware | 10% | 55% | 8/13 有实现；缺 compression/trust-proxy/private-hostname-guard/validate/http-log-policy/board-mutation-guard |
| server/ services | 15% | 70% | 193 非测试服务文件中 ~15 项缺 Rust 等价实现或仅桩 |
| server/ repos | 10% | 85% | pc-repos 25+ 子模块；部分领域服务未下沉（run-log-store 等） |
| UI client 接入 | 15% | 55% | V11 60/60 happy path 绿；M19 86.7%；复杂流程未验证 |
| CLI | 5% | 60% | 核心子命令可用；部分命令行为未对照 Node |
| 验证层 | 5% | 50% | e2e baseline 绿；long-run/perf 未重跑；rust-openapi.json 生成链路待修 |
| **加权总计** | 100% | **≈ 73%** | 不含适配器域（按用户指示） |

> 注：项目自评快照（R628）为 ~89%，其中适配器与轮次自评占比较高；本表按"核心域功能复刻深度"独立核算 ≈ 73%。

## 1. 差距清单（Node → Rust）

### 1.1 Middleware（7 项缺口）

| Node 文件 | Rust 现状 | 方案 |
|---|---|---|
| middleware/api-compression.ts | 缺 | pc-http::middleware::compression：tower-http CompressionLayer 包装，仅对可压缩响应启用 |
| middleware/trust-proxy.ts | 缺 | pc-http::middleware::trust_proxy：X-Forwarded-* 解析 + 信任白名单（纯函数 + 集成测试） |
| middleware/private-hostname-guard.ts | 缺 | pc-http::middleware::private_hostname_guard：Host 头校验（复用 pc-network-bind 常量） |
| middleware/validate.ts | 缺 | pc-http::middleware::validate：zod 语义 → 请求体/查询参数校验（对齐 error.rs 422 映射） |
| middleware/http-log-policy.ts | 缺 | pc-http::middleware::http_log_policy：日志采样/路径策略（对接 pc-log-redaction） |
| middleware/board-mutation-guard.ts | 缺 | pc-http::middleware::board_mutation_guard：board 变更保护（对照 node 语义） |
| middleware/error-handler.ts | 部分 | 扩展 error.rs 错误映射覆盖全部 ApiError 分支 |

### 1.2 Server services（15 项缺口，按依赖分组）

| 组 | Node 服务 | Rust 现状 | 方案 |
|---|---|---|---|
| 运行时 | run-continuations.ts | 缺 | pc-run-continuations（新 crate 或 pc-heartbeat 子模块）：run 续跑状态机 |
| 运行时 | run-log-store.ts | 缺 | pc-repos::run_log_store：run 日志存储查询（对接 pc-acpx sandbox_run_log_stream） |
| 运行时 | issue-liveness.ts | 缺 | pc-run-liveness 扩展：issue 存活检测 hook 对齐 |
| 协作 | invite-grants.ts | 缺 | pc-invite 扩展：invite grant 生命周期 |
| 运维 | hot-restart.ts | 缺 | pc-http::routes::dev_server_restart 扩展：完整热重启语义 |
| 策略 | tool-access-policy.ts | 缺 | pc-repos::tool 扩展：tool 访问策略表 |
| 收尾 | summary-slot-finalization.ts | 缺 | pc-http::routes::summary_slots 扩展：finalization 状态机 |
| 管道 | pipeline-case-outputs.ts / pipelines-aggregation.ts | 缺 | pc-pipelines 扩展：case outputs + 聚合查询 |
| 插件 | plugin-loader / plugin-job-coordinator / plugin-job-scheduler / plugin-managed-agents / plugin-managed-routines / plugin-managed-skills / plugin-secrets-handler / plugin-environment-driver | 缺/部分 | pc-plugin-host 扩展（loader + job 调度 + managed 资源），复用 pc-plugin-state-store |
| 环境 | environment-custom-image-runtime / environment-custom-image-setup-session-utils / environment-custom-image-terminal-sessions | 部分 | pc-repos::environment + pc-realtime 扩展：custom image 运行时与终端会话 |

### 1.3 UI 接入（4 项）

| 项 | 现状 | 目标 |
|---|---|---|
| rust-openapi.json 生成链路 | .route-audit 中 paths 为空（spec 生成异常） | 修复生成链路，paths 全量输出 |
| M19 UI↔OpenAPI 覆盖 | 86.7% | 100%（新增缺失 UI 路径注册） |
| V11 UI 60 client happy path | 60/60 绿 | 保持全绿（每轮回归） |
| 复杂流程 | 未验证 | terminal WS / settings / plugins / heartbeat 状态 UI 全栈验证 |

## 2. 架构决策

- **保持现有分层**：pc-http（路由+middleware）→ pc-repos（数据）→ pc-core（共享），不新建重复 crate；缺口实现优先下沉到现有 crate 子模块（高内聚低耦合）。
- **middleware 全部落在 pc-http::middleware**，与现有 access_log/auth/body_limit/cors/csrf/redaction/request_id/stack 同构，每个新 middleware 附纯函数单测 + 集成测试。
- **services 缺口**按领域归属就近实现：运行时的进 pc-heartbeat/pc-run-liveness；协作的进 pc-invite；插件的进 pc-plugin-host；环境的进 pc-repos::environment + pc-realtime。
- **验证协议**：每轮 = 实现 → 单测 → 契约测试（sqlx::test!）→ 全栈脚本（v11-ui-happy-path / e2e-baseline）→ evidence 文档（r###.md）→ 更新 progress-snapshot.md。
- **UI 不改源码**：仅通过 VITE_API_BASE / vite proxy 指向 Rust server；OpenAPI 以 Node openapi.ts 为基准做对齐注册。

## 3. 数据流（示例：middleware 栈）

```
client → trust_proxy → request_id → access_log → auth(actor) → csrf → body_limit → validate → route
                                          └─ redaction / http_log_policy（出站日志改写）
```

## 4. 风险

- 插件 job 调度与 Node worker 语义差异大：以 plugin-protocol 类型 + 互操作测试兜底（P2，不阻塞核心）。
- OpenAPI 扫描（源码扫描注入 600+ 路径）可能漏 UI 调用路径：以 check-ui-openapi.py 为回归闸门。
- /tmp 在受限环境不可用已修复（plugin_install_guard 测试改为 temp_dir）；后续测试禁用硬编码 /tmp。
*** End Patch
