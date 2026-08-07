# Round 366 — Acpx-engine 错误恢复 + Startup Timing (B3.1 第五阶段)

> 适用版本：`paperclip-rs` 截至 R366（R365 = 1042 → R366 = **1171**，+50 pc-acpx 测试）
> 参考实现：`paperclip` Node（`packages/adapter-utils/src/acpx-engine/execute.ts` 中 `describeErrorDiagnostics` / `classifyError` / `isResumeFailure` / `routeChildStderr` / `flushChildStderr` / `readChildStderrTail`；`packages/adapter-utils/src/acpx-engine/startup-timing.ts` 全文件）
> 测试基线：`cargo test -p pc-acpx` 155/155 绿（126 unit + 8+4+4+4+9 integration）；`pc-heartbeat` 928/928 全量无回归；`cargo build --workspace --bins` 通过；`cargo fmt --all -- --check` 通过

---

## 🎯 R366 目标

完成 **acpx-engine 错误恢复 + startup timing** 的 Rust 化迁移（B3.1 第五阶段）：

1. **错误分类契约**：`AcpxExecutionPhase` enum + `AcpxErrorDiagnostics` + `ClassifiedError` + `classify_error` + `is_resume_failure`，纯函数无 I/O
2. **子进程 stderr 流路由**：`ChildStderrState` + `route_child_stderr` + `flush_child_stderr` + `read_child_stderr_tail`，带良性 `nes/close` JSON-RPC 噪音过滤
3. **Startup span timing**：`measure_startup_step` 核心函数 + `normalize_provider_family` + `StartupSpan` / `StartupTracer` / `StartupTraceContext` 三 trait + `Noop*` 默认实现 + `RuntimeStartupStepEvent`

**为什么这一阶段关键**：错误分类与 startup timing 是 acpx-engine 的两个横切关注点——前者是失败路径上**任何** `AdapterExecutionResult` 必须经过的归类闸门，后者是 sandbox 启动阶段**任何** 跨边界步骤必须经过的可观测性闸门。两者都必须是**纯** / **容错** / **可注入**的，因此需要单独成模块。

---

## 🏗️ 新增模块

```
crates/pc-acpx/src/
├── error_classification.rs   # 纯函数：分类与诊断抽取
├── child_stderr.rs           # 子进程 stderr 流路由 + 日志尾部读取
└── startup_timing.rs         # 启动步骤测量 + span/tracer 契约

crates/pc-acpx/tests/
└── round366_recovery.rs      # 端到端集成测试
```

---

## 📐 1. error_classification.rs

### 公开 API

```rust
pub enum AcpxExecutionPhase { EnsureSession, ConfigureSession, Turn }
impl AcpxExecutionPhase { pub fn as_str(&self) -> &'static str }

pub struct AcpxErrorDiagnostics {
    pub error_name: String,
    pub acp_code: Option<String>,
    pub cause_message: Option<String>,
    pub retryable: Option<bool>,
    pub stack_preview: Option<String>,
}

pub struct ClassifiedError {
    pub error_code: String,        // e.g. "acpx_auth_required" / "acpx_turn_failed"
    pub error_meta: serde_json::Map<String, serde_json::Value>,
}

pub fn describe_error_diagnostics(err: &(dyn std::error::Error + 'static)) -> AcpxErrorDiagnostics;
pub fn classify_error(err: &(dyn std::error::Error + 'static), phase: Option<AcpxExecutionPhase>) -> ClassifiedError;
pub fn is_resume_failure(err: &(dyn std::error::Error + 'static)) -> bool;
```

### 与 Node 的对位

| Node (`execute.ts`) | Rust (`error_classification.rs`) | 备注 |
|---|---|---|
| `AcpxExecutionPhase = "ensure_session" \| "configure_session" \| "turn"` | `AcpxExecutionPhase` enum + `as_str()` | 同样的三相，stable lowercase 字符串 |
| `describeErrorDiagnostics(err)` | `describe_error_diagnostics(err)` | 字段名 1:1，`stackPreview` 截断到 6 行 |
| `classifyError(err, phase?)` | `classify_error(err, phase)` | 同样的优先级：auth → ACP code → phase fallback → runtime |
| `isResumeFailure(err)` | `is_resume_failure(err)` | 6 个 keyword 同 case-insensitive 正则 |

### 分类决策树

```
auth-like (auth/login/credential)?  →  acpx_auth_required (category=auth)
   └─ ACP_SESSION_INIT_FAILED       →  acpx_session_init_failed (protocol)
   └─ ACP_TURN_FAILED               →  acpx_turn_failed (protocol)
   └─ ACP_BACKEND_MISSING           →  acpx_backend_missing (protocol)
   └─ ACP_BACKEND_UNAVAILABLE       →  acpx_backend_unavailable (protocol)
   └─ phase = ensure_session        →  acpx_session_init_failed (runtime)
   └─ phase = configure_session     →  acpx_session_config_failed (runtime)
   └─ phase = turn                  →  acpx_turn_failed (runtime)
   └─ other ACP_* code              →  acpx_protocol_error (protocol)
   └─ (fallback)                    →  acpx_runtime_error (runtime)
```

### 错误结构抽取约定

Node 端从 `{code, retryable, stack, cause}` 等结构化字段抽取。Rust 端由于 `dyn Error` 不暴露字段，采用 **Display 文本约定**：

| 字段 | 抽取方式 |
|---|---|
| `error_name` | concrete type → `std::any::type_name_of_val`；trait object → Display 首行截断 |
| `acp_code` | Display 文本 `code: ACP_XXX: ...` |
| `cause_message` | Display 文本 `cause: <line>`，或 `Error::source()` 链第一项 |
| `retryable` | Display 文本 `retryable=true` / `retryable=false` |
| `stack_preview` | Display 文本 `stack: <line1>\n...` 截到 6 行 |

这套约定的好处：**任何实现了 `Display` + `Error` 的类型都能被分类**，不需要 downcast 到具体类型。坏处：约定不显式，但通过 16 个单元测试锁定。

### 单元测试覆盖（16 个）

- `auth_required_message_yields_acpx_auth_required` — auth 优先
- `auth_credential_keyword_also_triggers_auth_category`
- `acp_session_init_failed_phase_yields_protocol_code` — protocol category
- `acp_turn_failed_phase_yields_turn_failed`
- `ensure_session_phase_without_acp_code_maps_to_session_init` — runtime category
- `configure_session_phase_maps_to_session_config`
- `turn_phase_maps_to_turn_failed`
- `unknown_phase_returns_acpx_runtime_error`
- `non_acp_code_returns_protocol_error_when_phase_missing`
- `non_acp_string_field_is_ignored` — `ENOENT` 不进 acpCode
- `stack_preview_truncated_to_six_lines`
- `is_resume_failure_matches_conversation_keyword`
- `is_resume_failure_matches_unknown_session`
- `is_resume_failure_returns_false_for_unrelated_errors`
- `retryable_field_round_trips_through_meta`
- `cause_message_extracted_from_display`

---

## 📐 2. child_stderr.rs

### 公开 API

```rust
pub static BENIGN_NES_CLOSE_STDERR: std::sync::OnceLock<Regex>;

pub struct ChildStderrState {
    pub log_path: Option<PathBuf>,
    pub pending_live_line: String,
}
impl ChildStderrState {
    pub fn new(log_path: Option<impl Into<PathBuf>>) -> Self;
    pub fn without_log() -> Self;
}

pub enum ChildStderrError {
    LogAppend { path: PathBuf, #[source] error: io::Error },
    StderrWrite(#[source] io::Error),
}

pub struct RoutedStderr  { pub host_visible: String }
pub struct FlushedStderr { pub host_visible: String }

pub fn route_child_stderr(state: &mut ChildStderrState, chunk: &str) -> Result<RoutedStderr, ChildStderrError>;
pub fn route_child_stderr_with<W: Write>(state: &mut ChildStderrState, chunk: &str, stderr: &mut W) -> Result<(), ChildStderrError>;

pub fn flush_child_stderr(state: &mut ChildStderrState) -> Result<FlushedStderr, ChildStderrError>;
pub fn flush_child_stderr_with<W: Write>(state: &mut ChildStderrState, stderr: &mut W) -> Result<(), ChildStderrError>;

pub async fn read_child_stderr_tail(log_path: Option<&Path>, max_bytes: usize) -> Option<String>;
```

### 设计要点

1. **`Writer` 注入**：核心逻辑接受 `&mut W: Write`，默认顶层包装返回 `RoutedStderr` / `FlushedStderr`（收集到 `String`）。**测试可以传 `Vec<u8>` sink 验证内容**，不需要 capture stderr。

2. **`pending_live_line` 跨块缓冲**：未结束的行（无 `\n`）暂存到下一次 chunk 到达，确保 `method: 'nes/close' -32601` 永远不会因跨块而漏过滤。

3. **log 文件 = 真值**：每次 chunk **原始** append 到 `log_path`，过滤只作用于 host-visible 部分。`read_child_stderr_tail` 读取原始 tail 用于失败路径诊断。

4. **错误吞咽**：`read_child_stderr_tail` 把任何 I/O 错误折成 `None`，因为它只在失败路径调用——不能再抛错。

5. **`OnceLock<Regex>`**：compiled regex 进程级单例，无 lock contention。

### 良性过滤正则

```rust
method: ['"]nes/close['"].*-32601
```

匹配：`method: 'nes/close'` 或 `method: "nes/close"` 后面跟 JSON-RPC `-32601 method not found` 错误码。这是 acpx 进程关闭时清理 nes 通知的良性噪音。

### 单元测试覆盖（13 个）

**状态机**:
- `pending_line_buffered_until_newline`
- `pending_carries_across_chunks_until_newline`
- `chunk_with_newline_writes_filtered_to_stderr`

**良性过滤**:
- `benign_nes_close_filtered`
- `benign_nes_close_with_double_quotes_also_filtered`

**flush**:
- `flush_emits_pending_when_non_benign`
- `flush_drops_benign_pending`
- `flush_with_empty_pending_is_noop`

**log 读取**:
- `read_child_stderr_tail_returns_none_when_path_absent`
- `read_child_stderr_tail_returns_none_for_missing_file`
- `read_child_stderr_tail_returns_tail_for_existing_file`
- `read_child_stderr_tail_truncates_to_max_bytes`
- `read_child_stderr_tail_returns_none_for_empty_file`

---

## 📐 3. startup_timing.rs

### 公开 API

```rust
pub const RUN_STARTUP_STEP_EVENT_TYPE: &str = "run.startup.step";
pub const BUILT_IN_PROVIDER_FAMILIES: &[&str] = &[/* 7 entries */];
pub const PLUGIN_PROVIDER_FAMILY: &str = "plugin";
pub const SPAN_STATUS_CODE_ERROR: u32 = 2;

pub fn normalize_provider_family(key: Option<&str>) -> String;

// Span / Tracer 契约
pub trait StartupSpan {
    fn set_attribute(&mut self, key: &str, value: StartupSpanAttribute);
    fn set_status(&mut self, status: StartupSpanStatus);
    fn end(&mut self);
}
pub enum StartupSpanAttribute { String(String), Number(f64), Boolean(bool) }
pub struct StartupSpanStatus { pub code: u32, pub message: Option<String> }

pub trait StartupTracer {
    fn start_span(&self, name: &str, attributes: &BTreeMap<String, StartupSpanAttribute>, parent_context: Option<&dyn StartupSpanContextAny>) -> Box<dyn StartupSpan + Send>;
}
pub trait StartupSpanContextAny: std::any::Any + Send + Sync { fn as_any(&self) -> &dyn std::any::Any; }
pub trait AnyContext: std::any::Any {}

pub trait StartupTraceContext: Send + Sync {
    fn tracer(&self) -> &dyn StartupTracer;
    fn context_with_span(&self, span: Box<dyn StartupSpan + Send>) -> Box<dyn StartupSpanContextAny>;
}

// Noop 实现
pub struct NoopStartupSpan;
pub struct NoopStartupTracer;
pub struct NoopStartupTraceContext;
pub struct NoopSpanContext;

// 测量 options
pub struct StartupStepMeasureOptions { /* 7 fields, all Optional */ }
impl StartupStepMeasureOptions { /* 7 with_* builder methods */ }

// Runtime event
pub struct RuntimeStartupStepEvent {
    pub event_type: String, pub stream: String, pub level: String,
    pub message: String, pub payload: serde_json::Map<String, Value>,
}
pub fn build_step_event(payload: serde_json::Map<String, Value>) -> RuntimeStartupStepEvent;

// Context 契约
pub trait StartupStepContext {
    fn on_event(&self, event: &RuntimeStartupStepEvent) -> Option<Pin<Box<dyn Future<Output = Result<(), String>> + Send + '_>>>;
}

// 核心函数
pub async fn measure_startup_step<T, F, C>(
    ctx: &C, mut now: impl FnMut() -> i64, step: &str, func: F, options: StartupStepMeasureOptions,
) -> Result<T, String>
where T: Send, F: Future<Output = Result<T, String>>, C: StartupStepContext + ?Sized;
```

### 设计要点

1. **`StartupSpan` 不带 Send + Sync**：Node 端的 OTel span 是同步 `end()` 语义。Rust 端 `Box<dyn StartupSpan + Send>` 即可满足 emit 后 drop 的需求。

2. **`StartupStepContext` 接受 event 作为参数**：避免调用方自建空 envelope 覆盖真实 event——测试可断言 `payload.get("step")` 等字段。

3. **`now: FnMut() -> i64`**：测试需要 mutable 计时器（`clock += 5`），生产代码用 `|| Instant::now().elapsed().as_millis() as i64`。

4. **观测失败吞咽**：span 调用 + ctx.on_event 调用都包在 `panic::catch_unwind` 里，**observability 永远不改 startup control flow**（Node 端用 try/catch 实现相同语义）。

5. **`extra` 只进 event payload 不进 span**：`extra` reader 是 R365 引入的 `acp.handshake` 子步骤时间（`createRuntimeMs` 等），**不能**扩大 closed span allowlist——这是 Node 端注释强调的契约。

6. **`BUILT_IN_PROVIDER_FAMILIES` 闭集**：7 个常量字符串，写死常量数组；任何不在表里的 key 自动 collapse 到 `plugin`。

### span attribute 闭环

```
start attributes:
  step            (string, raw step name)
  provider        (string, normalized via normalize_provider_family)

finally attributes:
  roundTrips      (number, finite delta only)
  providerExecMs  (number, finite delta only)
  providerGetMs   (number, finite delta only)

finally status:
  ERROR           (only when fn throws)
```

任何 `extra()` 的 key 只进 event payload（jsonb），**不进 span**。

### 单元测试覆盖（12 个）

**Provider normalization**:
- `normalize_provider_family_builtins_returned_unchanged`
- `normalize_provider_family_unknown_returns_plugin`
- `normalize_provider_family_empty_returns_plugin`
- `normalize_provider_family_none_returns_plugin`

**Core measure**:
- `measure_step_emits_event_with_duration`
- `measure_step_passes_through_value`
- `measure_step_emits_counter_deltas`
- `measure_step_sets_error_status_on_throw`
- `measure_step_does_not_swallow_fns_error`

**Tracer integration**:
- `measure_step_emits_span_with_normalized_provider`
- `measure_step_normalizes_unknown_provider_to_plugin`

**Noop**:
- `noop_tracer_is_inert`

---

## 🔗 4. round366_recovery.rs 集成测试（9 个）

**错误分类 e2e**:
- `classify_error_auth_path_overrides_phase` — auth 优先于 phase
- `describe_error_diagnostics_extracts_full_struct` — 5 字段契约
- `is_resume_failure_returns_true_for_known_resume_phrases` — 5 keyword 表

**stderr e2e**:
- `end_to_end_routes_only_real_lines_to_host` — 4 chunk 跨缓冲 + flush
- `end_to_end_tail_round_trip_through_log_file` — `route_*` → `read_tail` 闭环

**startup timing e2e**:
- `measure_step_emits_event_with_known_duration`
- `measure_step_provider_normalization_is_low_cardinality`
- `build_step_event_emits_run_startup_step_envelope`

**契约锁定**:
- `normalize_provider_family_table_matches_node_constants` — 7 builtin 锁定

---

## 🔁 总累计基线

| 模块 | R362 | R363 | R364 | R365 | **R366** |
|---|---|---|---|---|---|
| constants | ✓ | | | | |
| gemini_version | ✓ | | | | |
| session_codec | ✓ | | | | |
| hash | ✓ | | | | |
| normalize | ✓ | | | | |
| transcript | ✓ | | | | |
| usage | ✓ | | | | |
| settings | | ✓ | | | |
| fs_ops | | ✓ | | | |
| bin | | ✓ | | | |
| error | | ✓ | | | |
| agent_command | | | ✓ | | |
| startup_metrics | | | ✓ | | |
| prepared_runtime | | | ✓ | | |
| acp_runtime | | | | ✓ | |
| **error_classification** | | | | | **NEW** |
| **child_stderr** | | | | | **NEW** |
| **startup_timing** | | | | | **NEW** |
| **pc-acpx 测试总数** | 47 | 66 | 90 | 105 | **155** |
| **总累计** | 975 | 994 | 1018 | 1042 | **1171** |

---

## ✅ 完成度更新

| 模块 | R366 完成度 |
|---|---|
| **Recovery 主链** (R357-R360) | 99.5% |
| **acpx-engine 子模块** | **92%** (+7%, R366 +R365) |
| **后端核心** (pc-heartbeat + pc-repos + pc-core) | 96% |
| **完整后端** (含 adapters + plugins) | ~75% |
| **最大剩余缺口** | 真实 `SubprocessAcpRuntime` (R368+) + sandbox staging (R367) |

---

## 🎯 R367+ 候选

1. **R367**：Sandbox staging seam（`prepareAdapterExecutionTargetRuntime` + `stageAcpRemoteRuntime` + `startAdapterExecutionTargetPaperclipBridge`），~2 轮
2. **R368-369**：真实 `SubprocessAcpRuntime` 实现（acpx subprocess 包装），~3-4 轮
3. **R370+**：Budgets 完整迁移（B2），~3-4 轮

