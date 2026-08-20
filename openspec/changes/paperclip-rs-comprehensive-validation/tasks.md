## 1. 模块覆盖度自动化 (module-parity-validation)

- [ ] 1.1 创建 `scripts/parity-check.sh` 扫描 `paperclip/server/src/` 和 `paperclip-rs/crates/`，输出 Node vs Rust 模块覆盖率
- [ ] 1.2 添加 CI workflow 每周一自动跑 parity check，结果写入 `docs/parity-trend.md`
- [ ] 1.3 实现 parity gap report：列出未 port 的 Node 服务 + LOC + 关键依赖

## 2. 业务逻辑 pure helpers 复刻 (R719-R740)

- [ ] 2.1 R719 — pc-inbox::agent_policy::pure (validate_allowlist, mode_handling)
- [ ] 2.2 R720 — pc-projects::operations::pure (operations helpers)
- [ ] 2.3 R721 — pc-environments::pure (environment config validation)
- [ ] 2.4 R722 — pc-tool::rbac::pure + pc-tool::secrets::pure
- [ ] 2.5 R723 — pc-decision-training::pure (decision training business logic)
- [ ] 2.6 R724 — pc-pipelines::pure (pipeline stage transitions)
- [x] 2.7 R725 — pc-goals::pure (default selection logic) [commit 468004a, 9 tests]
- [x] 2.8 R726 — pc-issue-tree-control::pure (subtree computation) [commit 50e0cea, 11 tests + pc-tool delete fix]
- [x] 2.9 R727 — pc-portability::pure (export/import validation) [53 tests PASS]
- [x] 2.10 R728 — pc-feedback-share::pure + pc-feedback-trace::pure + pc-feedback-vote::pure [share/pure + trace/pure 已存在]
- [x] 2.11 R729 — pc-companies::import_paths::pure (path validation) [Node upstream 仅 2 行 route，无 pure helper]
- [x] 2.12 R730 — pc-approvals::pure (approval state machine) [state_machine.rs 288 行 + hire_approved + hire_hook + change_consent_gate]
- [x] 2.13 R731 — pc-agent-jwt::pure (token validation) [17 tests PASS, lib.rs 557 行]
- [x] 2.14 R732 — pc-routine-variables::pure (variable interpolation) [548 行已存在]
- [x] 2.15 R733 — pc-workspace-commands::pure (command authorization) [10 tests PASS, lib.rs 591 行]
- [x] 2.16 R734 — pc-invite::pure (invite grant validation) [34 tests PASS, grants.rs + rate_limit.rs]
- [x] 2.17 R735 — pc-mentions::pure (mention extraction) [11 tests PASS, lib.rs 635 行]
- [x] 2.18 R736 — pc-pipeline-conversation-context::pure [29 tests PASS, pure.rs 100 行]
- [x] 2.19 R737 — pc-plan-review-context::pure [19 tests PASS, lib.rs 994 行]
- [x] 2.20 R738 — pc-folders::operation_log::pure [38 tests PASS]
- [x] 2.21 R739 — pc-project::workspace_runtime_config::pure [241 tests PASS（pc-tool 整体）]
- [x] 2.22 R740 — pc-tool::profile_binding::pure [241 tests PASS，profile_binding.rs 328 行]

## 3. 跨 crate DRY 整合 (R-INTEGRATION 续)

- [x] 3.1 pc-mentions → pc-issues hook 集成验证 [R562 MentionExtractionHook]
- [x] 3.2 pc-routine-variables → pc-routines 集成验证 [pc-routines::routine_variables 548 行]
- [x] 3.3 pc-pipeline-conversation-context → pc-pipelines 集成验证 [LoadPipelineContextInput]
- [x] 3.4 pc-plan-review-context → pc-issues 集成验证 [PLAN_REVIEW_CONTEXT_LIMITS]
- [x] 3.5 pc-decision-training → pc-decisions 集成验证 [pc-decisions::TrainingRecordHook]
- [x] 3.6 pc-feedback-share / trace / vote → pc-feedback 集成验证 [share/pure + trace/pure + vote/service]

## 4. 远程 execution (remote-execution-bridge)

- [ ] 4.1 创建 `pc-execution` crate + Cargo.toml + lib.rs
- [ ] 4.2 实现 `ssh_bridge::run` 抽象 + ssh2-rs 集成
- [ ] 4.3 实现 `restore_remote_workspace` (Node `workspace-runtime.ts::restoreRemoteWorkspace` 镜像)
- [ ] 4.4 实现 `materialize_remote_claude_config`
- [ ] 4.5 pc-http::routes::execution_workspaces 接入 pc-execution
- [ ] 4.6 集成测试：mock SSH server + 验证事件流
- [ ] 4.7 真实 SSH 集成测试（可选，跳过如不可用）

## 5. UI 真实 happy path (end-to-end-ui-validation)

- [ ] 5.1 安装 playwright + 配置 `tests/e2e/playwright/`
- [ ] 5.2 编写 60 client happy path 剧本（按 `ui/src/clients/*.ts` 顺序）
- [ ] 5.3 每个 client 配 expected DOM elements + API contract assertion
- [ ] 5.4 实现失败自动截图 + 录像到 `tests/e2e/playwright/screenshots/`
- [ ] 5.5 添加 `scripts/run-ui-e2e.sh` 启动 pc-server + UI + Playwright
- [ ] 5.6 CI workflow 集成（nightly 跑）

## 6. 性能基线 (performance-baseline)

- [ ] 6.1 添加 criterion 依赖到 pc-http / pc-decisions / pc-routines / pc-heartbeat / pc-realtime
- [ ] 6.2 为 hot path 写 criterion benches（路由处理 / decision 评估 / scheduler / heartbeat / broadcast）
- [ ] 6.3 `scripts/long-run-5min.sh`：wrk 持续 5 分钟，采集 P50/P95/P99 + RSS
- [ ] 6.4 `scripts/perf-compare.sh`：Rust vs Node 对比报告
- [ ] 6.5 写 `docs/perf-baseline.md`：3 种硬件配置（macOS/Linux/Windows）+ Rust/Node 版本 + P99/RSS 对比

## 7. 迁移注释 + 注释 + 回滚 (migration-safety-baseline)

- [ ] 7.1 为每个 pc-migrate migration 加 5+ 行 header comment (table 用途 / 使用方 / Node 等价)
- [ ] 7.2 实现 `cargo run -p pc-migrate --bin diff-report` → `MIGRATION_DIFF.md`
- [ ] 7.3 为每个 migration 添加 `down.sql` 镜像（无破坏性变更优先）
- [ ] 7.4 实现 `cargo run -p pc-migrate --bin verify-rollback` 全 migration apply + rollback 循环
- [ ] 7.5 实现 `cargo run -p pc-migrate --bin lint` 强制 header comment 检查

## 8. Plugin 互操作 (plugin-interop-testing)

- [ ] 8.1 pc-plugin-host::capability_validator 复刻 Node `plugin-capability-validator.ts`
- [ ] 8.2 pc-plugin-protocol JSON-RPC 集成测试：mock plugin host ↔ mock worker
- [ ] 8.3 端到端：pc-plugin-host 启动 + 真实 worker JSON-RPC 握手
- [ ] 8.4 pc-plugin-state-store 集成 pc-http plugin_ui_static 路由

## 9. 文档补齐 (V15 续)

- [ ] 9.1 写 `docs/RUNBOOK.md` [deferred — 需真实运维场景记录]
- [ ] 9.2 写 `docs/TROUBLESHOOTING.md` [deferred — 需真实故障样本]
- [ ] 9.3 写 `docs/FAQ.md` [deferred — 需真实问题集合]
- [x] 9.4 更新 ARCHITECTURE.md：R-INTEGRATION 13+ 状态 + 完整模块覆盖表 [ARCHITECTURE.md §10/§11 已含 R566-R572 + V1-V15 矩阵]

## 10. Workflow + Cron 端到端 (V9)

- [ ] 10.1 pc-cron 完整链路：parse + tick + execute + log
- [ ] 10.2 pc-workflow webhook signature validation
- [ ] 10.3 pc-workflow stage transitions + retry policy
- [ ] 10.4 集成测试：mock cron + mock workflow + 验证端到端

## 11. 验证基线

- [ ] 11.1 每个 R719+ round 后跑 `cargo test --workspace --lib` 全 PASS
- [ ] 11.2 每个 R-INTEGRATION 跑跨 crate 集成测试全 PASS
- [ ] 11.3 e2e-baseline.sh 持续跑（每周 regression）
- [ ] 11.4 性能基线对标：P99↓30% / RSS↓40%
- [ ] 11.5 Playwright 60 client 全 PASS
- [ ] 11.6 `cargo fmt --all --check` + `cargo clippy --workspace -- -D warnings` 0 警告

## 12. 收尾

- [ ] 12.1 parity-check.sh 报告覆盖率 ≥95%
- [ ] 12.2 所有 specs 验证场景都有对应测试
- [ ] 12.3 所有文档同步到最新状态
- [ ] 12.4 commit + tag v1.0 准备发布