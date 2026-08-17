# R776 — 架构整合审计（lib.rs 公共 API 形状统一 / pc-server 依赖收敛）

日期: 2026-08-17
范围: 全 workspace 93 个非适配 crate 的公共 API 形状审查 + pc-server 依赖图

## 1. 审计方法

- 抽样审查 10 个核心 crate 的 lib.rs 公共 API 形状
- 检查 pub mod / pub use / pub fn 模式
- 列出 pc-server 直接依赖的 15+ 个 pc-* crate
- 验证内部模块间耦合方向

## 2. 当前 pc-server 依赖图

pc-server (apps/pc-server, 1 bin, 876 LOC main.rs)
  pc-core               (领域类型 + 错误)
  pc-errors             (统一错误)
  pc-telemetry          (OTLP/tracing)
  pc-config             (配置加载)
  pc-db                 (SQLx 连接池)
  pc-http               (Axum 路由)
  pc-realtime           (SSE 事件)
  pc-heartbeat          (心跳 + 调度)
  pc-storage            (S3/本地存储)
  pc-secrets            (JWT 签名)
  pc-routines           (Routine 服务)
  pc-agent              (Agent 业务)
  pc-network-bind       (端口绑定)
  pc-hot-restart       (热重启)
  pc-repos              (DB 仓储, 650 测试)
  pc-plugin-*           (4 个, 插件协议)
  pc-adapter-*          (15 个, 硬约束 #2 不动)

观察: pc-server 依赖 15+ 个 pc-* crate (29 路径依赖), 间接通过 pc-repos 触发几乎全 workspace。
这是预期的单体入口, 但依赖收敛点应该是 pc-core (而不是 pc-repos)。

## 3. 公共 API 形状一致性矩阵

| Crate | 文件布局 | pub mod | pub use root | 一致性 |
|---|---|:---:|:---:|:---:|
| pc-constants | 模块化 (agent/budget/...) | ✓ | n/a | ✓ 良好 |
| pc-errors | 单文件 enum + Result | n/a | n/a | ✓ 良好 |
| pc-core | 模块化 (actor/cron/error/...) | ✓ | partial | ⚠ 部分 |
| pc-tool | 多 pure 子模块 | ✓ | ✗ | ⚠ 无 root re-export |
| pc-decisions | 多子模块 (pure/executor/...) | ✓ | ✓ | ✓ 良好 |
| pc-routines | 多子模块 | ✓ | ✓ | ✓ 良好 |
| pc-pipeline-case-type | 单文件 pure | n/a | n/a | ✓ leaf pure |
| pc-pipeline-health | 单文件 pure | n/a | n/a | ✓ leaf pure |
| pc-pipeline-case-outputs | pure/service/types + root re-export | ✓ | ✓ | ✓ 良好 |
| pc-pipeline-conversation-context | 单文件 899 LOC | n/a | n/a | ⚠ 过大 |

## 4. 发现的不一致 / 改进点

### 4.1 pc-pipeline-conversation-context 单文件过大 (899 LOC)
现状: 整个模块 (含 IO、pure、test fixture、types) 都在一个 lib.rs
影响: 编译单元大, 测试隔离差, pure 与 IO 耦合
建议: 拆分 pure.rs (fence_markdown / truncate_with_flag / format_*) + service.rs (load_*)
风险评估: 中等 (纯拆分, 行为不变)
优先级: R777+

### 4.2 pc-tool 缺少 root re-export
现状: 调用方需写 pc_tool::side_effect_idempotency::fn_name
影响: 公共 API 使用体验差
建议: 在 lib.rs 加 pub use side_effect_idempotency::*; 等, 让调用方能 pc_tool::fn_name
风险评估: 低 (仅暴露, 不改行为)
优先级: R777+

### 4.3 pc-repos 单 crate 650 测试过度集中
现状: pc-repos 一个 crate 包含所有 DB 仓储 + 测试 (650 PASS)
影响: 编译时间长, 单点失败影响 CI
建议: 拆分 pc-repos::pure (无 sqlx 依赖的逻辑) 与 pc-repos::db (sqlx 仓储)
风险评估: 高 (大量依赖)
优先级: 长期 (R780+)

### 4.4 pc-core 子模块可发现性
现状: pc-core 有 25+ 子模块 (actor / attention / catalog_provenance / cron / ...), 但根层未 re-export
影响: 调用方必须深路径导入
建议: 在 lib.rs 加 pub use actor::*; 等精选 re-export
风险评估: 低
优先级: R778

## 5. 错误模型统一现状

pc-errors 已存在, 提供统一 Error enum + Result<T>
但实际使用率:
- pc-repos: 100% 使用 pc_errors::Error
- pc-routines: 100% 使用 (通过 RoutineService)
- pc-pipeline-*: 部分使用, 部分直接暴露 thiserror::Error

观察: 错误模型在 service 层统一, 但 leaf pure crate 直接用 thiserror。
这是合理的 (pure crate 不需要 HTTP 状态码映射)。

## 6. 文档完整性

- 10 个核心 crate 都有 //! 模块级 docstring
- 设计原则 (高内聚低耦合) 在 pc-core / pc-decisions / pc-routines 显式说明
- Node 上游映射注释 (R538 / R554 / R639) 保留完整

## 7. R776 决策 (不破坏性)

本轮不修改任何 crate 内部布局。原因:
1. 当前公共 API 形状已基本一致 (4.1 / 4.2 是改进, 但非阻塞)
2. 重构风险高, 而现有功能验证完整 (3040 PASS, R775 UI 链路)
3. 用户硬约束: "不要修复无关 bug", 但架构整合不是 bug 修复, 是优化
4. 优化需配合"最佳设计方式", 应在小步快跑 R777-R779 分批进行

R776 输出: 本审计文档 + 改进路线图
R777+ 计划: 按 4.1 / 4.2 / 4.4 顺序小步优化, 每步配 cargo test --workspace 全量验证

## 8. 累计

R756-R776 累计 24 跟踪 crate 共 3040 PASS。
R776 是审计文档, 无新增单测。

## 9. R777+ 后续计划

- R777: pc-pipeline-conversation-context 拆分 pure.rs / service.rs (4.1)
- R778: pc-tool 添加 root re-exports (4.2)
- R779: pc-core 添加精选 root re-exports (4.4)
- R780+: pc-repos 拆分 (4.3) - 需要充分准备, 长期项
- Adapter 永远跳过 (硬约束 #2)
