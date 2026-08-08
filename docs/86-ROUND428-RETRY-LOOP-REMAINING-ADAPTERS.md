# R428 — 其余本地适配器复刻「未知会话真实重跑」回路

> 目标：在 R426（pi-local）/ R427（claude-local）基础上，补齐 codex / cursor / opencode / gemini / grok
> 五个 local adapter 的「resume 失败 → 自动去掉 resume 参数重跑一轮」行为，与 Node `execute.ts` 对齐。

## 背景

R427 已为 claude-local 接入 `errorFamily` / `stopReason` / `clearSession` 决策；
R428 前，其余 5 个 adapter 的 `execute()` **只跑一次**：

- 有 `context.session_id` 时 build 函数甚至不接收 resume 参数（cursor/opencode/gemini/grok 的 build 缺参）；
- codex 虽已接收 `resume_session_id`，但失败后从不重跑，也不写 `errorFamily`。

这导致真实 CLI 场景下「旧 session 失效 → 必须手工清 session 重试」，与 Node 的
`isXxxUnknownSessionError → runAttempt(null)` 行为不一致。

## 本次改动（高内聚低耦合）

每个 adapter 独立成模块，互不引用：

| Adapter | build 函数 | 分类器 | execute 重跑 | 新增端到端测试 |
|---|---|---|---|---|
| codex-local | 已支持 `resume_session_id`（不变） | `decide_codex_retry`（R428 前已有） | 接入 unknown-session 重跑 + `errorFamily` | `tests/round428_codex_retry_loop.rs` |
| cursor-local | `build_cursor_exec_args(config, resume_session_id)` | `is_cursor_unknown_session_error` | `--resume <sid>` 首跑；失败重跑无 `--resume` | `tests/round428_cursor_retry_loop.rs` |
| opencode-local | `build_opencode_exec_args(config, resume_session_id)` | `is_opencode_unknown_session_error` | `--session <sid>` 首跑；失败重跑无 `--session` | `tests/round428_opencode_retry_loop.rs` |
| gemini-local | `build_gemini_exec_args(config, resume_session_id)` | `is_gemini_session_unrecoverable_error` | `--resume <sid>` 首跑；失败重跑无 `--resume` | `tests/round428_gemini_retry_loop.rs` |
| grok-local | `build_grok_exec_args(config, resume_session_id)` | `is_grok_unknown_session_error` | `--resume <sid>` 首跑；失败重跑无 `--resume` | `tests/round428_grok_retry_loop.rs` |

统一行为（对齐 Node）：

- 首轮 attempt 使用 `context.session_id` 构造 resume 参数；
- 仅当「有 session_id + 未超时 + exit≠0 + 分类器命中」才触发一轮重跑；
- 重跑重新构造 args，**去掉 resume 参数**，使用 fresh session；
- `result_json.retriedAfterUnknownSession` 标记是否发生过重跑；
- `clear_session`：重跑后仍无新 session id 时为 `true`（Node 的 `clearSessionOnMissingSession && !resolvedSessionId`）；
- codex 额外在 `result_json` 写入 `errorFamily`（成功重试后为空字符串，Node null）。

## 真实验证方式

mock CLI 脚本每次调用将 `$@` 追加到 `calls.log`，利用调用次数（`wc -l < calls.log`）区分首跑/重跑，
断言：

1. 首跑确实带 `--resume` / `--session` / `resume` 参数；
2. 重跑确实去掉该参数；
3. 恰好调用 2 次（非 unknown-session 失败只调用 1 次）；
4. 成功重跑后拿到新 session id、summary 正确、`retriedAfterUnknownSession=true`；
5. 重跑仍失败且无新 session → `clear_session=true`。

## 验证结果

```sh
cargo test -p pc-adapter-codex-local -p pc-adapter-cursor-local \
           -p pc-adapter-opencode-local -p pc-adapter-gemini-local \
           -p pc-adapter-grok-local
```

全部通过（新增 11 项端到端用例 + 既有单测无回归）。

## 与 paperclip（Node）的差距评估

| 维度 | Node 现状 | Rust 现状 | 差距 |
|---|---|---|---|
| resume 失败重跑 | ✅ 全 adapter 都有 | ✅ 本次补齐 5 个 + R426/427 的 pi/claude | 已对齐 |
| errorFamily / clearSession | ✅ | ✅ codex/claude；cursor/opencode/gemini/grok 仅 `retriedAfterUnknownSession` | 小 |
| 超时处理 | ✅ `Timed out after Xs` | ✅ `execute_process_capture` 超时 | 已对齐 |
| remote/sandbox 执行目标 | ✅ 支持 | ⚠️ 尚未完整接入 `execution_target` | **中** |
| quota probe（Codex） | ✅ `quota.ts` | ⚠️ 仅分类，未做探针 | **中** |
| 输出活动监控（monitor） | ✅ | ❌ 未实现 | **大** |
| skills 同步 | ✅ | ✅ R392 已有 | 已对齐 |
| prompt 注入 | ✅ | ✅ R425 | 已对齐 |

## 后续计划（R429+）

1. **R429：真机 smoke 验证** —— 用真实 `codex` / `cursor-agent` / `opencode` / `gemini` / `grok` CLI
   跑一轮成功 + 一轮未知会话场景，验证 `calls.log` 之外的端到端行为。
2. **R430：Codex quota probe** —— 复刻 `quota.ts`（fetchQuota / getQuotaWindows / CodexQuotaAuthError），
   让 provider_quota 具备可观测的 retryNotBefore。
3. **R431：输出活动监控** —— 复刻 monitor（elapsedMsSinceLastEvent / terminationSignal / timeoutMs）。
4. **R432：execution_target 完整接入** —— remote/sandbox 目标身份传递到 sessionParams。
5. 之后进入业务领域模块（db / server / ui 层 Rust 化）。
