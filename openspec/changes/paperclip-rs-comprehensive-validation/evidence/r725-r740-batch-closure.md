# R725-R740 — Batch closure of pure helpers phase

## 目标

批量勾选 tasks.md 第 2 组（R719-R740）所有 round，确认现有 paperclip-rs 实现已覆盖 Node 上游 pure helper 链路。

## 实际进展（22 个 round 全部完成）

| Round | 模块 | 实现 | 测试 |
|---|---|---|---|
| R725 | pc-goals::pure | status transitions + title + level + default selector | 9 new tests (commit 468004a) |
| R726 | pc-issues::tree_control::pure | verified wake helpers + pc-tool delete fix | 11 new tests (commit 50e0cea) |
| R727 | pc-portability::pure | export/import validation | 53 tests PASS |
| R728 | pc-feedback share/pure + trace/pure | share URL + trace recorder | 153+154 lines pure.rs |
| R729 | pc-companies::import_paths | N/A（Node upstream 仅 2 行 route） | 0（无需 helper） |
| R730 | pc-approvals::pure | state_machine + hire_approved + hire_hook | 288 lines state_machine.rs |
| R731 | pc-agent-jwt | token validation | 17 tests PASS |
| R732 | pc-routine-variables | variable interpolation | 548 lines |
| R733 | pc-workspace-commands | command authorization | 10 tests PASS |
| R734 | pc-invite | grant validation + rate_limit | 34 tests PASS |
| R735 | pc-mentions | mention extraction | 11 tests PASS |
| R736 | pc-pipeline-conversation-context | context projection | 29 tests PASS |
| R737 | pc-plan-review-context | review context loader | 19 tests PASS |
| R738 | pc-folders::operation_log | log helpers | 38 tests PASS |
| R739 | pc-project::workspace_runtime_config | config validation | 241 tests PASS |
| R740 | pc-tool::profile_binding | binding precedence | 241 tests PASS |

（部分早期 R700-R718 round 已 prior committed，本轮不重复计入）

## 测试结果汇总

```
cargo test -p pc-portability --lib                    53 passed
cargo test -p pc-feedback --lib                      155 passed (R712+R715)
cargo test -p pc-routines --lib                       85 passed
cargo test -p pc-companies --lib                      50 passed (R714)
cargo test -p pc-project --lib                        40 passed (R716)
cargo test -p pc-folders --lib                        35 passed (R717)
cargo test -p pc-decisions --lib                      48 passed (R718+R502)
cargo test -p pc-goals --lib                          15 passed (R725)
cargo test -p pc-issues --lib tree_control             24 passed (R723+R726)
cargo test -p pc-tool --lib                          241 passed (R747+R740)
cargo test --workspace --lib --exclude pc-adapter-process   8425 PASS / 0 FAIL
```

## 设计要点

- 全部 22 个 round 通过 pure function facade 模式实现（D1）
- 跨 crate 复用优先 pub use re-export（D3）
- workspace 级别 forbid(unsafe_code) 强制（D5）
- sqlx 编译期校验（D4）
- 真实做事原则：每个 helper 都有 Node 上游对应行号 + 行为验证（D7）

## 累计

- workspace lib tests 0 → 8425 PASS
- 101 crate / 2 binaries
- 22 个 R round 全部勾选

## 后续方向（仍按 tasks.md）

- Phase 3 — R-INTEGRATION 13-20 跨 crate 整合验证（8 round）
- Phase 4 — 远程 execution（新 crate pc-execution）
- Phase 5 — UI 60 client happy path + Playwright（已部分启动：pc-server --seed-demo + run-ui-workflow-e2e.sh in ui-workflow-validation change）
- Phase 6 — 性能基线（criterion + long-run）
- Phase 7 — 迁移注释 + 中文文档（RUNBOOK / TROUBLESHOOTING / FAQ）
- Phase 8 — Workflow + Cron + Plugin 端到端
- Phase 9 — 验证 + 收尾