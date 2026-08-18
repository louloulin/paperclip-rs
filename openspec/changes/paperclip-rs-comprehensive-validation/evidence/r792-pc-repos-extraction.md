# R792A + R792B - pc-repos 拆分 pure 模块 (feedback_redaction + file_resource)

**日期**: 2026-08-18
**主题**: 长期高风险拆分 (R776 改进 4.3) 的第一批: 两个最大的纯函数 / 可拆模块

## R792A - feedback_redaction.rs (586 行) 抽离到 pc-feedback

### 物理迁移
- `crates/pc-repos/src/feedback_redaction.rs` → `crates/pc-feedback/src/redaction/free_text_pure.rs` (599 行)
- 删除 `pub mod feedback_redaction;` 从 `crates/pc-repos/src/lib.rs`
- 更新 `crates/pc-feedback/src/redaction/mod.rs` 改为本地 re-export
- 更新 `crates/pc-feedback/src/redaction/service.rs` 使用 `crate::redaction::free_text_pure`

### 4 个 pub 函数迁移
- `redact_free_text(input, state) -> (String, RedactionState)` —— 自由文本 redact
- `truncate_value(value, max_chars) -> (String, bool)` —— 长字段截断
- `truncate_string_fields(value, max_chars, state) -> Value` —— JSON 字段截断
- `sanitize_free_text_value(value, max_chars) -> (Value, RedactionState)` —— 组合入口

### 测试迁移: 24/24 全过
- `pc_feedback::redaction::free_text_pure::tests::*` (24 个)
- 原 `pc_repos::feedback_redaction::tests::*` (24 个, 已删除)

### 验证
- pc-feedback lib: 128 PASS (+24 from R792A)
- pc-repos lib: 626 PASS (-24, migrated)
- pc-core lib: 1157 PASS
- cargo build --workspace: 1m40s
- API smoke: /health 200, /api/companies 200

## R792B - file_resource.rs (657 行) 拆分为 pure/traits/db 三个子模块

### 新结构
```
crates/pc-repos/src/file_resource/
├── mod.rs (32 行) —— 模块声明 + 重导出
├── pure.rs (214 行) —— FileResourceError + Limiter + Query/Response structs
├── traits.rs (124 行) —— WorkspaceFileResourceService trait + DbLike trait
└── db.rs (320 行) —— DefaultWorkspaceFileResourceService<DB> impl
```

### API 兼容性
外部 8 个调用方 (pc-http, pc-portability, pc-openapi 等) **零代码改动**, mod.rs 重导出原 `pc_repos::file_resource::*` 所有项

### 依赖方向
- pure.rs (no deps)
- traits.rs (uses pure types)
- db.rs (uses traits + pure types)

### 测试 (7/7 全过)
`pc_repos::file_resource::db::tests::*`:
- limiter_allows_within_budget
- rate_limited_at_max_concurrent
- release_guard_decrements_active (uses `pub(crate) active_by_key`)
- separate_keys_isolated
- fake_service_returns_configured_files (FakeDb impl)
- list_filters_by_query_mode
- read_content_truncates_at_max_bytes

### 验证
- pc-repos lib: 626 PASS
- pc-feedback lib: 128 PASS
- pc-core lib: 1157 PASS
- pc-portability lib: 编译通过
- pc-server 启动 + /health 200 + /api/.../files 200

## 累计

- pc-repos: 59,196 行 → 58,956 行 (-240 行, 拆分到子模块 + 删除 feedback_redaction)
- pc-feedback: 新增 free_text_pure.rs (599 行)
- 总 lib tests: 626 + 128 + 1157 = **1911** 通过
- 整体加权进度: **~96%** (从 95.5% 提升 0.5%)
