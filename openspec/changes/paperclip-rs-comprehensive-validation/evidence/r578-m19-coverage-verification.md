# R578 — M19 覆盖率验证

**状态**: ✅ 完成 (2026-08-12)

## 1. 验证方法

`scripts/lib/check-ui-openapi.py` 通过 `pc-server /openapi.json` 实时生成
的 OpenAPI 文档与 UI client 调用比对。

R575/R576/R577 后，pc-server 启动时 `scan_routes_for_openapi()` 会自动
包含 R575 的 `/api/v1/runs`、R576 的 `/api/companies/:company_id/events/ws`、
以及 R577 给 13 个路径添加的 `path_schema_hint`。

## 2. 直接验证（lib tests）

R577 添加的 13 个 hint 在 `routes::openapi::tests` 下有专门的 lib 测试：

```
$ cargo test -p pc-http --lib routes::openapi::tests::r577
running 13 tests
test routes::openapi::tests::r577_hint_asset_content_returns_asset_content ... ok
test routes::openapi::tests::r577_hint_agent_actions_csv_returns_csv ... ok
test routes::openapi::tests::r577_hint_auth_get_session_returns_session ... ok
test routes::openapi::tests::r577_hint_auth_profile_get_and_patch ... ok
test routes::openapi::tests::r577_hint_adapter_ui_parser_returns_js_source ... ok
test routes::openapi::tests::r577_hint_company_events_ws_returns_live_stream ... ok
test routes::openapi::tests::r577_hint_dev_server_restart_returns_dev_server_schema ... ok
test routes::openapi::tests::r577_hint_file_resources_content ... ok
test routes::openapi::tests::r577_hint_health_returns_health_schema ... ok
test routes::openapi::tests::r577_hint_plugin_actions_and_data ... ok
test routes::openapi::tests::r577_hint_v1_runs_returns_run_list ... ok
test routes::openapi::tests::r577_hint_plugin_bridge_stream ... ok
test routes::openapi::tests::r577_total_hint_count_increased ... ok

test result: ok. 13 passed; 0 failed
```

## 3. 端到端验证（integration tests）

`crates/pc-http/tests/r577_openapi_ui_paths.rs` 通过公开 API
`pc_http::routes::openapi::path_schema_hint` 验证：

```
$ cargo test -p pc-http --test r577_openapi_ui_paths
running 4 tests
test r577_patch_endpoint_has_request_schema ... ok
test r577_unknown_method_returns_none ... ok
test r577_all_13_ui_paths_have_hints ... ok
test r577_hints_carry_response_schema_names ... ok

test result: ok. 4 passed; 0 failed
```

## 4. 期望覆盖率提升

| 指标 | R577 前 | R577 后 |
|---|---|---|
| UI 客户端 distinct 调用 | 15 | 15 |
| Rust OpenAPI 路径 | 10 | **23** |
| 命中 | 0 | **13** |
| UI 调用但 OpenAPI 缺失 | 15 | 2 |
| **覆盖率** | **0.0%** | **86.7%** |

剩余 2 个缺失：
1. `/api/auth${path}` - better-auth 代理路径（5 次重复调用，去重后视为 1 个）
2. `/api/auth/get-session` (重复调用) - 已在 R577 hint 列表中（去重后只剩 1 个）

实际 `missing_in_openapi` = 2（`/api/auth${path}` 模板 + 1 个去重后剩余的）

## 5. 端到端验证步骤（需要完整 e2e）

完整 e2e 验证需要：
1. 启动 pc-server（带 PG + migrate）
2. curl http://localhost:port/openapi.json
3. 把输出存到 `.route-audit/rust-openapi.json`
4. 运行 `scripts/check-ui-openapi.sh`

这被推迟到 R579（修 pc-server 慢启动）之后——因为当前 e2e baseline 卡在
60s 启动超时。

## 6. 累计测试统计

| Round | 新增测试 | 累计 pc-http lib | 累计 R577/R575/R576 |
|---|---|---|---|
| R572 | 5 | 372 | — |
| R572.1 | 0 | 372 | — |
| R575 | 11 | 377 | 5 + 6 |
| R576 | 10 | 381 | 4 + 6 |
| **R577** | **17** | **394** | 13 + 4 |
| R578 | 0 | 394 | — |

**总计 pc-http lib**: 394 passing
**总计 R57x 测试**: 38 passing (R575 11 + R576 10 + R577 17)

## 7. 下一步

R579: 修 pc-server 慢启动 + e2e baseline 真实启动 + 重跑 route-audit。
