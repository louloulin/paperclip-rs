# R757 — UI Routine / Tool mutation 冒烟 + Critical Bug 修复

## 目标

1. 验证 paperclip-rs Routine mutation 完整链路（POST/PATCH/GET/DELETE）
2. 验证 paperclip-rs Tool application mutation 完整链路
3. 发现并修复 ToolApplicationRow.kind DB 列映射 bug

## 环境

- Rust server：tty session 13694，端口 3100
- PostgreSQL 17：端口 55433
- 部署模式：local_trusted

## 1. Routine mutation 链路 — PASS

### 链路

| 操作 | 端点 | 状态码 | 结果 |
|---|---|---|---|
| POST | /api/routines | 201 | routine_id=b93f9110-810b-4d68-b34d-c6842358a1b0（revision=1）|
| PATCH | /api/routines/{id} | 200 | title/priority/status 更新（revision=2）|
| GET | /api/routines/{id} | 200 | 字段一致，含 descriptionDocument |
| DELETE | /api/routines/{id} | 204 | 清理完成 |
| GET (after) | /api/routines/{id} | 404 | "not found: routine ..." |

### DB 一致性

- 删前 count: 1
- 删后 count: 0
- revision 编号 1 → 2 证明 RoutineRepo::update_with_revision 创建了新 revision

### 证据

- `.tmp/r757-routine-create.json` / `r757-routine-update.json` / `r757-routine-get.json` / `r757-routine-delete.json`

## 2. Tool application mutation 链路 — 发现 Critical Bug

### 初次调用结果

| 操作 | 端点 | 状态码 | 现象 |
|---|---|---|---|
| POST | /api/companies/{id}/tools/applications | 500 | DB 写入成功但响应失败 |
| GET list | /api/companies/{id}/tools/applications | 500 | "Internal server error" |
| GET one | /api/tool-applications/{id} | 500 | "Internal server error" |

### 诊断过程

通过 eprintln 跟踪 + 文件 trace，最终定位到 `ToolApplicationRow`：

```
[R757DBG] list: after list_by_company, is_ok=false err=Some(Sql(ColumnNotFound("kind")))
```

### Root Cause

`ToolApplicationRow.kind` 字段只有 `#[serde(rename = "type")]`（影响 JSON 序列化），
但 sqlx 0.8 的 FromRow 派生独立看字段名（snake_case），找不到名为 `kind` 的列。
DB schema 列名是 `type`（SQL 关键字，不能直接当 Rust 字段名）。

```rust
// 修复前
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApplicationRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,  // <-- 缺 #[sqlx(rename = "type")]
    pub status: String,
    pub metadata: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// 修复后
#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApplicationRow {
    ...
    #[serde(rename = "type")]
    #[sqlx(rename = "type")]   // <-- 加上
    pub kind: String,
    ...
}
```

### 修复后链路 — PASS

| 操作 | 端点 | 状态码 | 结果 |
|---|---|---|---|
| POST | /api/companies/{id}/tools/applications | 200 | tool_id=37518b70-f091-4085-b6cd-5060d38ab3aa |
| PATCH | /api/tool-applications/{id} | 200 | {id, updated: true} |
| GET | /api/tool-applications/{id} | 200 | description 更新为 "R757 mutated" |
| DELETE | /api/tool-applications/{id} | 204 | 删除完成 |
| GET (after) | /api/tool-applications/{id} | 404 | "not found: tool application ..." |

### DB 一致性

- 删前 count: 1
- 删后 count: 0

### 关键发现

| 项 | 现象 | 状态 |
|---|---|---|
| POST 状态码 | 返回 200 而非 201 Created | ⚠️ 已知差异（不影响功能）|
| POST 返回包络 | 直接返回 ToolApplication JSON（无 `{tool: {...}}` 包络） | 与 R756 Agent POST 不同（Agent 有 `{agent: {...}}`）|
| PATCH 返回 | {id, updated: true}（不返回完整 row）| ⚠️ 与 GET 返回完整 row 不同 |
| description 路径 | 走 metadata.jsonb.description | ✅ 正确 |

## 3. R757 Regression 测试 — PASS

在 `crates/pc-repos/src/tool.rs::tests` 末尾追加 5 个 R757 unit test：

| 测试 | 验证 |
|---|---|
| r757_tool_application_row_kind_uses_db_type_column | source review：kind 字段必须带 `#[sqlx(rename = "type")]` |
| r757_tool_application_row_description_from_metadata | description() helper 正确从 metadata 提取 |
| r757_tool_application_row_config_from_metadata | config() helper 正确从 metadata 提取 |
| r757_tool_application_row_missing_metadata_keys | metadata 缺失 description/config 时返回 None/{} |
| r757_patch_tool_application_metadata_patch_order | PatchToolApplication 合并顺序正确 |

### 验证

```
cargo test -p pc-repos r757
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 645 filtered out

cargo test -p pc-repos --lib
test result: ok. 650 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

### 无 Regression

- pc-tool: 215 PASS（不变）
- pc-repos: 650 PASS（先前 645 + 5 R757 = 650）

## 4. 总结

| 项 | 状态 |
|---|---|
| Routine mutation 全链路 | ✅ PASS |
| Tool application mutation 全链路 | ✅ PASS（修复 critical bug 后）|
| R757 regression 测试 | ✅ 5 PASS |
| pc-tool 无 regression | ✅ 215 PASS |
| pc-repos 无 regression | ✅ 650 PASS |
| Bug 修复影响范围 | 阻塞 `/api/companies/{id}/tools/applications` 全路径，已修复 |
| 修改文件 | `crates/pc-repos/src/tool.rs`（ToolApplicationRow + 5 tests）|
| 修改文件 | `crates/pc-http/src/routes/tool_access.rs`（临时诊断后已恢复原状）|

## R758+ 后续计划

- R758 — pc-issues / liveness / scheduler 集成测试
- R759 — pc-heartbeat / reconcile 集成测试
- R760 — pc-decisions / wakeup / execution 集成测试
- 真实 Chromium 浏览器对核心页面完成 mutation 流程
- Adapter 仍按硬约束保持不动
