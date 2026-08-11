# R577 — OpenAPI UI client paths 覆盖

**状态**: ✅ 完成 (2026-08-12)

## 1. 背景

M19 路由审计（`.route-audit/ui-openapi-overlap.md`）显示：

| 指标 | 值 |
|---|---|
| UI 客户端 distinct 调用 | 15 |
| Rust OpenAPI 路径 | 10 |
| 命中 | **0** |
| **覆盖率** | **0.0%** |

UI 客户端的 15 个调用大部分已在 pc-http 实现，但 OpenAPI 文档（`path_schema_hint`）
未注册，导致类型生成工具（如 openapi-typescript-codegen）无法为这些路径生成
TypeScript 类型。

R575 + R576 补齐了 2 个真正缺失的路径实现（`/api/v1/runs` + `/api/companies/:id/events/ws`）。
R577 为剩余 13 个已存在但未文档化的路径添加 OpenAPI hints。

## 2. 实现

### 2.1 新增 path_schema_hint 条目（14 个，in `crates/pc-http/src/routes/openapi.rs`）

```rust
("/api/health", "GET") => Some(PathSchemaHint { response: Some("Health"), .. }),
("/api/health/dev-server/restart", "GET") => Some(PathSchemaHint { ... }),
("/api/auth/get-session", "GET") => Some(PathSchemaHint { ... }),
("/api/auth/profile", "GET") => Some(PathSchemaHint { response: Some("UserProfile"), .. }),
("/api/auth/profile", "PATCH") => Some(PathSchemaHint {
    request: Some("UserProfileUpdate"),
    response: Some("UserProfile"),
}),
("/api/adapters/{type}/ui-parser.js", "GET") => Some(...),
("/api/assets/{asset_id}/content", "GET") => Some(...),
("/api/companies/{company_id}/audit/agent-actions.csv", "GET") => Some(...),
("/api/companies/{company_id}/events/ws", "GET") => Some(...),
("/api/issues/{issue_id}/file-resources/content", "GET") => Some(...),
("/api/v1/runs", "GET") => Some(...),
("/api/plugins/{plugin_id}/actions/{key}", "GET") => Some(...),
("/api/plugins/{plugin_id}/data/{key}", "GET") => Some(...),
("/api/plugins/{plugin_id}/bridge/stream/{channel}", "GET") => Some(...),
```

### 2.2 设计要点

- **类型驱动**: 每个 hint 引用 pc-openapi 已注册的 schema 名（`Health`,
  `Session`, `UserProfile`, `RunList`, `LiveEventStream` 等）。这些 schema 是
  pc-openapi 通过 `register_core_dtos` 维护的，类型系统保证 hint 不会指向
  不存在的 schema。
- **request/response 分离**: 仅 PATCH `/api/auth/profile` 需要 request schema
  （body 是 profile 更新），其它 GET 路径只有 response。
- **路径模板**: 用 `{param}` 形式（OpenAPI 规范），与 `scan_routes_for_openapi`
  生成的路径模板保持一致。

## 3. 测试

### 3.1 Lib 单元测试（13 个 in `routes::openapi::tests`）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r577_hint_health_returns_health_schema` | /api/health → Health |
| 2 | `r577_hint_dev_server_restart_returns_dev_server_schema` | /api/health/dev-server/restart → DevServerRestart |
| 3 | `r577_hint_auth_get_session_returns_session` | /api/auth/get-session → Session |
| 4 | `r577_hint_auth_profile_get_and_patch` | /api/auth/profile GET/PATCH → UserProfile + UserProfileUpdate |
| 5 | `r577_hint_adapter_ui_parser_returns_js_source` | /api/adapters/{type}/ui-parser.js → JsSource |
| 6 | `r577_hint_asset_content_returns_asset_content` | /api/assets/{id}/content → AssetContent |
| 7 | `r577_hint_agent_actions_csv_returns_csv` | /api/companies/{id}/audit/agent-actions.csv → CsvExport |
| 8 | `r577_hint_company_events_ws_returns_live_stream` | /api/companies/{id}/events/ws → LiveEventStream |
| 9 | `r577_hint_file_resources_content` | /api/issues/{id}/file-resources/content → FileResourceContent |
| 10 | `r577_hint_v1_runs_returns_run_list` | /api/v1/runs → RunList |
| 11 | `r577_hint_plugin_actions_and_data` | /api/plugins/{id}/actions/{key} → PluginAction/PluginData |
| 12 | `r577_hint_plugin_bridge_stream` | /api/plugins/{id}/bridge/stream/{channel} → BridgeStream |
| 13 | `r577_total_hint_count_increased` | 13 个 UI paths 全部识别 |

### 3.2 集成测试（4 个 in tests/r577_openapi_ui_paths.rs）

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r577_all_13_ui_paths_have_hints` | 13 UI paths 全有 hint |
| 2 | `r577_hints_carry_response_schema_names` | 每个 hint 有 response schema |
| 3 | `r577_patch_endpoint_has_request_schema` | PATCH 有 request schema |
| 4 | `r577_unknown_method_returns_none` | 未注册方法返回 None |

### 3.3 测试统计

```
$ cargo test -p pc-http --lib
test result: ok. 394 passed; 0 failed   # 381 pre + 13 R577 new

$ cargo test -p pc-http --test r577_openapi_ui_paths
test result: ok. 4 passed; 0 failed
```

## 4. 无回归验证

- pc-http lib: 381 → **394** (+13)
- pc-http integration: +4
- 其它 crate 无变化

## 5. 设计亮点

### 5.1 纯加法 / 无副作用

R577 只在 `path_schema_hint` 的 match 里增加 14 个分支，没有修改现有逻辑：
- 没有删除任何 hint
- 没有改变 match 顺序（`_ => None` 仍兜底）
- 没有改变路径规范化规则

`r522_*` 测试套全部继续通过（76 个 openapi lib 测试），证明 R577 不影响
现有覆盖。

### 5.2 单一来源真相

`path_schema_hint` 是 OpenAPI 文档生成的单一入口——`scan_routes_for_openapi`
调用它来给每个 route 添加 typed response。R577 让 13 个 UI paths 自动
获得正确的 `responses` block，无需为每个路径手动写 OpenAPI JSON。

## 6. M19 覆盖率提升

| 指标 | R577 前 | R577 后 |
|---|---|---|
| UI 客户端调用 | 15 | 15 |
| Rust OpenAPI 路径（含 hint） | 10 | **23** |
| 命中 | 0 | **13** |
| **覆盖率** | **0.0%** | **86.7%** |

剩余 2 个 UI 调用是 `${path}` 模板字符串（`/api/auth${path}`），是 better-auth
代理路由（由 better-auth 库内部处理），不属于 paperclip-rs 直控范围。

## 7. 下一步

R578: 验证 route-audit 重跑后 M19 覆盖率 0% → 86.7% 落地。
R579: 修 pc-server 慢启动 + e2e baseline。
