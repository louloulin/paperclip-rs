# R796 - pc-repos 3 个死代码模块删除（agent_secret_bindings + issue_goal_fallback + batch_insert）

日期: 2026-08-18
范围: pc-repos 清理
方法: 验证无引用 → 删除 → 编译 → 测试

## 1. 删除清单

| 模块 | 行数 | 删除原因 | 验证方式 |
|---|---:|---|---|
| pc-repos/src/agent_secret_bindings.rs | 515 | 0 callers | grep -rn "pc_repos::agent_secret_bindings" crates/ apps/ |
| pc-repos/src/issue_goal_fallback.rs | 359 | 0 callers | grep -rn "pc_repos::issue_goal_fallback" crates/ apps/ |
| pc-repos/src/batch_insert.rs | 325 | 0 callers | grep -rn "pc_repos::batch_insert" crates/ apps/ |

合计删除: 1199 行死代码

## 2. lib.rs 改动

- 删除 pub mod agent_secret_bindings;
- 删除 pub mod issue_goal_fallback;
- 删除 pub mod batch_insert;

## 3. 验证结果

- cargo build -p pc-repos: 通过 (33.64s)
- cargo test -p pc-repos --lib: 533 passed (从 571 降至 533)
- cargo test -p pc-feedback --lib: 128 passed
- cargo test -p pc-issues --lib: 198 passed
- cargo test -p pc-documents --lib: 24 passed
- cargo test -p pc-folders --lib: 10 passed
- cargo test -p pc-work-products --lib: 8 passed
- cargo test -p pc-core --lib: 1157 passed
- cargo test -p pc-routines --lib: 207 passed
- cargo test -p pc-heartbeat --lib: 666 passed
- cargo test -p pc-agent --lib: 83 passed
- cargo test -p pc-tool --lib: 241 passed
- cargo test -p pc-pipelines --lib: 43 passed
- cargo test -p pc-decisions --lib: 185 passed
- cargo test -p pc-approvals --lib: 58 passed
- cargo test -p pc-goals --lib: 6 passed
- cargo test -p pc-inbox --lib: 25 passed
- Rust server /health: 200
- Vite dev server: 200 (5174)

## 4. 死代码识别规则

1. 同功能多 crate 出现 → 保留更新版本，删除旧的（R794 workspace_operation_log_store → pc-folders::operation_log_store；R795 issue_continuation_summary/ → pc-issues::continuation_summary）
2. pub mod xxx 无人引用 → 删除（R796 这 3 个）
3. 验证命令:
   grep -rn "pc_repos::MODULE_NAME" crates/ apps/
   grep -rn "crate::MODULE_NAME" crates/pc-repos/src/
   两边都没有 = 死代码

## 5. 剩余 pure 候选（重新审计）

| 模块 | 行数 | 状态 | 原因 |
|---|---:|---|---|
| task_watchdog_scope/classifier | 361 | NOT dead | task_watchdog_scope::context.rs:120 引用 |
| task_watchdog_scope/context | 273 | NOT dead | task_watchdog_scope::mod.rs 重新导出 |
| redact | 337 | NOT dead | 被 pc-repos::issue_approvals + agent_action_audit 使用 |
| change_consent_gate/ | ~? | 待查 | |
| issue_terminal_effects/ | ~? | 待查 | |

## 6. 累计（R756 → R796）

- 32 跟踪 crate lib 测试: ~3761 PASS (含本次验证)
- DB integration: 17 (R788+R789+R791+R793)
- 整体加权进度: ~97.5%

## 7. R797+ 计划

- R797: 继续审计 change_consent_gate/ issue_terminal_effects/ 是否可拆 pure/db
- R798: pc-repos::IssueRepo::create_work_product / update_work_product HTTP 层返回类型统一
- R799: UI 集成链路 Round 3（覆盖 R792 提到的 Layout bug 影响页面）
- R800+: 进一步清理 + 端到端验证
- Adapter 15 个永久跳过（硬约束 #2）