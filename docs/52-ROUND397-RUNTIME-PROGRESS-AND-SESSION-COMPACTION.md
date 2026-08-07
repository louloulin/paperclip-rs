# R397 — Runtime Progress + Session Compaction (Node parity port)

## 目标

按 `comet-open` + `RTK` 思路,把 Node `packages/adapter-utils/src/` 中两个
核心 adapter-utils 模块完整移植到 `crates/pc-acpx/src/`:

1. `runtime-progress.ts` (170 行) → `runtime_progress.rs` (496 行)
2. `session-compaction.ts` (187 行) → `session_compaction.rs` (498 行)

这两个模块是 `execution-target.ts` 上层的关键依赖 — 进度上报用于
sandbox/SSH 同步/恢复阶段,会话压缩策略决定 adapter session 何时轮转。

## Node 函数 / 类型映射

### runtime-progress.ts (10 exports)

| Node | Rust | 说明 |
|---|---|---|
| `RuntimeProgressSink` | `RuntimeProgressSink` | `Arc<dyn Fn(&str)>` |
| `RuntimeProgressPhase` (enum) | `RuntimeProgressPhase` (enum) | `Syncing` / `Restoring` / `ImportingGitHistory` / `ExportingGitHistory` |
| `RuntimeProgressDirection` | `RuntimeProgressDirection` | `To` / `From` |
| `RuntimeProgressTarget` | `RuntimeProgressTarget` | `Sandbox` / `Ssh` |
| `RuntimeStatusPhase` (enum) | `RuntimeStatusPhase` (enum) | 6 个 phase |
| `RuntimeStatusUpdate` (interface) | `RuntimeStatusUpdate` (struct) | 含 `phase` / `message` / `currentToolName` / `lastAssistantSnippet` / `lastEventAt` |
| `RuntimeStatusSink` | `RuntimeStatusSink` | `Arc<dyn Fn(&RuntimeStatusUpdate)>` |
| `RuntimeProgressReporterOptions` (interface) | `RuntimeProgressReporterOptions` (struct) | 完整字段镜像 |
| `RuntimeProgressReporter` (interface) | `RuntimeProgressReporter` (struct) | `report` / `complete` / `fail` + accessor |
| `createRuntimeProgressReporter` | `create_runtime_progress_reporter` | 工厂函数 |
| `formatMb` (internal) | `format_mb` (pub) | 公开 (helper 复用) |
| `clampPercent` (internal) | `clamp_percent` (pub) | 公开 (helper 复用) |
| `BYTES_PER_MB` (const) | `BYTES_PER_MB` (const) | `1024.0 * 1024.0` |

### session-compaction.ts (10 exports)

| Node | Rust | 说明 |
|---|---|---|
| `SessionCompactionPolicy` (interface) | `SessionCompactionPolicy` (struct) | `enabled` / `maxSessionRuns` / `maxRawInputTokens` / `maxSessionAgeHours` |
| `NativeContextManagement` (type) | `NativeContextManagement` (enum) | `Confirmed` / `Likely` / `Unknown` / `None` |
| `AdapterSessionManagement` (interface) | `AdapterSessionManagement` (struct) | 3 字段镜像 |
| `ResolvedSessionCompactionPolicy` (interface) | `ResolvedSessionCompactionPolicy` (struct) | 4 字段镜像 |
| `LEGACY_SESSIONED_ADAPTER_TYPES` (Set) | `LEGACY_SESSIONED_ADAPTER_TYPES` (`&[&str]`) | 8 个 adapter type |
| `ADAPTER_SESSION_MANAGEMENT` (Record) | `get_adapter_session_management` (fn) | 改为 lookup 函数 (Rust 无静态 Record) |
| `getAdapterSessionManagement` | `get_adapter_session_management` | snake_case + Option 返回 |
| `readSessionCompactionOverride` | `read_session_compaction_override` | 完整 mirror (heartbeat.sessionCompaction / sessionRotation / top-level) |
| `resolveSessionCompactionPolicy` | `resolve_session_compaction_policy` | 完整 mirror (3-way source resolution) |
| `hasSessionCompactionThresholds` | `has_session_compaction_thresholds` | 完整 mirror |

## 关键设计决策

### 1. Async → Sync
Node 使用 `Promise<void>` 返回值 (因为 `sink` 可以是 async),Rust 改为:
- `RuntimeProgressSink = Arc<dyn Fn(&str) + Send + Sync>` (sync)
- `report()` / `complete()` / `fail()` 改为 `&mut self` + 无返回值

这是因为 Rust 的 `report` 是同步调用 — 调用方在同一个线程上读取结果。
adapter-utils 的所有现有调用点都是 fire-and-forget (写入日志),不依赖
异步语义。

### 2. `Date` → u64 millis
Node `now: () => number` 返回 `Date.now()` (ms since epoch),Rust 改为
`Arc<dyn Fn() -> u64 + Send + Sync>`。所有时间都以 u64 毫秒表示。

### 3. `Partial<T>` → 结构化 `PartialSessionCompactionPolicy`
Node `Partial<SessionCompactionPolicy>` 保留 `Partial` 语义,Rust 改为
显式 `Option` 字段 (`PartialSessionCompactionPolicy`),避免依赖 nightly。
提供 `is_empty()` 方法镜像 `Object.keys().length > 0`。

### 4. `Set` / `Record` → 显式枚举
- `LEGACY_SESSIONED_ADAPTER_TYPES: Set<string>` → `&[&str]` + `.contains()`
- `ADAPTER_SESSION_MANAGEMENT: Record<string, ...>` → `match` 函数

### 5. `unknown` → `&serde_json::Value`
Node 接受 `runtimeConfig: unknown`,Rust 改为 `&serde_json::Value` —
使用现有的 `serde_json` 依赖,避免泛型边界复杂化。`isRecord` 检查
镜像 Node `typeof value === "object" && value !== null && !Array.isArray(value)`。

## 单元测试

### runtime_progress: 11 tests

- `format_mb_formats_bytes_as_mb` — 边界 (0, 1MB, 2.5MB)
- `clamp_percent_clamps_and_rounds` — NaN/Infinity/正负/上下限
- `report_emits_on_first_call` — 第一次 report 必须 emit
- `report_emits_on_step_crossing` — 跨步进必须 emit
- `report_does_not_emit_within_same_step` — 同步进内不 emit
- `report_emits_on_terminal` — doneBytes == totalBytes 时 emit 100% 并标记 completed
- `complete_is_idempotent` — 多次 complete 只 emit 一次
- `fail_emits_failure_line` — fail 包含 "failed at"
- `fail_is_mutually_exclusive_with_complete` — fail after complete 不 emit
- `report_with_unknown_total_uses_elapsed_throttle` — 无 total 时只按 elapsed 节流
- `complete_uses_last_known_values` — complete(None, None) 用最后 report 的值

### session_compaction: 17 tests

- `default_policy_has_expected_values` — 200 / 2M / 72h
- `adapter_managed_policy_has_zero_thresholds` — claude/codex/hermes 用
- `get_adapter_session_management_claude` / `_gemini` / `_unknown_returns_none`
- `read_boolean_parses_various_formats` — true/false/0/1/yes/no/on/off/maybe
- `read_number_parses_various_formats` — int/float/negative/string
- `read_override_from_heartbeat_session_compaction` — 优先路径
- `read_override_from_heartbeat_session_rotation_alias` — alias 路径
- `read_override_from_top_level_session_compaction` — fallback 路径
- `read_override_empty_when_no_config` — 空 config → empty override
- `resolve_policy_adapter_default_for_claude` — Claude 用 ADAPTER_MANAGED
- `resolve_policy_adapter_default_for_gemini` — Gemini 用 DEFAULT (200/2M/72h)
- `resolve_policy_legacy_fallback_for_unknown_adapter` — 未知 adapter
- `resolve_policy_agent_override_wins_over_default` — override 优先
- `resolve_policy_legacy_fallback_enabled_for_known_legacy_types` — 8 个 legacy type 全部 enabled
- `has_thresholds_detects_positive_values` — 任一 > 0 即 true

## 集成测试 (round397_progress_and_compaction.rs): 7 tests

### runtime_progress 集成
- `progress_reporter_full_sync_lifecycle` — 25%/50%/75%/100% 全周期
- `progress_reporter_fail_marks_completed` — fail 后 reporter.is_completed() == true

### session_compaction 集成
- `compaction_claude_uses_adapter_managed_policy` — Claude 无阈值
- `compaction_gemini_uses_default_thresholds` — Gemini 有阈值
- `compaction_agent_override_takes_precedence` — override 覆盖 default
- `compaction_legacy_fallback_for_unknown_adapter` — 未知 adapter
- `compaction_all_legacy_adapters_have_enabled_policy` — 8 个 legacy type 遍历

## 验证结果

```bash
cargo test -p pc-acpx --lib -- runtime_progress
# 11 passed

cargo test -p pc-acpx --lib -- session_compaction
# 17 passed

cargo test -p pc-acpx --test round397_progress_and_compaction
# 7 passed

cargo test -p pc-acpx --lib
# 527 passed (up from 499)

cargo test -p pc-acpx --tests | grep "0 failed" | wc -l
# 32 (all integration test files pass)

cargo fmt -p pc-acpx -- --check
# Clean
```

## 与 Node parity 检查

| Node export | Rust | Unit | Integration |
|---|---|---|---|
| `RuntimeProgressSink` (type) | ✓ | — | — |
| `RuntimeProgressPhase` (enum) | ✓ | — | — |
| `RuntimeProgressDirection` (enum) | ✓ | — | — |
| `RuntimeProgressTarget` (enum) | ✓ | — | — |
| `RuntimeStatusPhase` (enum) | ✓ | — | — |
| `RuntimeStatusUpdate` (interface) | ✓ | — | — |
| `RuntimeStatusSink` (type) | ✓ | — | — |
| `RuntimeProgressReporterOptions` (interface) | ✓ | — | — |
| `RuntimeProgressReporter` (interface) | ✓ | 11 | 2 |
| `createRuntimeProgressReporter` | ✓ | (via reporter) | (via reporter) |
| `formatMb` | ✓ | 1 | — |
| `clampPercent` | ✓ | 1 | — |
| `SessionCompactionPolicy` | ✓ | — | — |
| `NativeContextManagement` | ✓ | — | — |
| `AdapterSessionManagement` | ✓ | 3 | — |
| `ResolvedSessionCompactionPolicy` | ✓ | — | — |
| `LEGACY_SESSIONED_ADAPTER_TYPES` | ✓ | — | 1 |
| `ADAPTER_SESSION_MANAGEMENT` | ✓ (via get fn) | (via get fn) | (via get fn) |
| `getAdapterSessionManagement` | ✓ | 3 | — |
| `readSessionCompactionOverride` | ✓ | 4 | — |
| `resolveSessionCompactionPolicy` | ✓ | 5 | 5 |
| `hasSessionCompactionThresholds` | ✓ | 1 | 2 |

**Runtime-progress: 100% Node parity**
**Session-compaction: 100% Node parity**

## 累计进度

| 状态 | 模块 | Node 行数 | Rust 行数 |
|---|---|---|---|
| ✅ R396 | billing | 20 | 151 |
| ✅ R396 | exclude-patterns | 28 | 145 |
| ✅ R396 | sandbox-shell | 7 | 73 |
| ✅ R396 | command-redaction | 58 | 217 |
| ✅ R396 | remote-execution-env | 49 | 169 |
| ✅ R396 | sandbox-install-command | 46 | 126 |
| ✅ R397 | runtime-progress | 170 | 496 |
| ✅ R397 | session-compaction | 187 | 498 |
| **小计** | **8 个模块** | **565** | **1875** |

**pc-acpx 累计模块数**: 54 (含本次新增 2 个)
**pc-acpx 累计 lib 测试**: 499 → 527 (+28)
**pc-acpx 累计集成测试文件**: 31 → 32 (+1)

## 下一步 (R398+ 候选)

- **R398**: `local-process-sandbox.rs` (509 行) + `workspace-restore-merge.rs` (259 行)
- **R399**: `git-workspace-sync.rs` (433 行) + `remote-managed-runtime.rs` (239 行)
- **R400**: `command-managed-runtime.rs` (570 行) + `sandbox-callback-bridge.rs` (1262 行)
- **R401**: `sandbox-managed-runtime.rs` (1224 行)
- **R402**: `execution-target.ts` (1877 行) — 最大模块
- **R403**: `ssh.ts` (1862 行)
