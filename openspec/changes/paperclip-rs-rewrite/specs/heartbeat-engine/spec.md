## ADDED Requirements

### Requirement: 心跳引擎按计划拉起 agent run SHALL
The system SHALL satisfy the following behavior.

`pc-heartbeat` 周期扫描 `agent_runtime_state.monitor_next_check_at` 与 `agent_wakeup_requests`，对到期或被唤醒的 agent 拉起 run；并发上限由 `agents.max_concurrent_runs` 控制。

#### Scenario: 计划触发
- **WHEN** 当前时间 ≥ `monitor_next_check_at` 且 `monitor_attempt_count < max`
- **THEN** 创建 `heartbeat_run`，状态机进入 `PickRunnable`

### Requirement: 状态机 PickRunnable → Finalize SHALL
The system SHALL satisfy the following behavior.

每个 run 经历：`PickRunnable → AcquireLock → ScheduleInvocation → SpawnAdapterWorker → StreamEvents → PersistRunEvent → Finalize → NotifyLiveBus`。

#### Scenario: 状态机迁移
- **WHEN** run 完成 `Finalize`
- **THEN** 写入 `heartbeat_run_events.final` 并通过 `pc-realtime` 广播 `heartbeat.completed` 事件

### Requirement: 适配器 worker 子进程 + JSON-RPC over stdio SHALL
The system SHALL satisfy the following behavior.

`pc-heartbeat` 通过 `tokio::process::Command` 启动适配器 worker，host 与 worker 通过 stdio JSON-RPC 通信；worker 退出或崩溃时 run 标记为 `failed` 并写入 watchdog 决策。

#### Scenario: worker 崩溃
- **WHEN** worker 子进程非 0 退出
- **THEN** run 状态转为 `failed`，记录 stderr 摘要，写入 `heartbeat_run_watchdog_decisions`
