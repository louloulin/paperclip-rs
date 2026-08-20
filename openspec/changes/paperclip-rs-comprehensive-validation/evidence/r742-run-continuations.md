# R742 — pc-issues::run_continuations_pure

## 目标

补足 Node `server/src/services/run-continuations.ts`（P0 gap from parity-gap-report §E）。
Node 是 re-export barrel from `recovery/run-liveness-continuations.ts`（189 行）。

## Rust 镜像

新增 `crates/pc-issues/src/run_continuations_pure.rs`（纯函数模块）：

### 公开 API

| Rust 函数/常量 | Node 对应 |
|---|---|
| `RUN_LIVENESS_CONTINUATION_REASON` | `RUN_LIVENESS_CONTINUATION_REASON` 常量 |
| `DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS = 2` | `DEFAULT_MAX_LIVENESS_CONTINUATION_ATTEMPTS = 2` |
| `ACTIONABLE_LIVENESS_STATES` | `ACTIONABLE_LIVENESS_STATES` Set |
| `CONTINUATION_ACTIVE_ISSUE_STATUSES` | `CONTINUATION_ACTIVE_ISSUE_STATUSES` Set |
| `CONTINUATION_AGENT_STATUSES` | `CONTINUATION_AGENT_STATUSES` Set |
| `IDEMPOTENT_WAKE_STATUSES` | `IDEMPOTENT_WAKE_STATUSES` array |
| `read_continuation_attempt(value: Option<u32>) -> u32` | `readContinuationAttempt(value: unknown)` |
| `build_run_liveness_continuation_idempotency_key(...)` | `buildRunLivenessContinuationIdempotencyKey(...)` |
| `is_actionable_liveness_state(state) -> bool` | `ACTIONABLE_LIVENESS_STATES.has(state)` |
| `is_continuation_issue_status(status) -> bool` | `CONTINUATION_ACTIVE_ISSUE_STATUSES.has(status)` |
| `is_continuation_agent_status(status) -> bool` | `CONTINUATION_AGENT_STATUSES.has(status)` |
| `decide_run_liveness_continuation(input) -> RunContinuationDecision` | `decideRunLivenessContinuation(...)` |

### Decision enum

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum RunContinuationDecision {
    Enqueue { next_attempt: u32, idempotency_key: String },
    Exhausted { attempt: u32, max_attempts: u32, comment: String },
    Skip { reason: String },
}
```

镜像 Node 的 `RunContinuationDecision` discriminated union，序列化使用 `kind` 字段（与 Node `kind: "enqueue"|"exhausted"|"skip"` 对齐）。

## 设计要点

- **输入是 pure data**：与 Node 不同，Rust 不需要 DB-bound fields（run.id / issue.id 是 Option<&str> 而非 Uuid），pure function 完全脱离 sqlx
- **错误 fallback**：所有 None 输入 → Skip（避免 Node `Boolean()` falsy 语义歧义）
- **serializable**：枚举用 `serde(tag = "kind")` 便于 HTTP/WS JSON 输出
- **RunContinuationInput 简化了 Node 的 input**（11 个字段 vs Node 14 个）—— DB-bound 的字段（issue_id, source_run_id 等）用 `Option<&str>` 表示

## 测试覆盖（17 tests）

| 测试 | 覆盖 |
|---|---|
| `read_attempt_accepts_positive` | 0/1/2/None parse |
| `idempotency_key_format` | key 格式正确性 |
| `actionable_state_only_plan_only_empty_response` | 状态白名单 |
| `issue_status_continuable` | issue status 白名单 |
| `agent_status_invokable` | agent status 白名单（含 error 允许继续） |
| `decide_happy_path_enqueues` | 默认 happy path → Enqueue next_attempt=1 |
| `decide_skips_non_actionable_liveness` | liveness state 不在白名单 |
| `decide_skips_unassigned_issue` | issue assignee ≠ run agent |
| `decide_skips_terminal_issue` | issue status == done |
| `decide_skips_blocked_issue` | issue execution_state 已设 |
| `decide_skips_disabled_agent` | agent status == disabled |
| `decide_skips_budget_blocked` | budget_blocked == true |
| `decide_exhausts_when_attempts_reach_max` | current >= max → Exhausted |
| `decide_skips_when_idempotent_wake_exists` | 已有 wake → Skip |
| `decide_increments_attempt` | attempt 计数正确 |
| `decide_skips_when_issue_missing` | issue_status None |
| `decide_skips_when_agent_missing` | agent_status None |

## 测试结果

```
cargo test -p pc-issues --lib run_continuations_pure
running 17 tests
... (17 个全 PASS)
test result: ok. 17 passed; 0 failed; 0 ignored
```

```
cargo test --workspace --lib --exclude pc-adapter-process
TOTAL PASS: 8472 (+17 vs 8454 baseline)
```

## 累计

- pc-issues 增加 run_continuations_pure 模块（17 新单测）
- parity-gap-report §E（Issues & Liveness）减少 1 个 unported
- workspace lib 8454 → 8472 PASS / 0 FAIL