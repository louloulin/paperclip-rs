# R412：codex-local 错误分类与重试提示复刻

## 差距依据

Node 参考：

- `paperclip/packages/adapters/codex-local/src/server/parse.ts`
- `paperclip/packages/adapters/codex-local/src/server/parse.test.ts`

Rust 原有 `parse_codex_jsonl` 已能解析 thread、agent message 和 usage，但缺少 Node parser 的协议状态判断、stale session、OAuth refresh 分类、quota/transient 互斥以及 usage-limit retry 时间。

## 实现

新增 `crates/pc-adapter-codex-local/src/codex_errors.rs`：

- `CodexProtocolState` 和 `is_codex_harness_crash`
- `is_codex_unknown_session_error`
- `classify_codex_auth_refresh_failure`
- `is_codex_provider_quota_error`
- `is_codex_transient_upstream_error`
- `extract_codex_retry_not_before`
- `CodexAuthRefreshFailureClass` 枚举

实现保持 Node 的关键策略：

- 协议已启动但非零退出且没有终态，才视为 harness crash。
- usage limit/capacity 不归类为普通 transient。
- remote compact + high demand/temporary errors 才进入自动 transient 重试范围。
- bare 401 不直接归类为 refresh token 失效，必须有 OAuth/credential 上下文。

## 验证

- `cargo test -p pc-adapter-codex-local`：全量通过。
- `cargo test -p pc-adapter-codex-local --test round412_codex_errors`：7 passed。
- 覆盖 harness crash、rollout stale session、OAuth 三类 refresh failure、usage quota、remote compaction transient、capacity quota 和次日 retry。
- `cargo check --workspace`：workspace 编译验证。

## 剩余差距

- 已加入 `chrono-tz` IANA timezone wall-clock 转换，并覆盖 `America/Chicago` 集成测试；DST 边界仍需更多日期 fixture。
- Codex parser 的错误分类尚未全部接入 heartbeat recovery policy。
- Codex execute 仍需要真实 CLI/API 环境下的远程 resume、OAuth 失效和 remote compaction smoke 验证。
