# R433：Codex 输出不活动监控

## 目标
复刻 Node `packages/adapters/codex-local/src/server/` 的两个监控模块：
- `output-inactivity-monitor.ts`：子进程长时间无输出/无进程活动时触发，SIGTERM→5s→SIGKILL 终止
- `process-activity-monitor.ts`：Linux `/proc` 采样 CPU/IO 作为进程活动信号

## 改动

### `crates/pc-adapter-codex-local`（新模块 `output_inactivity_monitor.rs`）
- `resolve_codex_inactivity_timeout`：default 30m / configured / disabled(null) / non_positive 回退
- `OutputInactivityMonitor`：同步状态机（spawned_at / last_event_at / fired_at / outputChunkCount / outputBytes / parsedEventCount / processActivityCount）
- 心跳判定：stdout 行可 JSON 解析；stderr 只重置计时
- `check_timeout`：`now - last_event_at >= timeout` 触发一次
- `format_output_inactivity_monitor_error_message`：`monitor: no codex activity (output or process) for Nm Ns`
- `spawn_monitor`：tokio 后台任务（250ms tick，单调时钟）
- `sample_codex_process_activity`：Linux `/proc/<pid>/stat` + `/proc/<pid>/io`（进程组聚合）

### `crates/pc-adapter-process`（流式执行 API）
- `execute_process_capture_with_options`：逐 chunk 回调（喂 monitor）+ `kill_flag`（monitor 触发立即 kill）
- `StreamingProcessExecution`：多 `spawned_pid`（供进程活动采样）
- 保留原 `execute_process_capture` 向后兼容

### `crates/pc-adapter-codex-local`（execute 接线）
- `execute_codex_with_monitor`：monitor 触发 → kill_flag → 子进程终止 → 返回 `MonitorOutcome`
- 主流程组装 `codex_output_inactivity_monitor` 错误：
  - `exitCode: null`、`signal: SIGTERM`、`timedOut: false`
  - `errorCode: "codex_output_inactivity_monitor"`
  - `resultJson.outputInactivityMonitor = { kind, timeoutMs, elapsedMsSinceLastEvent, terminationSignal }`
- `outputInactivityTimeoutMs=null` → 禁用并输出诊断日志

## 测试
- `output_inactivity_monitor.rs`：12 项单测（fake-clock 状态机、超时解析、格式化、/proc 解析）
- `pc-adapter-process`：4 项（新增流式 chunk 回调测试）
- `pc-adapter-codex-local`：46 项全绿，新增 2 项集成测试：
  - `monitor_fires_and_kills_silent_process`：真实 `sleep 30` + 300ms 超时 → monitor 触发 kill
  - `monitor_disabled_returns_no_outcome`：None → 正常执行
- 真实 codex 0.144.4：`exit_code=0` 正常路径无回归，14 个流式事件

## 关键 bug 修复
- `tokio::time::Instant::now().elapsed()` 恒为 0（Instant 自身相对 elapsed）→ 改用 `std::time::Instant` 单调时钟

## 待办
- SIGTERM → 5s 宽限 → SIGKILL 升级：当前 kill_flag 直接 `child.kill()`（SIGKILL）；后续可在 `pc-adapter-process` 增加优雅终止序列
- `process-activity-monitor` 的 15s 轮询采样尚未接入 execute（Node 在非 remote 时启用）；当前依赖输出心跳
