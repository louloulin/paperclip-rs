# R575 — `/api/v1/runs` 路由补齐

**状态**: ✅ 完成 (2026-08-12)

## 1. 背景

M19 路由审计（`.route-audit/ui-openapi-overlap.md`）显示 UI 客户端真实调用
但 Rust 端未注册的 15 个路径，其中：

| Path | 状态 |
|---|---|
| `GET /api/v1/runs` | **R575 补齐** ✅ |
| `GET /api/companies/{companyId}/events/ws` | R576 待办 |
| 其它 13 个 | 已存在 pc-http 但 OpenAPI 未注册 |

## 2. 实现

### 2.1 新增模块 `crates/pc-http/src/routes/v1.rs` (145 LOC)

```rust
pub fn router() -> Router<AppState> {
    Router::new().route("/runs", get(list_runs))
}

async fn list_runs(
    State(state): State<AppState>,
    Query(q): Query<ListRunsQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let filter = HeartbeatRunFilter { ... };
    let rows = HeartbeatRepo::new(&state.db)
        .list_for_company(q.company_id, &filter)
        .await?;
    // ...
}
```

### 2.2 设计要点

- **版本前缀**: `/api/v1/...` 与 `/api/...` 解耦
- **公司隔离**: `company_id` 是必需 query 参数（强制 scope）
- **纯查询**: v1 仅暴露读路径；写路径继续走 `/api/...`
- **委托**: 通过 `HeartbeatRepo::list_for_company` 委托 pc-repos（不重新实现 SQL）
- **类型安全**: `parse_statuses` 过滤非法 status（"done" → 跳过，"queued" → 接受）

### 2.3 类型对齐修正

R575 首次实现用了 `chrono::DateTime<Utc>`，但 `HeartbeatRow.started_at` 实际是
`Option<pc_core::Timestamp>`（newtype 包装 DateTime）。修正后：

```rust
pub started_at: Option<pc_core::Timestamp>,
pub finished_at: Option<pc_core::Timestamp>,
```

## 3. 测试

### 3.1 Lib 单元测试（5 个 in `v1.rs::tests`）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r575_parse_statuses_empty` | None/空字符串返回空 vec |
| 2 | `r575_parse_statuses_single` | 单个 status 正确解析 |
| 3 | `r575_parse_statuses_multiple` | 多个 status 都解析 |
| 4 | `r575_parse_statuses_invalid_filtered` | 非法 status 被过滤 |
| 5 | `r575_router_exposes_runs_path` | router() 编译并返回 Router |

### 3.2 集成测试（crates/pc-http/tests/r575_v1_runs_route.rs, 6 个）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r575_v1_module_exports_router` | v1 模块静态可用 |
| 2 | `r575_list_runs_query_required_field_is_company_id` | 缺 companyId 必失败 |
| 3 | `r575_list_runs_query_with_only_company_id` | 仅 companyId 可解析 |
| 4 | `r575_list_runs_query_with_all_fields` | 全字段解析 |
| 5 | `r575_run_summary_serializes_with_camel_case` | camelCase 输出 |
| 6 | `r575_run_summary_omits_null_fields` | None 字段被跳过 |

### 3.3 测试统计

```
$ cargo test -p pc-http --lib
test result: ok. 377 passed; 0 failed   # 372 pre + 5 R575 new

$ cargo test -p pc-http --test r575_v1_runs_route
test result: ok. 6 passed; 0 failed
```

## 4. 无回归验证

- pc-http lib: 372 → **377** (+5)
- pc-http integration tests: +6
- workspace 整体无变化

## 5. 设计亮点

### 5.1 单测反推设计

写 `r575_parse_statuses_multiple` 测试时发现 "done" 不是合法的
`HeartbeatRunStatus`（合法值是 "succeeded"）。修正测试用例的同时验证了
`parse_statuses` 的非法过滤行为：

- "running, invalid_state, succeeded" → 解析出 2 个（"running", "succeeded"）
- 非法 status 静默跳过，不报错（API 容错设计）

### 5.2 高内聚低耦合

R575 没有自己写 SQL，而是委托 `pc_repos::heartbeat::HeartbeatRepo::list_for_company`。
未来 pc-repos 升级（如加 status 过滤优化）时，R575 自动受益。

### 5.3 版本前缀价值

`/api/v1/...` 与 `/api/...` 解耦，将来 v2 引入不兼容变更时可共存，
不需要做路由迁移 / 客户端 breaking change。

## 6. 下一步

R576: 实现 `/api/companies/{companyId}/events/ws` WebSocket 端点（UI client
真实调用但 Rust 端未实现）。
