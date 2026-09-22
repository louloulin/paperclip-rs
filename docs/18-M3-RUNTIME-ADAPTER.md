# M3-2：运行时 adapter（`mc-runtime`）

> 落地切片：LUM-1408（M3-2）。上游对照：`server/pkg/agent/{agent,pi,claude,pi_session_lock_unix}.go`。
> 计划出处：`docs/15-M3-PLAN.md` §7.5/§7.6/§9.3、`docs/plan1.md` R4。

## 1. 本片做了什么

| 项 | 状态 |
| --- | --- |
| `RuntimeAdapter` trait（launch / stream / cancel / probe-version / capabilities） | ✅ `crates/mc-runtime/src/adapter/traits.rs` |
| `AdapterRegistry`（**替换** M0 的 `AdapterRegistryStub`） | ✅ `crates/mc-runtime/src/registry.rs`；`mc-http/src/state.rs` 只 `pub use` |
| adapter 元数据表（25 项白名单 + 启动骨架 + CLI 命令名） | ✅ `crates/mc-runtime/src/catalog.rs` |
| 一致性套件（**宏/模板**，M3-8 的 25 个 adapter 复用） | ✅ `crates/mc-runtime/src/conformance.rs` + `adapter_conformance!` |
| 打通 **1 个**真实 adapter：`pi-local` 端到端 | ✅ `crates/mc-runtime/src/adapters/pi_local/` |
| 不写库、不加路由、不动 `ConfigSnapshot` 默认值 | ✅ 见 §7 |

一次 run 的端到端路径（本片实装的部分）：

```
LaunchRequest ──► PiLocal::launch
                    ├─ resolve_executable()     缺失 ⇒ AdapterError::ExecutableUnavailable（runtime_offline）
                    ├─ ensure_session_file()    新会话建空文件；resume 路径不动
                    ├─ try_lock_session()       同一 JSONL 已在跑 ⇒ AdapterError::SessionBusy
                    ├─ Command + env + cwd ──► child
                    └─ RunHandle { events: mpsc<RuntimeEvent>, outcome: oneshot<RunOutcome> }
stdout ──► BufReader::lines ──► PiDecoder ──► RuntimeEvent（Text/Thinking/ToolUse/ToolResult/Usage/Error）
终态   ──► RunOutcome { status, failure_reason, exit_code, error, output, usage, session_id, stderr_tail }
```

## 2. 契约（实现者与调用方都必须遵守）

写进 `lib.rs` 的五条，一致性套件逐条验：

1. **launch 失败 ≠ run 失败**。`launch` 的 `Err` 只表示"没能启动"（二进制缺失、会话忙、prompt 空白）；
   进程起来之后的一切都走 `RunOutcome`（含非零退出、被 cancel、超时）。
2. **`Started` 是第一条事件**，且带 `executable` 与 `pid`（pid 缺失时 `None`，但事件本身必须发）。
3. **终态一定会到**：事件通道**先**关闭（所有事件发完），**然后** `RunOutcome` 到达。
   因此 `drain()` 能"先收全事件、再拿终态"，两个 `await` 不会互相饿死。
4. **事件通道无界**。不用有界通道 + `.await` 背压：阻塞在 `send` 上的 run 连 `cancel` 都响应不了。
   契约因此要求调用方二选一 —— `next_event()`/`drain()`（消费）或 `outcome()`（丢弃未读事件）。
   后者会 drop 接收端，`send` 立刻返回 `Err`，**生产方必须容忍 send 失败**。
5. **白名单是硬边界**。`AgentType` 只有 25 个取值，`parse()` 不拿默认值兜底
   （拼错的类型不能静默变成 `pi`）。

## 3. 一致性套件：怎么用

套件不是"给 pi 写的一套测试"，而是一份 **宏模板**：M3-8 批量接其余 24 个 adapter 时，
新 adapter 只需实现一个 hook trait，再写一行宏调用。

```rust
// crates/mc-runtime/src/adapters/<新 adapter>/mod.rs

impl TestableAdapter for MyAdapter {
    /// 用「假 CLI + 临时工作目录」构造一个实例 —— 断言全部跑在这个假 CLI 上，
    /// 所以套件不依赖机器上装了哪个真实 CLI。
    fn with_conformance_env(executable: &Path, workdir: &Path) -> Self { ... }

    /// 回放脚本：成功 transcript / 期望输出 / 期望错误串 / 期望版本号。
    fn conformance_script() -> ConformanceScript { ... }
}

crate::adapter_conformance!(MyAdapter);
```

`adapter_conformance!(A)` 展开出 8 个用例（`#[cfg(unix)]`）：

| 用例 | 断言 |
| --- | --- |
| `kind_is_in_catalog` | `kind()` 能反向 parse 回白名单（防拼写漂移） |
| `capabilities_are_self_consistent` | 自述能力与实际行为一致（如 `usage_reporting` ⇒ 终态带用量） |
| `probe_version_parses_semver` | 版本可解析；**非** semver 输出 ⇒ `Ok(version: None)`（探测不是硬失败） |
| `launch_streams_and_completes` | `Started` 首条 + executable/pid；文本拼接 == 期望输出；`Completed`/退出码 0；用量；prompt 走 stdin **且不出现在 argv** |
| `nonzero_exit_maps_to_agent_error` | 非零退出 ⇒ `Failed` + `failure_reason=agent_error`；错误串与 `stderr_tail` 带回来 |
| `timeout_maps_to_timeout` | 超时 ⇒ `Timeout` + `failure_reason=timeout`；进程真被杀（`/proc/<pid>` 消失） |
| `cancel_is_idempotent` | 取消 ⇒ `Signalled`，重复取消 ∈ {`Signalled`,`NotRunning`}；终态 `Cancelled`/`manual`；进程已回收 |
| `decoder_tolerates_junk` | stdout 混日志/坏 JSON 行不中断 run |

`FakeCli`（`conformance.rs`）现场生成 `#!/bin/sh` 脚本并记录 argv/stdin，退出时删临时目录；
没有 `tempfile` 依赖。断言都写在泛型 `check_*::<A>()` 里，宏体只负责起 8 个 `#[tokio::test]`，
因此每个 adapter 的覆盖度完全相同。

## 4. 接一个新 adapter 的 6 步

1. `crates/mc-runtime/src/catalog.rs`：确认 `AgentType` 里已有该 provider key
   （25 项已全部登记，无需新增）；核对 `launch_header()` 与上游 `launchHeaders` 逐字一致。
2. 建目录 `src/adapters/<name>/{mod.rs,args.rs,run.rs,stream.rs}`（单文件 < 800 行，gate ⑩）。
3. `impl RuntimeAdapter`：`kind` / `capabilities` / `launch` / `cancel` / `probe_version` / `decoder`。
   `launch` 只负责 spawn + 返回 `RunHandle`，**不要**在这里等 run 结束。
4. `impl EventDecoder`：把该 CLI 的 stdout 协议映射到 `RuntimeEvent`；
   未知行返回空 `Vec`（协议容错），文本增量用 `TextDrain` 消毒后再吐。
5. `impl TestableAdapter` + `crate::adapter_conformance!(<Name>)`。
6. `adapters/mod.rs` 与 `lib.rs` 的 re-export 补一行；`AdapterRegistry::with_builtin_adapters()`
   里决定是否进生产装配。

## 5. 25 项白名单（逐字取自上游 `agent.go`）

`AgentType::ALL` 的顺序 = 上游 `SupportedTypes`（L350-376）的顺序；`launch_header` 逐字复制
上游 `launchHeaders`（L502-528）。**不是** `docs/15` §9.3 纠正的那个 26 项目录。

| # | provider key | 启动骨架（上游原文） | CLI 命令 |
| --- | --- | --- | --- |
| 1 | `claude` | `claude (stream-json)` | `claude` |
| 2 | `codebuddy` | `codebuddy (stream-json)` | `codebuddy` |
| 3 | `codex` | `codex app-server` | `codex` |
| 4 | `copilot` | `copilot (json)` | `copilot` |
| 5 | `opencode` | `opencode run (json)` | `opencode` |
| 6 | `codearts` | `codearts run (json)` | `codearts` |
| 7 | `deveco` | `deveco run (json)` | `deveco` |
| 8 | `openclaw` | `openclaw agent (json)` | `openclaw` |
| 9 | `hermes` | `hermes acp` | `hermes` |
| 10 | `pi` | `pi (json mode)` | `pi` |
| 11 | `cursor` | `cursor-agent (stream-json)` | `cursor-agent` |
| 12 | `kimi` | `kimi acp` | `kimi` |
| 13 | `reasonix` | `reasonix acp` | `reasonix` |
| 14 | `dsh` | `dsh --profile multica (stdio)` | `dsh` |
| 15 | `kiro` | `kiro-cli acp` | `kiro-cli` |
| 16 | `antigravity` | `agy -p (non-interactive)` | `agy` |
| 17 | `qoder` | `qodercli --acp` | `qodercli` |
| 18 | `qoderclicn` | `qoderclicn --acp` | `qoderclicn` |
| 19 | `traecli` | `traecli acp serve` | `traecli` |
| 20 | `grok` | `grok agent stdio` | `grok` |
| 21 | `qwen` | `qwen -p (stream-json)` | `qwen` |
| 22 | `qwenpaw` | `qwenpaw acp` | `qwenpaw` |
| 23 | `mcode` | `mcode acp` | `mcode` |
| 24 | `dim` | `dim acp` | `dim` |
| 25 | `zeroclaw` | `zeroclaw acp` | `zeroclaw` |

上游对这份表的约束（`agent.go` L333-349）值得抄在这里：它"必须与
`runtime_profile.protocol_family` 的 CHECK 约束保持一致 —— 自定义 runtime profile
只能基于 Multica 官方支持的 backend"。`launchHeaders` 的注释（L496-501）说它
"刻意最小化：内部 flag、传输取值与环境变量都不进表"，所以这里也没有。

### 5.1 陷阱：25 ≠ 26，`runtime_profile` 不是 adapter 目录

- `mc_core::RuntimeProfile` 有 **26** 项（多一个 `omp`），名字是 kebab-case 的 profile 名
  （`claude-code`、`kiro-cli`），与 provider key 不是一套词汇。
- `AgentType::runtime_profile()` 因此返回 `Option`：25 项里 **22 项**能按名字匹配上，
  3 项匹配不上 —— `claude` / `kiro` / `qoder`（profile 里只有 `qodercli`）。
  消费方（M3-7 注册 runtime）必须显式处理 `None`，不要 `unwrap_or_default`。
- 这条差异由 `catalog.rs::runtime_profile_mapping_is_partial_and_documented` 锁住。

## 6. 上游对齐与两处**刻意**偏离

### 6.1 pi-local 忠实复制了什么

| 上游 | 本仓 |
| --- | --- |
| `buildPiArgs`（L909-1092） | `adapters/pi_local/args.rs`：`-p --mode json [--session <path>] [--model …] [--thinking …]` + 自定义参数过滤（`piBlockedArgs` 5 项 + 50 项取值模式） |
| prompt 走 **stdin** | 同（写入后显式 `drop(stdin)`，对齐上游 #2188/#6457） |
| `stripPiToolCallMarkup` 全家（L195-360） | `adapters/pi_local/sanitize.rs`：控制 token 两种形态、`call:/response:` 前缀回压、结构化工具标记整段剥除；**不用 regex**，按字节扫描（无新依赖） |
| 事件表（`agent_start`/`turn_start`/`message_update`/`tool_execution_*`/`turn_end`/`error`/`auto_retry_end`） | `adapters/pi_local/stream.rs` |
| 终态优先级链（L640-760） | `adapters/pi_local/run.rs::finalize`：超时 → 取消（带 turn error 则 failed）→ waitErr → 写 prompt 失败 → 退出码 0 但 turn error → completed |
| `piSessionDir` / `newPiSessionPath` | `~/.multica/pi-sessions` + `<UTC 20060102T150405.000000000>.jsonl` |
| `detectCLIVersion`（`claude.go`） | `probe_version`：`--version` + 10s 上限；非零退出但版本可解析仍算成功（salvage） |

两处必须写清楚的偏离：

1. **会话锁：`flock(LOCK_EX|LOCK_NB)` → 进程内注册表**。上游用 `unix.Flock` 保证"同一个
   JSONL 不能被两个 run 同时续写"，内核在守护进程死亡时自动释放。Rust std 没有 flock，
   `libc` 是新的直接依赖（§7.5 禁止），所以本片改成 `RunRegistry`：`HashSet<PathBuf>` +
   持有到子进程结束的 RAII guard。**语义差异**：守护进程崩溃时锁不会自动释放
   （进程内结构随进程消失，效果等价），但**跨进程**不互斥 —— M3-7 的守护进程必须自己
   保证"同一会话文件只有一个 run"，或届时补 `flock`。冲突映射为
   `AdapterError::SessionBusy`（上游 `piSessionBusyResult` 的 `ResumeRejectedTransient`
   归类属 M3-3）。
2. **`stderr` 保留尾部**：上游把 stderr 交给 `WaitDelay` 只做日志；本片额外把最后
   ≤4 KiB 放进 `RunOutcome.stderr_tail`，否则 M3-3 落库的失败原因只剩一个退出码。
   错误串的形状保持上游一致，所以一致性套件能同时断言 `error` 与 `stderr_tail`。

### 6.2 明确不做（M3-8 再说）

- `piTurnErrorGuard`（10 分钟"有 turn error 但进程不退出"宽限）：本片用
  `LaunchRequest::timeout` 兜住窗口；统一实现留给 M3-8。
- 最小版本门（上游 `version.go::MinVersions`）：`probe_version` 只报告，不拦。
- 其余 24 个 adapter、Windows 分支、配额/计费。

## 7. 与其它切片的接口

- **M3-3（`mc-task`，并行切片）**：`FailureReason` 的 5 个字符串
  （`agent_error|timeout|runtime_offline|runtime_recovery|manual`）与 `mc-task` 的表 1:1；
  `RunOutcome.session_id` 是"下次续跑"的唯一凭据；`RuntimeRecovery` **不由 adapter 产生**
  （租约/重试判定在 M3-3/M3-7），这里只是把取值占好。
- **M3-7（守护进程）**：`AdapterCapabilities` 是台账与守护进程选择的输入；
  `AdapterError::is_runtime_offline()` 决定跳过 retry。
- **§7.6 接线**：`ConfigSnapshot` 新增 `runtime: mc_config::RuntimeConfig`（**只接线，不新增
  env 变量**），默认值与 `mc_config::Config::default().runtime` 同源 ——
  `RuntimeConfig` 补了 `impl Default`，`Config::default()` 改为调用它，两处不再各写一份字面量。
  `state.rs` 的手写 `Default` 与 `config_snapshot_defaults_are_semantic` 用例保持不变。
- **`AdapterRegistryStub` 的下场**：删除，实现搬到 `mc-runtime`。12 个调用点
  （`apps/mc-server`、`mc-conformance`、8 个 `mc-http/tests/*.rs`、根 `tests/smoke.rs`、
  `routes/{auth,inbox}.rs`）只改标识符；`mc-http/src/state.rs` 用 `pub use` 保留
  `mc_http::state::AdapterRegistry` 这个导入路径，调用方不必各自新增依赖。
  `AdapterRegistry::default()` 仍是**空注册表**（不探测、不 spawn），
  生产装配用 `with_builtin_adapters()`。

## 8. 验收证据

```bash
# 单 crate
cargo test -p mc-runtime              # 70 passed（含 8 个 pi-local 一致性用例）
cargo test -p mc-runtime --test pi_local_e2e   # 7 passed（公开 API 视角的端到端）

# 全量门（含 ⑦ route-parity / ⑧ schema-drift / ⑨ conformance / ⑩ file-size）
MULTICA_TEST_DATABASE_URL=… bash scripts/gates.sh --with-db
```

`pi_local_e2e.rs` 的 7 个用例 = issue 点名的 4 个（生命周期 / 取消 / 超时 / 非零退出）
+ 会话文件互斥与释放 + 空白 prompt 拒绝 + 二进制缺失归类为 `runtime_offline`。
它们全部通过**公开 API**（注册表 → adapter → `RunHandle`），因此接口一旦被改成
M3-3 用不了的样子，这里就会红。
