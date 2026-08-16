# R702 — pc-execution-workspace-guards readiness helpers (2026-08-16)

## 目标

补足 Node `services/workspace-runtime.ts` (5,178 行 monolith) 中的 pure helpers。

## 设计

- **新 submodule**: `crates/pc-execution-workspace-guards/src/readiness.rs` (194 行)
- **不破坏既有 lib.rs**: 新增 `pub mod readiness;` 注入
- **不引入 runtime git CLI**: 只移植 pure 函数
- **输入**: `HashMap<String, serde_json::Value>` (与 Node service.config 同形)
- **公开 API**:
  - `format_short_sha(Option<&str>) -> String` (3 行 Node, 1:1 parity)
  - `looks_like_workspace_dev_server_command(&str) -> bool` (8 行 Node, regex 1:1)
  - `resolve_workspace_runtime_readiness_timeout_sec(&HashMap) -> u32` (5 行 Node)
  - `is_paperclip_dev_runtime_service(Option<&str>, Option<&str>) -> bool` (5 行 Node)

## 测试

```
running 20 tests
test readiness::internal_tests::format_short_sha_basic ... ok
test readiness::internal_tests::format_short_sha_empty ... ok
test readiness::internal_tests::format_short_sha_none ... ok
test readiness::internal_tests::format_short_sha_short_input ... ok
test readiness::internal_tests::is_paperclip_dev_case_insensitive ... ok
test readiness::internal_tests::is_paperclip_dev_command_marker ... ok
test readiness::internal_tests::is_paperclip_dev_service_name ... ok
test readiness::internal_tests::looks_dev_inside_larger_command ... ok
test readiness::internal_tests::looks_does_not_match_case ... ok
test readiness::internal_tests::looks_does_not_match_build ... ok
test readiness::internal_tests::looks_handles_empty ... ok
test readiness::internal_tests::looks_like_npm_yarn_bun_dev ... ok
test readiness::internal_tests::looks_like_pnpm_dev ... ok
test readiness::internal_tests::looks_handles_whitespace ... ok
test readiness::internal_tests::resolve_timeout_dev_command_default ... ok
test readiness::internal_tests::resolve_timeout_empty_service ... ok
test readiness::internal_tests::resolve_timeout_explicit ... ok
test readiness::internal_tests::resolve_timeout_explicit_clamped ... ok
test readiness::internal_tests::resolve_timeout_explicit_negative_clamped_to_1 ... ok
test readiness::internal_tests::resolve_timeout_other_command_default ... ok

test result: ok. 20 passed; 0 failed
```

## 关键 parity 验证

- `format_short_sha` - 12 字符截断 + null/empty 走 "unknown" 分支
- `looks_like_workspace_dev_server_command` - 完整 token 匹配 (避免 "pnpm devtools" 误匹配)
- `resolve_workspace_runtime_readiness_timeout_sec` - 优先级: explicit > dev-server > default
- `is_paperclip_dev_runtime_service` - serviceName OR (dev:once + tailscale-auth)

## R702 关键交付

- [x] readiness.rs 模块 + 20 个单测 PASS
- [x] lib.rs 接入 + 公开 re-export
- [x] Node `formatShortSha`/`looksLikeWorkspaceDevServerCommand`/`resolveWorkspaceRuntimeReadinessTimeoutSec`/`isPaperclipDevRuntimeService` 100% parity
- [x] 真实验证 (cargo test)
- [x] 新增 Cargo.toml 依赖: serde_json

## 累计 R700-R702 成果

- **R700**: 全量差距分析 (4028 bytes)
- **R701**: pc-tool/risk classify (11 tests)
- **R702**: pc-execution-workspace-guards/readiness (20 tests)
- **总计**: 31 个新单测 PASS, ~470 行新增代码

## 剩余 workspace-runtime.ts 缺口

- ~5,000 行 Node 仍待复刻
- 大部分是 git CLI wrapper (`runGit`, `refreshRemoteTrackingBaseRef`)
- 业务逻辑层（readiness, normalize, format）持续推进

