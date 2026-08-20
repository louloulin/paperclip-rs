---
change: paperclip-rs-comprehensive-validation
design-doc: docs/superpowers/specs/2026-08-20-paperclip-rs-comprehensive-validation-design.md
base-ref: f8d801ee1e1fb5e143617ce5464e1c1816fbb978
---

# paperclip-rs 全面验证与模块补齐 — 实施计划

> 配套 Design Doc `docs/superpowers/specs/2026-08-20-paperclip-rs-comprehensive-validation-design.md`
> 配套 tasks.md `openspec/changes/paperclip-rs-comprehensive-validation/tasks.md`

## 概述

按 tasks.md 的 12 组任务顺序执行，每组作为一个 phase。每个 phase 内按 round 粒度提交（每 round = 一个 commit）。每 round 完成后立即跑 `cargo test --workspace --lib` 全 PASS 才能进入下一 round。

## 执行阶段

### Phase 1 — 基础设施（task 1.x）
- 1.1 `scripts/parity-check.sh`
- 1.2 CI workflow
- 1.3 parity gap report

### Phase 2 — 业务逻辑层补齐 R719-R740（task 2.x）

22 个 round，每个 round 一个模块，pure helpers 实现 + 单测。详见 design doc §2.1 表格。

每个 round 模板：
1. `Read paperclip/server/src/services/<name>.ts` 找 pure helpers
2. `paperclip-rs/crates/pc-<name>/src/pure.rs` 镜像实现
3. 5-15 个单测
4. `cargo test -p pc-<name> --lib pure` 全 PASS
5. `cargo test --workspace --lib` 全 PASS
6. git commit 单 round

### Phase 3 — 跨 crate 整合（task 3.x）

8 个 round，每个 round 一个整合路径 + 集成测试。

### Phase 4 — 远程 execution（task 4.x）

新 crate `pc-execution`：SSH bridge + restore workspace + materialize claude config + 集成测试。

### Phase 5 — UI 真实 happy path（task 5.x）

Playwright + 60 client 剧本 + 自动截图。

### Phase 6 — 性能基线（task 6.x）

criterion benches + long-run + perf-baseline.md。

### Phase 7 — 迁移 + 文档（task 7.x + 9.x）

迁移注释 + diff report + rollback verify + RUNBOOK/TROUBLESHOOTING/FAQ/perf-baseline。

### Phase 8 — Workflow + Cron + Plugin 端到端（task 10.x + 8.x）

pc-cron + pc-workflow + pc-plugin-protocol 集成测试。

### Phase 9 — 验证 + 收尾（task 11.x + 12.x）

全 PASS 验证 + parity ≥95% + commit + tag。

## 依赖关系

```
Phase 1 → Phase 2 → Phase 3 → Phase 4
                    ↓          ↓
                  Phase 5 → Phase 6 → Phase 7 → Phase 8 → Phase 9
```

## 验证策略

每个 phase 完成后：
- `cargo test --workspace --lib` 全 PASS
- `cargo fmt --all --check` 无 diff
- `cargo clippy --workspace -- -D warnings` 无 warning
- e2e baseline 通过（仅 Phase 5+ 需要）

每个 commit：
- 改动文件 < 500 行
- commit message 说明本 round 范围

## 关键风险

- 大量 round 回归风险 → 每 round 单独 commit + 立即跑 workspace test
- ssh2 FFI 跨平台 → trait 抽象 + 双实现
- Playwright CI 时间 → 并行 + 单浏览器
- 中文文档维护 → OpenSpec proposal 强制覆盖

## 不变量

- HTTP 契约冻结
- 数据库 schema 兼容
- Workspace 级别 `forbid(unsafe_code)`
- Pure function 不依赖全局环境
- 错误传播 `Result<T, E>` + `?` 操作符

## 完成标准

- 所有 12 组 tasks 全部勾选
- parity-check.sh 报告 ≥ 95% 覆盖
- 全 workspace test ≥ 8000 PASS
- e2e baseline PASS
- perf-baseline.md 完成
- 中文文档齐