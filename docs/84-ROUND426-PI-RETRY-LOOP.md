# R426 — pi-local 完整 retry 主循环

## 目标

复刻 `packages/adapters/pi-local/src/server/execute.ts` 的核心 retry 逻辑：
- 解析 `runtime.session_id` 与本地 session 文件头里的 cwd，判断能否 resume；
- 不能 resume 时生成新的 session 文件路径（本地 `~/.pi/paperclips/`）；
- 失败且 stdout/stderr 命中 `isPiUnknownSessionError` 时自动用新 session 路径再跑一次，
  最终 `clearSession = true`。

## 主要改动

| 文件 | 改动 |
| --- | --- |
| `crates/pc-adapter-pi-local/src/execute_helpers.rs` | 新增 `build_session_path` / `build_remote_session_path` / `decide_resume` / `DecideResumeInput` / `retry_after_unknown_session` / `RetryAfterUnknownInput` / `RetryDecision` / `paperclip_sessions_dir` / `current_iso_timestamp`；保留 `model_provider` / `model_id` / `resolve_pi_biller` / `parse_session_header_cwd` / `should_clear_session` / `should_resume` 既有 helper。补充 13 项单元测试覆盖 session path 生成、resume 决策、retry 决策。 |
| `crates/pc-adapter-pi-local/src/lib.rs` | `PiLocalAdapter::execute` 现在读取 `context.session_id` 调 `decide_resume`；构造本地 session path；跑首轮 attempt；命中 `retry_after_unknown_session` 时用新 session path 再跑一次；最终 `result.result_json` 增加 `sessionPath` 与 `retriedAfterUnknownSession`，`clearSession` 综合 retry 与未知 session 错误两个来源。 |
| `crates/pc-adapter-pi-local/tests/round426_pi_retry_loop.rs` | 3 个端到端集成测试：首轮成功不重试、首轮 unknown-session 触发重试且清 session、首轮 rate-limit 不触发重试。 |

## Node 等价性

| 行为 | Node 路径 | Rust 实现 |
| --- | --- | --- |
| `canResumeSession = true` 时使用 `runtimeSessionId` | `execute.ts` L470 | `if runtime_session_id 非空 + saved_cwd 匹配 → 用 runtime_session_id` |
| 否则用 `buildSessionPath(agentId, now())` | L121-123 | `build_session_path(&sessions_dir, "agent-local", &current_iso_timestamp())` |
| `buildRemoteSessionPath` 留待 R428 | L125-127 | helper 已就位 |
| 首轮失败 + `isPiUnknownSessionError(stdout, rawStderr)` → 二次 attempt | L790-815 | `retry_after_unknown_session(...)` 决定 + execute 重新 `execute_process_capture` |
| 重试成功 → `clearSession = true` | L815 | `decision.clear_session_on_retry` |
| 重试日志 `[paperclip] Pi session ... is unavailable; retrying ...` | L796 | 留待 R428 接入 `on_log`；当前 mock CLI 不需要 |

## 测试矩阵

- `pc-adapter-pi-local::execute_helpers::tests` 新增 13 项（`build_session_path_替换不安全字符`、`decide_resume_全部匹配` 等）。
- `round426_pi_retry_loop.rs` 3 项端到端集成测试。

## 验证

- `cargo check --workspace --tests`：0 errors / 391 warnings。
- `cargo test -p pc-adapter-pi-local`：142 + 3 passed。

## 后续

- R427：claude-local 完整 retry loop（含 `claudeTransientHandoffNote`、`poisonedPreviousMessageId` 等子路径）。
- R428：codex-local + 其余 5 个 adapter retry loop；接入远程 execution_target（managed home staging 等）。
