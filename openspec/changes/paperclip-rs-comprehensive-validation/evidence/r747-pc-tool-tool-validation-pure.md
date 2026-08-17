# R747 — pc-tool/tool_validation_pure 纯函数模块

## 目标

把 `pc-tool/src/service.rs` 中 `create` / `patch` / `set_status` 校验抽到
独立 `tool_validation_pure` 模块，与 R744/R745/R746 同模式。

## 新增内容

### `crates/pc-tool/src/tool_validation_pure.rs`（新增 10.0 KB / 32 单测）

#### 公开 API

| 函数 / 常量 | 用途 | 对齐 Node |
|---|---|---|
| `ALLOWED_TOOL_KINDS` | mcp / api / cli / webhook | `pc_repos::ToolApplicationType` |
| `ALLOWED_TOOL_STATUSES` | active / disabled / draft | `pc_repos::ToolApplicationStatus` |
| `is_tool_kind_allowed` / `is_tool_status_allowed` | 谓词 | service 内联 |
| `validate_tool_name_non_empty` | name trim + 非空 | service create / patch |
| `validate_tool_kind` | kind 枚举校验 | service create |
| `validate_tool_status` | status 枚举校验 | service set_status |
| `validate_tool_metadata` | metadata 是 object 或 null | service create |
| `validate_tool_patch_name` / `_description` / `_status` / `_metadata_merge` | patch 三态语义（None/Some(non-empty)/Some(empty)）| service patch |
| `has_duplicate_name` | trim 后判重 | service create name uniqueness |
| `normalize_tool_kinds` | trim + 收集 | service 批量路径 |

#### 设计要点

1. **零 DB**：所有函数只消费字符串 / Value / slice。
2. **不引入 ToolError**：返回 `Result<(), &'static str>`，调用方在 service 层
   包成 `ToolError::Validation`。
3. **Some/Some("")/None 三态**：与 R746 trigger patch 校验一致。
4. **trim 语义**：name / kind / status 校验前统一 trim，避免前后空白导致的
   "看似不同实际重复"问题。
5. **tests 全部命名 `r747_*`**。

## 验证

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip-rs
cargo test -p pc-tool --lib tool_validation_pure
```

结果：

```
test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 183 filtered out
```

```bash
cargo test -p pc-tool --lib
```

结果：

```
test result: ok. 215 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

## 与 Node parity

| 行为 | Node | Rust | 一致 |
|---|---|---|---|
| name 非空 | ✓ | ✓ | ✅ |
| kind ∈ {mcp, api, cli, webhook} | ✓ | ✓ | ✅ |
| status ∈ {active, disabled, draft} | ✓ | ✓ | ✅ |
| metadata 是 object | ✓ | ✓ | ✅ |
| 同 trim 后 name 重复 | ✓ | ✓ | ✅ |

## 累计

| 项 | 之前 | R747 后 |
|---|---:|---:|
| pc-tool lib tests | 182 | **215** |
| pc-tool R747 新增 | — | **+33** |
| 累计 R712-R747 新增 | 372 | **+32 = 404 PASS** |
| 累计新代码行数 | ~10500 | **~11000** |

## 后续

- **R748** — pc-feedback/redaction 服务层补足
- **R749** — pc-companies/search_rate_limit 补足
- **R750** — pc-routines/activity_gate pure helper 抽取
