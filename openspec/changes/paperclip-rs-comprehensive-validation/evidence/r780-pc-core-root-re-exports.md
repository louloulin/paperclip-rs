# R780 - pc-core 加精选 root re-exports (R776 改进 4.4)

日期: 2026-08-17
范围: crates/pc-core/src/lib.rs
新增: 13 个子模块精选 root re-export，约 73 个公开项

## 背景

R776 架构审计 4.4 指出 pc-core 有 60+ 子模块但部分常用模块缺精选 re-export。本轮补足这些缺口，
调用方可写 pc_core::sha256_hex(...) 而非 pc_core::hash::sha256_hex(...)。

## 验证

cargo build -p pc-core         编译成功 (0 error, 18 warnings 均为既有 unused)
cargo test -p pc-core --lib   1157 passed (基线一致)

## 新增 re-export 子模块 (13 个)

| 子模块 | re-export 项数 | 关键项 |
|---|---:|---|
| attention | 8 | DETAIL_EXCERPT_LENGTH, OPEN_DECISION_MAX_LIMIT |
| dev_server_status | 5 | read_persisted_status, PersistedDevServerStatus |
| execution_workspace_config | 5 | DESIRED_STATE_RUNNING, DesiredState |
| execution_workspace_overview | 5 | max_date, max_date_dt, ServiceStatus |
| git_status_paths | 2 | parse_git_status_paths, GitStatusPaths |
| hash | 2 | sha256_hex, constant_time_eq |
| issue_execution_validation | 4 | parse_issue_execution_state, ValidationIssue |
| stable_string | 4 | stable_stringify, versioned_sha256_fingerprint |
| tool_content_guards | 5 | REDACTED_VALUE, is_plain_object, stable_serialize |
| workspace_branch_incoherence | 5 | fingerprint_workspace_branch_incoherence, Cleanliness |
| workspace_branch_incoherence_explain | 8 | explain_git_worktree_branch_reconcile_inspection |
| workspace_dirty_quarantine_formatter | 7 | format_dirty_quarantine_contention_refusal |
| workspace_file_classify | 8 | WORKSPACE_FILE_*_BYTES, MAX_LIST_DEPTH |
| workspace_runtime_readiness | 5 | resolve_shell, looks_like_workspace_dev_server_command |

## 设计决策

1. 精选而非全量: 不使用 pub use xxx::*; 全量 re-export，避免根命名空间污染。手动列出每个公共项。
2. 不重复定义: 跳过 id::Id（已在 pc-core 顶层 use 别处定义），避免冲突。
3. 不修改子模块: 仅在 lib.rs 追加 use 语句，无行为变化。
4. 保留既有 re-export: 保留 actor / actor_runtime / source_trust / 等 30+ 既有 re-export。

## 累计

R756-R780 累计 25 跟踪 crate 共 3055 PASS (R780 0 增量单测，纯 API 形状改进)。
pc-core 测试数 1157 passed 保持。

## R781+ 后续计划

- R781 - pc-pipeline-conversation-context 拆分 pure.rs/service.rs (R776 改进 4.1)
- R782+ - pc-repos 拆分 pure/db (R776 改进 4.3 长期)
- Adapter 永远跳过 (硬约束 #2)