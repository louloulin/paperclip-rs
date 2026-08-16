# R657 (2026-08-16) — 修测试 setup + bearer_webhook 端到端真实 PG 通过

## 背景

上一轮 handoff 指出 `crates/pc-http/tests/routines_http_contract.rs::bearer_webhook_trigger_encrypts_secret_and_fires_idempotently` 在 line 675 处 panic。本轮真实诊断 + 一行最小修复 + 跑全部 8 个测试。

## 根因

调用 `routes::routines::router().with_state(test_state(db.clone()))` 后没有安装 auth middleware。handler 内部用 `AxumExtension(actor): AxumExtension<AuthContext>`（axum 内置 Extension 提取器），所以在测试中无法找到 extension：

```
Missing request extension: Extension of type `pc_auth::AuthContext` was not found.
Perhaps you forgot to add it? See `axum::Extension`.
```

导致 **全部 8 个测试** 失败——同一根因。生产环境 pc-server 用 `middleware::auth::auth_layer` 中间件注入 extension；测试 setup 缺失该层。

## 修复（测试 setup only，非产品代码）

在 `crates/pc-http/tests/routines_http_contract.rs` 内新增辅助函数 `build_app`，并替换 8 个调用点：

```rust
fn build_app(db: Db) -> axum::Router {
    // 测试 setup only: 安装 axum::Extension(AuthContext::system())
    // 让 handler 中的 AxumExtension(actor): AxumExtension<AuthContext> 提取能命中。
    // 真实启动时由 middleware::auth::auth_layer 注入（参见 pc-server）。
    routes::routines::router()
        .with_state(test_state(db.clone()))
        .layer(axum::Extension(pc_auth::AuthContext::system()))
}
```

替换 8 处 `routes::routines::router().with_state(test_state(db.clone()))` → `build_app(db.clone())`：

- L115, L187, L227, L283, L373, L440, L542, L672（原始行号）

用 `system()` 而非 `anonymous()` 是因为 `enforce_permission` 对 anonymous 返回 forbidden (`Actor::Anonymous => false`)，System 有全公司访问权限。

## 验证

```
\$ cargo test -p pc-http --test routines_http_contract
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

之前 8 个 0/8 → 现在 **8/8 PASS**。具体用例：

- `bearer_webhook_trigger_encrypts_secret_and_fires_idempotently` ← R657 核心目标
- `company_routine_create_uses_ui_contract_and_creates_initial_revision`
- `company_routine_list_filters_by_project_and_returns_ui_aggregates`
- `manual_run_creates_execution_issue_heartbeat_and_enriched_run_views`
- `routine_detail_includes_description_document_and_relationship_aggregates`
- `routine_revision_restore_uses_revision_id_and_preserves_history`
- `schedule_trigger_creation_appends_revision_and_returns_ui_wrapper`
- `trigger_update_and_delete_append_revisions_with_exact_snapshots`

中间调试痕迹清理完成：移除 handoff 留下的 `[DBG-TEST]` / `[DBG-WEBHOOK-CREATE]` / `PROBE_CREATE` eprintln/println 调试行。

## 同时落地

- 全 workspace lib: 2539 PASS / 1 FAIL（1 FAIL 是与本次工作无关的预存在 `pc-adapter-process::graceful_tests::terminate_with_grace_handles_quick_exit`）
- pc-routines: 110 PASS / 0 FAIL
- pc-realtime: 94 PASS / 0 FAIL

## 下一步

R657b — 真实启动 pc-server + curl 触发 webhook trigger 端点：
- 启动临时 PG + pc-migrate + pc-server
- 通过 `POST /api/routine-triggers/public/:publicId/fire` 触发 bearer + hmac_sha256 模式
- 验证 `x-timestamp` replay window 行为
- 验证 Secret 在 DB 中加密存储

## 修改文件清单

- `crates/pc-http/tests/routines_http_contract.rs`:
  - 新增 `build_app` helper（含 `pc_auth::AuthContext::system()` 注入）
  - 替换 8 处 router 构造
  - 加 `use pc_auth as _;`
  - 清理调试行
