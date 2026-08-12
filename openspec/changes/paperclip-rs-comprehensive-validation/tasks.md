# Tasks: 核心模块与 UI 差距清零

## Round 计划（R635 起，每轮含实现+验证+evidence）

- [x] **R635 middleware 补齐 batch 1**：compression / trust-proxy / private-hostname-guard / http-log-policy 四个 middleware（纯函数单测 + 集成测试 + 注册进 stack）— evidence/r635-middleware-compression-trust-proxy-hostname-guard.md；92 middleware + 451 lib 测试绿
- [x] **R636 middleware 补齐 batch 2**：validate / board-mutation-guard + error-handler 全分支（Node 形状 HttpError + skill_policy_denied 脱敏 + structured connection 字段展开 + Zod 形态）；evidence/r636-middleware-validate-board-mutation-error-handler.md；pc-http 473 lib 测试绿；pc-server 编译通过
- [ ] **R637 运行时服务 batch 1**：run-continuations / run-log-store / issue-liveness（pc-heartbeat + pc-run-liveness + pc-repos 扩展）
- [ ] **R638 协作与策略**：invite-grants / hot-restart 完整语义 / tool-access-policy（pc-invite + pc-http + pc-repos 扩展）
- [ ] **R639 收尾与管道**：summary-slot-finalization / pipeline-case-outputs / pipelines-aggregation（pc-http::routes::summary_slots + pc-pipelines 扩展）
- [ ] **R640 插件内部 batch 1**：plugin-loader / plugin-job-coordinator / plugin-job-scheduler（pc-plugin-host 扩展，JSON-RPC 契约测试）
- [ ] **R641 插件内部 batch 2**：plugin-managed-agents / plugin-managed-routines / plugin-managed-skills / plugin-secrets-handler / plugin-environment-driver（复用 pc-plugin-state-store）
- [ ] **R642 环境运行时**：environment-custom-image-runtime / setup-session-utils / terminal-sessions 完整链路（pc-repos::environment + pc-realtime + terminal WS 全栈）
- [ ] **R643 OpenAPI 生成链路修复**：rust-openapi.json paths 全量输出 + M19 UI↔OpenAPI 对齐到 100%（check-ui-openapi.py 全绿）
- [ ] **R644 UI 复杂流程全栈验证**：terminal WS / settings / plugins / heartbeat 状态 UI → Rust server 真实剧本（v11-ui-happy-path 保持 60/60）
- [ ] **R645 回归与收尾**：workspace 全量单测绿、e2e-baseline、long-run-5min、perf-baseline 重跑 + MODULE-MAPPING/PROJECT-PLAN/progress-snapshot 更新 + evidence 归档

## 验收标准

- 每轮 evidence 文档存在（openspec/changes/paperclip-rs-comprehensive-validation/evidence/r###-*.md）
- 每轮新增单测全绿；workspace 全量 lib 测试 0 failed
- R643 后 M19 UI↔OpenAPI 覆盖 = 100%
- R644 后 V11 60 client happy path = 60/60；Playwright v12 full-flow 通过
- R645 后 MODULE-MAPPING 与 progress-snapshot 更新到最新轮次
