# R778 — pc-agent::agent_assignability 加测 (+15 PASS)

日期: 2026-08-17
范围: pc-agent/src/agent_assignability.rs
新增: 15 个 r778_ 单测（数据形状 / serde 序列化 / Display 文案）

## 背景

R777 深度审计发现 pc-agent::agent_assignability 1:1 port 自 Node services/agent-assignability.ts，
但 Rust 端 0 单测（Node 端也无 .test.ts）。这是 14 个核心业务域中最大的覆盖率缺口之一。

## 验证

cargo test -p pc-agent --lib        83 passed (+15 R778, 基线 68)
cargo test -p pc-agent agent_assignability  15 passed

## 新增测试

### AgentAssignmentKind (3)
- r778_assignment_kind_serializes_work — Work → JSON 含 work
- r778_assignment_kind_serializes_routine — Routine → JSON 含 routine
- r778_assignment_kind_is_copy_and_eq — Copy 语义 + 不等判定

### AgentAssignmentConflictReason (2)
- r778_conflict_reason_serializes_all_variants — 7 变体全部 snake_case
- r778_conflict_reason_distinct_pairs — 3 对不等判定

### AncestorChainEntry (2)
- r778_ancestor_chain_entry_serializes_camel_case — companyId / reportsTo camelCase
- r778_ancestor_chain_entry_serializes_reports_to_null — reports_to=None → null

### AgentAssignmentConflictDetail (2)
- r778_conflict_detail_code_is_agent_not_assignable — code 字段固定
- r778_conflict_detail_serializes_with_chain — 含 invalidAncestorAgentId / ancestorChain

### AgentAssignmentError (3)
- r778_error_not_found_display — Display 文案 1:1
- r778_error_cross_company_display — Display 文案 1:1
- r778_error_conflict_serializes_with_detail_flatten — Conflict 变体 flatten 序列化
- r778_error_conflict_display_includes_message — Display 含 message

### AssertAssignableOptions (2)
- r778_options_default_has_no_kind — Default 实现
- r778_options_with_kind_clone — Clone 保留 kind

## 设计决策

1. 避免反序列化: 类型只 derive Serialize, 未 derive Deserialize，所以不做完整 round-trip 测试；改为序列化输出 contains 子串 + Debug 验证。
2. 覆盖纯数据形状: 公开 enum/struct 的所有变体 + serde 行为 + Display 文案，不依赖 DB。
3. 使用 contains() 而非字面值比较: 避免反斜杠转义陷阱 (已踩坑修复)。
4. 内联 #[cfg(test)] mod internal_tests: 遵循 handoff 硬约束 #10。

## 累计

R756-R778 累计 25 跟踪 crate 共 3055 PASS (+15 R778)。

## R779+ 后续计划

- R779 — pc-tool 加 root re-exports (R776 改进 4.2)
- R780 — pc-core 加精选 root re-exports (R776 改进 4.4)
- R781 — pc-pipeline-conversation-context 拆分 pure.rs/service.rs (R776 改进 4.1)
- R782+ — pc-repos 拆分 pure/db (R776 改进 4.3)
- Adapter 永远跳过 (硬约束 #2)