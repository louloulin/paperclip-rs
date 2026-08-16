# R722 — pc-tool/src/profile_helpers.rs

## 目标

补足 Node services/tool-access.ts 中 profile 相关的零 DB helpers。

## 新增 helpers（4 个 + 1 个 struct）

| Node 函数 | Rust 函数 |
|---|---|
| profileEntryMatchesCatalog | profile_entry_matches_catalog(entry, catalog_entry) |
| summarizeProfile | summarize_profile(profile, entries, bindings, catalog, agent_ids) + ToolProfileSummary struct |
| profileCoversCatalogScope | profile_covers_catalog_scope(entry, catalog_entry, catalog_by_id) |
| pendingNewToolsForProfile | pending_new_tools_for_profile(profile, entries, catalog, apps, conns, watermark) + PendingNewToolItem struct |

## 测试结果

cargo test -p pc-tool --lib profile_helpers
running 9 tests
...
test result: ok. 9 passed; 0 failed

## 关键设计

- 所有 helper 操作 serde_json::Value，避免引入 DTO 类型（保持 pure）
- profile_entry_matches_catalog 严格按 Node 5 个 selectorType 分支
- summarize_profile 按 Node defaultAction == 'allow' 决定 accessMode
- pending_new_tools_for_profile 接受显式 watermark 参数（更易测试）
