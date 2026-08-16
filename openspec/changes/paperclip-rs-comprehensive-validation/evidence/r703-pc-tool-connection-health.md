# R703 — pc-tool connection_health (sanitize_http_failure) (2026-08-16)

## 目标

补足 Node `services/tool-access.ts::sanitizeHttpFailure`
(7,028 行 Node monolith 中第二个核心 pure function)。

## 设计

- **新 submodule**: `crates/pc-tool/src/connection_health.rs` (159 行)
- **新公开 API**:
  - `sanitize_http_failure(&HttpErrorLike) -> SanitizedHealthFailure`
  - `sanitize_runtime_error(&dyn Error) -> SanitizedHealthFailure`
  - `sanitize_unknown_failure() -> SanitizedHealthFailure`
  - `ToolConnectionHealthStatus` enum (Healthy/Unhealthy/Error/Failed/MissingSecret/Unknown)
  - `HttpErrorLike` struct (status + message + code)
- **优先级 (与 Node 完全一致)**:
  1. `code == "oauth_challenge"` → Error
  2. `code == "oauth_refresh_missing"` → Failed
  3. `code in {binding_missing, secret_deleted, secret_inactive, version_missing}` → MissingSecret
  4. `status == 404 && /secret/i.test(message)` → MissingSecret
  5. fallback → Error (paperclip_error)

## 测试

```
running 13 tests
test connection_health::internal_tests::oauth_challenge ... ok
test connection_health::internal_tests::oauth_refresh_missing ... ok
test connection_health::internal_tests::binding_missing ... ok
test connection_health::internal_tests::secret_deleted ... ok
test connection_health::internal_tests::secret_inactive ... ok
test connection_health::internal_tests::version_missing ... ok
test connection_health::internal_tests::http_404_with_secret_in_message ... ok
test connection_health::internal_tests::http_404_without_secret_in_message ... ok
test connection_health::internal_tests::paperclip_error_fallback ... ok
test connection_health::internal_tests::runtime_error_truncates_at_240 ... ok
test connection_health::internal_tests::unknown_failure_default ... ok
test connection_health::internal_tests::status_as_str_matches_node ... ok
test connection_health::internal_tests::status_serde_snake_case ... ok

test result: ok. 13 passed; 0 failed
```

## 关键 parity 验证

- `sanitize_http_failure` - 5 优先级分支 1:1 复刻 Node
- `sanitize_runtime_error` - 240 字符截断与 Node slice(0, 240) 一致
- `sanitize_unknown_failure` - 默认 fallback 与 Node fallback 一致
- `ToolConnectionHealthStatus` - 6 状态值与 Node ToolConnectionHealthStatus 1:1
- serde `rename_all = "snake_case"` 镜像 Node wire format

## R703 关键交付

- [x] connection_health.rs 模块 + 13 个单测 PASS
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `sanitizeHttpFailure` 100% parity
- [x] 真实验证 (cargo test)

## 累计 R700-R703 成果

- **R700**: 全量差距分析 (4028 bytes)
- **R701**: pc-tool/risk classify (11 tests)
- **R702**: pc-execution-workspace-guards/readiness (20 tests)
- **R703**: pc-tool/connection_health (13 tests)
- **总计**: 44 个新单测 PASS, ~630 行新增代码

## 下一步

- R704 — pc-tool descriptor_hash (stable hash for catalog diff)
- R705 — pc-execution-workspace-guards normalize_adapter_managed_runtime_services
- R706 — pc-tool profile_entry_matches (Node selector matching)

