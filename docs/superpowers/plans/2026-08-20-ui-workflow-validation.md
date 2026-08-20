---
change: ui-workflow-validation
design-doc: docs/superpowers/specs/2026-08-20-ui-workflow-validation-design.md
base-ref: 468004a8b59d3fcfff99fac0d455cdd83b96fcec
---

# ui-workflow-validation — 实施计划

> 配套 Design Doc `docs/superpowers/specs/2026-08-20-ui-workflow-validation-design.md`
> 配套 tasks.md `openspec/changes/ui-workflow-validation/tasks.md`

## 概述

按 tasks.md 的 16 组任务顺序执行，每组作为一个 phase。每个 phase 内按 round 粒度提交。

## 执行阶段

### Phase 1 — pc-server bootstrap（task 1.x）
- 1.1 `--seed-demo` CLI flag
- 1.2 `seed_demo()` 实现
- 1.3 demo UUID 命名空间
- 1.4 idempotent
- 1.5 真实启动验证

### Phase 2-12 — 11 个 workflow（task 2.x-12.x）
每个 workflow 一个 round：编写 spec → 启动 + 跑 → evidence 收集

P0 优先级先做：
- Phase 2: auth-flow
- Phase 3: companies-flow
- Phase 5: issues-flow

P1 其次：
- Phase 4: agents-flow
- Phase 6: pipelines-flow
- Phase 7: decisions-flow
- Phase 9: projects-flow

P2 最后：
- Phase 8: routines-flow
- Phase 10: portability-flow
- Phase 11: inbox-flow
- Phase 12: realtime-flow

### Phase 13 — Reporter（task 13.x）
evidence 统一收集 + summary.md

### Phase 14 — 启动 + CI（task 14.x）
- run-ui-workflow-e2e.sh
- GitHub Actions nightly

### Phase 15-16 — 文档 + 收尾
- UI-WORKFLOWS.md
- 全 PASS + commit + tag

## 依赖关系

```
Phase 1 (seed) → Phase 2-12 (workflows, 可并行)
              → Phase 13 (reporter, 收集所有 workflow 输出)
              → Phase 14 (启动脚本, 依赖 Phase 13)
              → Phase 15-16 (文档 + 收尾, 依赖 Phase 14)
```

## 验证策略

每个 phase 完成后：
- pc-server 可真实启动（cargo build + 真启动 + curl /health）
- Playwright spec 可真实跑通
- evidence 文件生成
- 全 workspace test 无回归

## 关键风险

- pc-server 启动慢 → 复用 R579 启动计时
- UI 端字段缺失 → openapi spec 对比
- WebSocket flaky → 5s 超时 + 重试
- Seed 冲突 → UUID 命名空间隔离

## 不变量

- HTTP 契约冻结
- 数据库 schema 兼容
- `forbid(unsafe_code)`
- e2e 用隔离 PG schema

## 完成标准

- 所有 16 组 tasks 全部勾选
- 11 个 workflow 全 PASS
- 全 workspace test 无回归
- evidence 完整
- nightly CI 配置
- UI-WORKFLOWS.md 完成