# R744 — pc-environment::run_orchestrator_pure

## 目标

补足 Node `server/src/services/environment-execution-target.ts` (259 行) + `environment-run-orchestrator.ts` (609 行) 的 pure 部分（P0 gap from parity-gap-report §G）。

## Rust 镜像

新增 `crates/pc-environment/src/run_orchestrator_pure.rs`：

### 公开 API

| Rust 函数/类型 | Node 对应 |
|---|---|
| `EnvironmentErrorCode` enum (10 variants) | `EnvironmentErrorCode` type union |
| `EnvironmentErrorCode::as_str() -> &'static str` | 字符串字面量 |
| `first_non_empty_line(text) -> Option<String>` | `firstNonEmptyLine(text)` |
| `ProvisionFailure` struct | `formatProvisionFailureDetail` 入参 |
| `format_provision_failure_detail(&ProvisionFailure) -> String` | `formatProvisionFailureDetail(result)` |

## 设计要点

- **typed API**：Node 的 `unknown` 参数 → Rust `Option<&str>` / `String` 类型安全
- **CRLF + LF 兼容**：`first_non_empty_line` 使用 `split(['\r', '\n'])` 同时处理 Unix / Windows 换行
- **trim signal**：signal 字段自动 trim（Node 行为）
- **stderr 优先**：与 Node 一致（stderr 第一个非空行 → 回退 stdout）
- **fail-fast on timeout**：`timed_out = true` 跳过 exit code 检查，直接返回 timed_out 信息

## 测试覆盖（13 tests）

| 测试 | 覆盖 |
|---|---|
| `error_code_as_str_matches_node` | 10 个 error code 字符串字面量 |
| `first_non_empty_line_returns_first_trimmed` | LF separator |
| `first_non_empty_line_handles_crlf` | CRLF separator |
| `first_non_empty_line_returns_none_for_empty` | None / 空 / 全空白 |
| `format_failure_timed_out` | timed_out 优先级 |
| `format_failure_exit_code_only` | 仅 exit_code |
| `format_failure_exit_code_null` | exit_code = None |
| `format_failure_with_signal` | signal 字段 |
| `format_failure_includes_stderr` | stderr 第一非空行 |
| `format_failure_stderr_priority_over_stdout` | stderr 优先 stdout |
| `format_failure_falls_back_to_stdout` | stdout fallback |
| `format_failure_with_signal_and_stderr` | signal + stderr 组合 |
| `format_failure_signal_trimmed` | signal 自动 trim |

## 测试结果

```
cargo test -p pc-environment --lib run_orchestrator_pure
running 13 tests
... (13 个全 PASS)
test result: ok. 13 passed; 0 failed; 0 ignored
```

```
cargo test --workspace --lib --exclude pc-adapter-process
TOTAL PASS: 8505 (+13 vs 8492 baseline)
```

## 累计

- pc-environment 增加 run_orchestrator_pure 模块（13 新单测 + 1 enum + 1 struct）
- parity-gap-report §G（Environments）减少 2 个 unported（environment-execution-target + environment-run-orchestrator）
- workspace lib 8492 → 8505 PASS / 0 FAIL